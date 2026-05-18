// DEV-002: persisted bond record after the LESC + app-OOB pair flow
// completes.
//
// The Ed25519 private key NEVER appears in this record. The pair flow
// generates the keypair inside the Android Keystore under
// [keystoreAlias]; the only material this record persists is the
// alias the unlock path uses to open the Keystore-resident handle.
// Closes the SPEC §3.2 D6 gap row `docs/known-gaps.md` DEV-002 — the
// Ed25519 seed is no longer present on disk in any form.
//
// The [bondKey] field stays plaintext because it is the symmetric MAC
// key for the unlock channel (used by `verifyChallengeFrame` to gate
// peer authenticity), not the long-term identity key. Moving the
// bond_key into Keystore is residual surface area called out in the
// DEV-002 journey doc's Closure section as a future strengthening
// candidate.
package com.sy.syauth.android.bond

/**
 * In-memory bond record consumed by the GATT host service and the
 * approve route at unlock time.
 *
 * The shape mirrors what the unlock path needs: [bondKey] for
 * `verifyChallengeFrame`, [keystoreAlias] for opening the
 * Keystore-resident Ed25519 signing key at sign time, and the
 * human-readable [hostName] / [peerId] for the approve notification +
 * the GATT response transport registry.
 */
public data class BondRecord(
    val peerId: String,
    val hostName: String,
    val bondKey: ByteArray,
    val keystoreAlias: String,
    val phonePubkey: ByteArray,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is BondRecord) return false
        return peerId == other.peerId &&
            hostName == other.hostName &&
            bondKey.contentEquals(other.bondKey) &&
            keystoreAlias == other.keystoreAlias &&
            phonePubkey.contentEquals(other.phonePubkey)
    }

    override fun hashCode(): Int {
        var result = peerId.hashCode()
        result = 31 * result + hostName.hashCode()
        result = 31 * result + bondKey.contentHashCode()
        result = 31 * result + keystoreAlias.hashCode()
        result = 31 * result + phonePubkey.contentHashCode()
        return result
    }
}
