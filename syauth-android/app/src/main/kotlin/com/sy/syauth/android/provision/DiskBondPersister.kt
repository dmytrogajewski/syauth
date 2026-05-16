// syauth — pair-flow [BondPersister] adapter that writes through to
// the on-disk bond store.
//
// The S-016 pair flow's `BondRecord` carries only `peerId`,
// `peerName`, and `bondKey` (the LESC handshake never observes the
// peer's Ed25519 signing key — that surface lands once the
// desktop-side LESC server exposes its identity key on the wire).
// The v0.1 demo bootstrap therefore prefers the desktop's provision
// file over this code path; the pair flow itself is stubbed and
// never advances past `Scanning`.
//
// Still, wiring the persister to a real on-disk write means a future
// LESC backend that DOES produce a full bond record (peer pubkey
// included) can supply it without re-plumbing the seam. For the
// current pair path the seed + pubkey fields are written as zeroed
// placeholders; the on-disk record is unusable for `signWire` until
// the LESC path lands.
package com.sy.syauth.android.provision

import android.util.Log
import com.sy.syauth.android.pair.api.BondPersister
import com.sy.syauth.android.pair.api.PersistError

/** Logcat tag used when the persister writes a stub record. */
public const val DISK_PERSISTER_LOG_TAG: String = "syauth.persister"

/**
 * Filler string written into the `created_at` field for pair-flow
 * records. The v0.1 demo bootstrap path overwrites this when it
 * comes via the desktop's provision file (which carries the real
 * RFC-3339 timestamp).
 */
internal const val DISK_PERSISTER_PLACEHOLDER_CREATED_AT: String =
    "1970-01-01T00:00:00Z"

/**
 * [BondPersister] that delegates to [BondStore.save]. Production
 * wires this with a `BondStore` rooted at `context.filesDir`.
 */
public class DiskBondPersister(private val bondStore: BondStore) : BondPersister {

    override fun persist(record: com.sy.syauth.android.pair.api.BondRecord) {
        Log.w(
            DISK_PERSISTER_LOG_TAG,
            "pair-flow persist on stub backend; seed/pubkey placeholders written",
        )
        val pkg = ProvisionPackage(
            schemaVersion = PROVISION_SCHEMA_VERSION,
            hostName = record.peerName,
            peerId = record.peerId,
            bondKey = record.bondKey,
            phoneSigningKeySeed = ByteArray(PROVISION_KEY_BYTES),
            phonePubkey = ByteArray(PROVISION_KEY_BYTES),
            createdAt = DISK_PERSISTER_PLACEHOLDER_CREATED_AT,
        )
        try {
            bondStore.save(pkg)
        } catch (e: java.io.IOException) {
            throw PersistError(message = "could not save bond record to disk: ${e.message}", cause = e)
        }
    }
}
