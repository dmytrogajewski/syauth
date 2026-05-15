// Roadmap item S-016 — Pairing ViewModel.
//
// The state machine is the canonical SPEC §4.4 pairing workflow:
//
//   Idle ──[onStartScanTapped]──▶ Scanning
//   Scanning ──[onPeerPicked, adapter supports LESC]──▶ LescNegotiating(code)
//   Scanning ──[onPeerPicked, LESC unsupported]──▶ Failed("adapter $name …")
//   LescNegotiating ──[onLescBondCompleted(bondKey)]──▶ OobConfirming(emoji)
//   LescNegotiating ──[onLescBondFailed(reason)]──▶ Failed(reason)
//   OobConfirming ──[onOobYesTapped]──▶ Bonded(name) [bondPersister.persist]
//   OobConfirming ──[onOobNoTapped]──▶ Failed("OOB code did not match …")
//                                      [bondRemover.remove(peerId)]
//   Scanning|… ──[onCancelTapped]──▶ Idle
//
// Architectural notes (mirrors prrr-android's QRScanViewModel):
//   - `StateFlow<PairingState>` is the single source of truth.
//   - Side-effect dependencies (`backend`, `oobCalculator`,
//     `bondPersister`, `bondRemover`) are injected as interfaces so the
//     unit test wires hand-rolled fakes (no mockk, per the brief).
//   - All transitions happen synchronously on the caller's thread; async
//     I/O (BT scan, LESC handshake) is the backend's job. This keeps the
//     test on `UnconfinedTestDispatcher` and asserts behavior, not timing.
//
// Why no `viewModelScope.launch { ... }`:
//   The Pairing flow is event-driven from the UI side and the radio side.
//   The UI thread emits taps; the BT stack emits bond-complete callbacks;
//   the ViewModel folds them into the state machine. There is no
//   long-running computation we need to background. The UniFFI HKDF call
//   is microseconds (S-015 already proves it on real ARMv8 hardware).
package com.sy.syauth.android.pair

import androidx.lifecycle.ViewModel
import com.sy.syauth.android.pair.api.BluetoothBondRemover
import com.sy.syauth.android.pair.api.BondPersister
import com.sy.syauth.android.pair.api.BondRecord
import com.sy.syauth.android.pair.api.LescResult
import com.sy.syauth.android.pair.api.OobCalculator
import com.sy.syauth.android.pair.api.PairBackend
import com.sy.syauth.android.pair.api.PeerHandle
import com.sy.syauth.android.pair.api.PersistError
import com.sy.syauth.android.pair.api.PickPeerResult
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Reason strings are constants so the unit test can assert on them
 * without a fragile string-equality. They are also the user-visible
 * message in the Failed branch of the Compose screen.
 */
internal object PairingReasons {
    const val ADAPTER_NO_LESC_PREFIX: String = "adapter "
    const val ADAPTER_NO_LESC_SUFFIX: String = " does not support LE Secure Connections"
    const val OOB_MISMATCH: String =
        "OOB code did not match — peer might be a relay attacker"
    const val PERSIST_PREFIX: String = "could not persist bond: "
}

