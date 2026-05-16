// syauth — disk-backed bond record.
//
// The phone persists the v0.1 bond material (bond_key for MAC
// verification + Ed25519 seed for signing + peer/host metadata) in a
// single TOML file under `context.filesDir`. Reuses the parser in
// [ProvisionPackage.kt] both ways: the on-disk wire format is the
// same shape as the provision file the desktop ships, so a future
// reader doesn't have to learn two layouts.
//
// Storage is an app-private file (mode 0600 by virtue of being under
// `filesDir`, which is sandboxed to the app's UID). Writes go through
// a tmpfile-then-rename so a power-loss mid-save cannot leave the
// store half-written.
//
// Threading: every call is synchronous. The file is tiny (< 1 KB) and
// the API is invoked at most twice per app lifecycle (bootstrap +
// optional rewrite via the PairingViewModel persister), so a
// background dispatcher would be ceremony with no payoff.
package com.sy.syauth.android.provision

import java.io.File
import java.io.IOException

/** File name (under `filesDir`) holding the persisted bond record. */
public const val BOND_RECORD_FILE_NAME: String = "syauth-bond.toml"

/**
 * In-memory bond record. The shape mirrors what the GATT host service
 * needs at challenge-verify time: [bondKey] for `verifyChallengeFrame`,
 * [phoneSigningKeySeed] for `signChallengeResponse`, and the human-
 * readable [hostName] / [peerId] for the approve notification + the
 * GATT response transport registry.
 */
public data class BondRecord(
    val peerId: String,
    val hostName: String,
    val bondKey: ByteArray,
    val phoneSigningKeySeed: ByteArray,
    val phonePubkey: ByteArray,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is BondRecord) return false
        return peerId == other.peerId &&
            hostName == other.hostName &&
            bondKey.contentEquals(other.bondKey) &&
            phoneSigningKeySeed.contentEquals(other.phoneSigningKeySeed) &&
            phonePubkey.contentEquals(other.phonePubkey)
    }

    override fun hashCode(): Int {
        var result = peerId.hashCode()
        result = 31 * result + hostName.hashCode()
        result = 31 * result + bondKey.contentHashCode()
        result = 31 * result + phoneSigningKeySeed.contentHashCode()
        result = 31 * result + phonePubkey.contentHashCode()
        return result
    }
}

/**
 * Disk-backed bond persistence. The constructor accepts the storage
 * directory (production: `context.filesDir`; tests: a per-test
 * `TemporaryFolder` root) so the class is fully exercisable on the
 * JVM without Robolectric.
 */
public class BondStore(private val storageDir: File) {

    /** Path the store reads/writes for this instance. */
    public val storePath: File
        get() = File(storageDir, BOND_RECORD_FILE_NAME)

    /**
     * Persist [pkg] as the canonical bond record. Atomic via
     * tmpfile + rename so a partial write never leaves a corrupt
     * store.
     */
    @Throws(IOException::class)
    public fun save(pkg: ProvisionPackage) {
        if (!storageDir.exists() && !storageDir.mkdirs()) {
            throw IOException("could not create bond storage dir: $storageDir")
        }
        val tmp = File(storageDir, "$BOND_RECORD_FILE_NAME$TMP_SUFFIX")
        tmp.writeText(serializeProvisionPackage(pkg))
        val target = storePath
        if (target.exists() && !target.delete()) {
            tmp.delete()
            throw IOException("could not replace existing bond record: $target")
        }
        if (!tmp.renameTo(target)) {
            tmp.delete()
            throw IOException("could not rename bond tmpfile into place: $target")
        }
    }

    /**
     * Load the persisted record. Returns `null` if the file does not
     * exist (first-launch state). Throws [ProvisionParseError] on a
     * corrupt file; the caller should surface this and refuse to
     * boot rather than silently dropping a malformed bond.
     */
    @Throws(ProvisionParseError::class)
    public fun load(): BondRecord? {
        val target = storePath
        if (!target.exists()) return null
        val pkg = parseProvisionPackage(target.readText())
        return BondRecord(
            peerId = pkg.peerId,
            hostName = pkg.hostName,
            bondKey = pkg.bondKey,
            phoneSigningKeySeed = pkg.phoneSigningKeySeed,
            phonePubkey = pkg.phonePubkey,
        )
    }
}

private const val TMP_SUFFIX: String = ".tmp"
