// Roadmap item S-016 — radio-side abstraction for the pairing flow.
//
// The PairBackend is the test seam between the [PairingViewModel] and the
// platform Bluetooth stack. Production wiring (lands fully in S-018 with
// the CompanionDeviceService bridge) injects an Android-BT-backed impl;
// the Robolectric unit tests inject an in-process fake.
//
// This file declares ONLY the contract. No platform imports — keeps the
// JVM unit test pure and avoids dragging android.bluetooth.* into the
// shared classpath where Robolectric would have to stub it out.
package com.sy.syauth.android.pair.api

/**
 * Identity record for a peer surfaced by [PairBackend.startScan]. The
 * fields are the minimum the screen needs to render a list row and the
 * minimum the ViewModel needs to drive the pick → LESC → bond pipeline.
 *
 * - [id] is opaque; production may carry a MAC string, tests may carry
 *   anything stable.
 * - [name] is the user-visible label (e.g. "alex-desktop").
 */
data class PeerHandle(val id: String, val name: String)

/**
 * Result of `PairBackend.pickPeer`: either the LESC dialog started with a
 * 6-digit comparison code, or the adapter refused to bring up LESC.
 *
 * The ViewModel maps these to `LescNegotiating(code)` or
 * `Failed("adapter $name does not support LE Secure Connections")`.
 */
sealed class PickPeerResult {
    data class LescStarted(val code: String) : PickPeerResult()
    data class LescUnsupported(val adapterName: String) : PickPeerResult()
    data class Failed(val reason: String) : PickPeerResult()
}

/**
 * Result of the LESC handshake completing (or failing): on success, the
 * ViewModel feeds [bondKey] into [OobCalculator] to compute the app-level
 * OOB code; on failure, transitions to `Failed(reason)` and removes the
 * BT bond.
 */
sealed class LescResult {
    /**
     * BT LESC succeeded. [bondKey] is the negotiated bond key bytes; the
     * ViewModel passes it to `oobCalculator.compute(bondKey)`. [peerName]
     * is the peer's friendly name for the eventual Bonded(name) state.
     */
    data class Bonded(val bondKey: ByteArray, val peerName: String) : LescResult() {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Bonded) return false
            return bondKey.contentEquals(other.bondKey) && peerName == other.peerName
        }

        override fun hashCode(): Int =
            31 * bondKey.contentHashCode() + peerName.hashCode()
    }

    data class Failed(val reason: String) : LescResult()
}

/**
 * The platform-Bluetooth seam for the pairing flow. Production impl lands
 * in S-018; tests inject a fake.
 *
 * Methods are synchronous from the ViewModel's perspective — async work
 * is the implementation's concern. The ViewModel calls them on its
 * `viewModelScope` so the test can run on `UnconfinedTestDispatcher`.
 */
interface PairBackend {
    /**
     * Start BLE scanning. Returns once the scan is in flight; the screen
     * may render results progressively via a flow the implementation
     * exposes elsewhere (S-018 problem).
     *
     * For S-016 the unit-test contract is: this method never throws on a
     * valid permission state. Permission failures map to a `Failed`
     * return from [pickPeer] (or an early-exit from the ViewModel).
     */
    fun startScan()

    /** Stop scanning (called on cancel-from-scanning). */
    fun stopScan()

    /**
     * Pick a peer and initiate LE Secure Connections bonding. Returns the
     * 6-digit code on success, an [PickPeerResult.LescUnsupported] sentinel
     * if the adapter can't do LESC, or a generic [PickPeerResult.Failed]
     * for other errors.
     */
    fun pickPeer(peer: PeerHandle): PickPeerResult

    /**
     * Wait for the LESC handshake to complete. The implementation
     * observes the system bond state; tests resolve this synchronously.
     */
    fun awaitLescResult(): LescResult
}
