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
// On `viewModelScope.launch { ... }`:
//   For S-016 we did not need this; every transition was synchronous.
//   S-018 added a single suspend hop on the OOB-yes happy path —
//   `companionAssociator.associate(peer)` shows an OS dialog and
//   resolves on a callback — so the Yes path now ends with a
//   `viewModelScope.launch(associateDispatcher) { ... }`. Tests pass
//   `Dispatchers.Unconfined` so the assertions in
//   PairingViewModelTest stay synchronous; production uses
//   `Dispatchers.Main.immediate`.
package com.sy.syauth.android.pair

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.sy.syauth.android.pair.api.BluetoothBondRemover
import com.sy.syauth.android.pair.api.BondPersister
import com.sy.syauth.android.pair.api.BondRecord
import com.sy.syauth.android.pair.api.CompanionAssociationError
import com.sy.syauth.android.pair.api.CompanionAssociator
import com.sy.syauth.android.pair.api.LescResult
import com.sy.syauth.android.pair.api.OobCalculator
import com.sy.syauth.android.pair.api.PairBackend
import com.sy.syauth.android.pair.api.PeerHandle
import com.sy.syauth.android.pair.api.PersistError
import com.sy.syauth.android.pair.api.PickPeerResult
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

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

    /**
     * Prefix for the Failed reason emitted when [CompanionAssociator]
     * returns a failure (S-018). The full string is
     * `companion-device association rejected: <reason>`.
     */
    const val ASSOCIATE_PREFIX: String = "companion-device association rejected: "
}

/**
 * No-op fallback [CompanionAssociator]. Used by callers that have not
 * been migrated to S-018 yet (the S-016-era test wiring); production
 * code never installs this — it injects [com.sy.syauth.android.pair.impl.RealCompanionAssociator].
 *
 * Returns a synthetic [com.sy.syauth.android.pair.api.AssociationHandle]
 * so the happy path still ends in [PairingState.Bonded] for tests that
 * don't exercise the associator seam directly.
 */
internal class NoopCompanionAssociator : CompanionAssociator {
    override suspend fun associate(
        peer: com.sy.syauth.android.pair.api.PeerHandle,
    ): Result<com.sy.syauth.android.pair.api.AssociationHandle> =
        Result.success(
            com.sy.syauth.android.pair.api.AssociationHandle(
                associationId = NOOP_ASSOCIATION_ID,
                peerId = peer.id,
            ),
        )

    private companion object {
        const val NOOP_ASSOCIATION_ID: Long = -1L
    }
}

class PairingViewModel(
    private val backend: PairBackend,
    private val oobCalculator: OobCalculator,
    private val bondPersister: BondPersister,
    private val bondRemover: BluetoothBondRemover,
    private val companionAssociator: CompanionAssociator = NoopCompanionAssociator(),
    private val associateDispatcher: CoroutineDispatcher = Dispatchers.Main.immediate,
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
                // Stash the bond key + Keystore fields for the eventual
                // persist() call BEFORE emitting the new state, so a
                // same-thread observer who immediately reacts to
                // OobConfirming sees consistent stash on the subsequent
                // onOobYesTapped().
                stashedBondKey = result.bondKey
                stashedKeystoreAlias = result.keystoreAlias
                stashedPhonePubkey = result.phonePubkey
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

    /** Keystore alias carried from LescResult.Bonded to onOobYesTapped (DEV-002). */
    private var stashedKeystoreAlias: String = ""

    /** Phone Ed25519 pubkey carried from LescResult.Bonded to onOobYesTapped (DEV-002). */
    private var stashedPhonePubkey: ByteArray = ByteArray(0)

    /**
     * User tapped Yes on the OOB-match question. Persist the bond,
     * request a CDM association (S-018), and transition to [Bonded].
     *
     * Failure modes:
     * - Persist throws -> [Failed] with PERSIST_PREFIX reason + BT
     *   bond removed.
     * - Associate returns a failed [Result] -> [Failed] with
     *   ASSOCIATE_PREFIX reason + BT bond removed AND the just-persisted
     *   bond record is NOT rolled back at the Kotlin layer because the
     *   `BondPersister` interface intentionally has no `remove(peerId)`
     *   method in v0.1 (the `bonds.toml` rollback path is documented as
     *   "lost = pair again" in SPEC §4.4). We document the residual:
     *   the BT bond is removed (so the OS won't reuse the LESC bond)
     *   and the next pair attempt overwrites the persister entry.
     *
     * The associate call is suspend, so we launch on [viewModelScope].
     * Production wires `Dispatchers.Main.immediate` so the state
     * updates happen on the UI thread; tests inject
     * `Dispatchers.Unconfined` to run synchronously.
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
                    keystoreAlias = stashedKeystoreAlias,
                    phonePubkey = stashedPhonePubkey,
                ),
            )
        } catch (e: PersistError) {
            removeBondBestEffort()
            _state.value = PairingState.Failed(
                PairingReasons.PERSIST_PREFIX + (e.message ?: "unknown"),
            )
            return
        }
        viewModelScope.launch(associateDispatcher) {
            requestAssociationAndTransition(peer)
        }
    }

    private suspend fun requestAssociationAndTransition(peer: PeerHandle) {
        val result: Result<com.sy.syauth.android.pair.api.AssociationHandle> = try {
            companionAssociator.associate(peer)
        } catch (e: CompanionAssociationError) {
            Result.failure(e)
        }
        result.fold(
            onSuccess = {
                _state.value = PairingState.Bonded(peer.name)
            },
            onFailure = { throwable ->
                removeBondBestEffort()
                val reason = throwable.message ?: throwable::class.java.simpleName
                _state.value = PairingState.Failed(
                    PairingReasons.ASSOCIATE_PREFIX + reason,
                )
            },
        )
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