class PairingViewModel(
    private val backend: PairBackend,
    private val oobCalculator: OobCalculator,
    private val bondPersister: BondPersister,
    private val bondRemover: BluetoothBondRemover,
) : ViewModel() {

    private val _state: MutableStateFlow<PairingState> =
        MutableStateFlow(PairingState.Idle)

    /** Observable state for the screen. */
    val state: StateFlow<PairingState> = _state.asStateFlow()

    /**
     * Provisional pick: the peer the user tapped in the scan list. Held
     * here so the [LescNegotiating] → [OobConfirming] / [Bonded]
     * transitions can carry the peer identity forward without leaking
     * it into the state-class payload (which would force every state
     * variant to carry it).
     */
    private var pickedPeer: PeerHandle? = null

    /**
     * Transition Idle → Scanning. Called when the user taps the
     * "Pair with computer" CTA.
     */
    fun onStartScanTapped() {
        if (_state.value !is PairingState.Idle) return
        backend.startScan()
        _state.value = PairingState.Scanning
    }

    /**
     * Cancel from [Scanning] or [LescNegotiating] back to [Idle]. Stops
     * any in-flight scan; does NOT remove a BT bond (we are not bonded
     * yet at this point).
     */
    fun onCancelTapped() {
        when (_state.value) {
            is PairingState.Scanning, is PairingState.LescNegotiating -> {
                backend.stopScan()
                pickedPeer = null
                _state.value = PairingState.Idle
            }
            else -> Unit
        }
    }

    /**
     * Transition [Scanning] → [LescNegotiating] (or [Failed] if the
     * adapter lacks LESC). Called when the user picks a peer from the
     * scan results.
     */
    fun onPeerPicked(peer: PeerHandle) {
        if (_state.value !is PairingState.Scanning) return
        pickedPeer = peer
        _state.value = when (val r = backend.pickPeer(peer)) {
            is PickPeerResult.LescStarted -> PairingState.LescNegotiating(r.code)
            is PickPeerResult.LescUnsupported -> PairingState.Failed(
                PairingReasons.ADAPTER_NO_LESC_PREFIX +
                    r.adapterName +
                    PairingReasons.ADAPTER_NO_LESC_SUFFIX,
            )
            is PickPeerResult.Failed -> PairingState.Failed(r.reason)
        }
    }

    /**
     * Drive the LESC outcome into the state machine. Called by the
     * backend (in production) or directly by the test. On success:
     * compute the OOB via UniFFI and transition to [OobConfirming]. On
     * failure: transition to [Failed] and remove the BT bond.
     */
    fun onLescResult(result: LescResult) {
        if (_state.value !is PairingState.LescNegotiating) return
        when (result) {
            is LescResult.Bonded -> {
                pickedPeer = pickedPeer?.copy(name = result.peerName)
                    ?: PeerHandle(id = result.peerName, name = result.peerName)
                val emoji = oobCalculator.compute(result.bondKey)
                // Stash the bond key for the eventual persist() call BEFORE
                // emitting the new state, so a same-thread observer who
                // immediately reacts to OobConfirming sees a consistent
                // stashedBondKey on the subsequent onOobYesTapped().
                stashedBondKey = result.bondKey
                _state.value = PairingState.OobConfirming(emoji)
            }
            is LescResult.Failed -> {
                removeBondBestEffort()
                _state.value = PairingState.Failed(result.reason)
            }
        }
    }

    /** Bond key carried from LescResult.Bonded to onOobYesTapped. */
    private var stashedBondKey: ByteArray? = null

    /**
     * User tapped Yes on the OOB-match question. Persist the bond and
     * transition to [Bonded]. If persistence throws, transition to
     * [Failed] with a PERSIST_PREFIX reason and remove the BT bond
     * (DoD #3: no residual state on any error path).
     */
    fun onOobYesTapped() {
        if (_state.value !is PairingState.OobConfirming) return
        val peer = pickedPeer ?: return
        val bondKey = stashedBondKey ?: return
        try {
            bondPersister.persist(
                BondRecord(
                    peerId = peer.id,
                    peerName = peer.name,
                    bondKey = bondKey,
                ),
            )
            _state.value = PairingState.Bonded(peer.name)
        } catch (e: PersistError) {
            removeBondBestEffort()
            _state.value = PairingState.Failed(
                PairingReasons.PERSIST_PREFIX + (e.message ?: "unknown"),
            )
        }
    }

    /**
     * User tapped No on the OOB-match question. Remove the BT bond and
     * transition to [Failed]. The [BondPersister] is NEVER called on
     * this path (DoD #3 / TC-07).
     */
    fun onOobNoTapped() {
        if (_state.value !is PairingState.OobConfirming) return
        removeBondBestEffort()
        _state.value = PairingState.Failed(PairingReasons.OOB_MISMATCH)
    }

    /**
     * Best-effort BT bond cleanup. Returns silently on any failure; the
     * BondPersister is intentionally NOT consulted here — see DoD #3 and
     * the No-path comment in [onOobNoTapped].
     */
    private fun removeBondBestEffort() {
        val peer = pickedPeer ?: return
        @Suppress("UNUSED_VARIABLE")
        val removed: Boolean = bondRemover.remove(peer.id)
        // We deliberately ignore `removed`. The journey doc and SPEC §6
        // T-004 note both call out that BT cleanup is best-effort; the
        // app-level non-persistence is what matters.
    }
}
