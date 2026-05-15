// Roadmap item S-016 — bond-persistence seam.
//
// On `OobConfirming → Bonded` (the user tapped Yes), the ViewModel calls
// [BondPersister.persist] before emitting `Bonded(name)`. On any failure
// (Yes path that throws, or No path), the persister is NEVER called — DoD
// #3 verifies this with a fake whose call count is asserted in
// `failed_state_does_not_persist_bond`.
//
// The production wiring (S-018) writes via the UniFFI-exposed bond
// keystore (a future addition; S-014 already exposes `oob_code_for_bond`
// and friends, but not yet a "save bond" function). For S-016 the seam
// is enough — tests verify behavior independently of the future Rust
// surface.
package com.sy.syauth.android.pair.api

/**
 * Snapshot of a successful pairing, ready to be written to storage.
 *
 * The [bondKey] is the negotiated bond key; [peerId] / [peerName] mirror
 * the [PeerHandle] the user picked.
 */
data class BondRecord(
    val peerId: String,
    val peerName: String,
    val bondKey: ByteArray,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is BondRecord) return false
        return peerId == other.peerId &&
            peerName == other.peerName &&
            bondKey.contentEquals(other.bondKey)
    }

    override fun hashCode(): Int {
        var result = peerId.hashCode()
        result = 31 * result + peerName.hashCode()
        result = 31 * result + bondKey.contentHashCode()
        return result
    }
}

/** Persists a bond record on the Kotlin side. */
fun interface BondPersister {
    /**
     * Persist [record]. Throws [PersistError] on any failure; the
     * ViewModel maps a throw to `Failed("could not persist bond: …")`
     * and triggers BT bond removal.
     */
    fun persist(record: BondRecord)
}

/** Typed error surfaced by [BondPersister] impls. */
class PersistError(message: String, cause: Throwable? = null) : Exception(message, cause)
