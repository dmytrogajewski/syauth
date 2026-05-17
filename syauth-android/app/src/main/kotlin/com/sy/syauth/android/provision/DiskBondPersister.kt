// syauth — pair-flow [BondPersister] adapter that writes through to
// the on-disk bond store.
//
// GAP: DEV-001 — the S-016 pair flow currently uses `StubPairBackend`
// in `MainActivity.kt`, so this persister is reached only via the
// provision-file bootstrap path. The bond_key is written but the
// `phoneSigningKeySeed` / `phonePubkey` fields are zeroed
// placeholders here — they only carry real bytes when the bootstrap
// path consumes a `ProvisionPackage` and calls `BondStore.save`
// directly. Closure: when LESC pairing lands and the BondRecord
// carries the full bond material (bond_key + signing-key seed +
// pubkey), this persister writes it directly. See
// `docs/known-gaps.md` row DEV-001.
package com.sy.syauth.android.provision

import android.util.Log
import com.sy.syauth.android.pair.api.BondPersister
import com.sy.syauth.android.pair.api.PersistError

/** Logcat tag used when the persister writes a stub record. */
public const val DISK_PERSISTER_LOG_TAG: String = "syauth.persister"

/**
 * Filler string written into the `created_at` field for pair-flow
 * records. The provision-file bootstrap overwrites this with a real
 * RFC-3339 timestamp on its own write path; the placeholder is only
 * observable if a stub backend (today's `StubPairBackend`) calls
 * `persist` directly.
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
