// DEV-002: pair-flow [BondPersister] adapter that writes through to
// the on-disk bond store after a successful LESC + app-OOB pair.
//
// The [com.sy.syauth.android.pair.api.BondRecord] the ViewModel hands
// us at `OobConfirming -> Bonded` carries (peer_id, peer_name,
// bond_key). The Keystore alias + phone pubkey fields are owned by
// the LESC pair backend (it generates the Ed25519 keypair inside the
// Keystore via [KeystoreKeyGenerator] and registers the pubkey with
// the desktop's pair service) and are installed via [persistFull]
// from the production wiring.
//
// The Ed25519 private key NEVER appears in this persister. The bond
// record on disk carries only the alias the unlock path uses to open
// the Keystore-resident handle; the SPEC §3.2 D6 closure is the
// reason this seam exists.
package com.sy.syauth.android.bond

import android.util.Log
import com.sy.syauth.android.pair.api.BondPersister
import com.sy.syauth.android.pair.api.PersistError

/** Logcat tag used by the persister. */
public const val DISK_PERSISTER_LOG_TAG: String = "syauth.persister"

/**
 * Surfaced as the [BondRecord.keystoreAlias] when the ViewModel's
 * abbreviated [com.sy.syauth.android.pair.api.BondRecord] reaches the
 * persister before the LESC backend has handed in a real alias. The
 * production wiring then overwrites the record with the real alias
 * via [persistFull].
 */
internal const val PLACEHOLDER_ALIAS: String = ""

/**
 * Zero-filled placeholder for the phone-pubkey field on the
 * [BondPersister] surface — the ViewModel's
 * [com.sy.syauth.android.pair.api.BondRecord] only carries the
 * bond_key. The full record (with the Keystore alias and phone pubkey)
 * is written via [persistFull] by the production wiring once those
 * values are produced by the real LESC exchange.
 */
internal val PLACEHOLDER_PUBKEY: ByteArray = ByteArray(BOND_KEY_BYTES_LEN)

/**
 * [BondPersister] that delegates to [BondStore.save]. Production
 * wires this with a `BondStore` rooted at `context.filesDir`.
 */
public class DiskBondPersister(private val bondStore: BondStore) : BondPersister {

    override fun persist(record: com.sy.syauth.android.pair.api.BondRecord) {
        // The api-side [BondRecord] now carries `keystoreAlias` +
        // `phonePubkey` (the DEV-002 closure fields the real LESC pair
        // flow populates from the Keystore-minted Ed25519 keypair).
        // Empty alias / empty pubkey ByteArray means the caller is
        // running through a code path that has not yet been migrated
        // (older test fixture) — fall back to the historical
        // placeholders so the BondStore schema constraints still hold,
        // but log a warning so the production wiring's missing-alias
        // case surfaces in logcat instead of silently shipping zeros.
        val alias = record.keystoreAlias.ifEmpty {
            Log.w(DISK_PERSISTER_LOG_TAG, "persist called without keystoreAlias; using placeholder")
            PLACEHOLDER_ALIAS
        }
        val pubkey = if (record.phonePubkey.size == BOND_KEY_BYTES_LEN) {
            record.phonePubkey
        } else {
            Log.w(
                DISK_PERSISTER_LOG_TAG,
                "persist called without phonePubkey (size=${record.phonePubkey.size}); using placeholder",
            )
            PLACEHOLDER_PUBKEY
        }
        val full = BondRecord(
            peerId = record.peerId,
            hostName = record.peerName,
            bondKey = record.bondKey,
            keystoreAlias = alias,
            phonePubkey = pubkey,
        )
        persistFull(full)
    }

    /**
     * Persist a fully-populated [BondRecord]. Used by the production
     * pair-flow wiring once the LESC pubkey exchange has yielded all
     * three fields. Throws [PersistError] on any underlying I/O
     * failure; the ViewModel maps a throw onto `Failed(reason)` and
     * triggers BT bond removal.
     */
    public fun persistFull(record: BondRecord) {
        Log.i(DISK_PERSISTER_LOG_TAG, "persisting LESC bond for peer=${record.peerId}")
        try {
            bondStore.save(record)
        } catch (e: java.io.IOException) {
            throw PersistError(message = "could not save bond record to disk: ${e.message}", cause = e)
        }
    }
}
