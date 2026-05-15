// Roadmap item S-016 — Pairing ViewModel unit tests (Robolectric).
//
// Robolectric is required because PairingViewModel extends
// `androidx.lifecycle.ViewModel`, which in some module configurations
// reaches into the Android framework for `MainThreadHelper`-like
// utilities. `@Config(sdk = [34])` pins the framework version to API 34
// (the compileSdk in `app/build.gradle.kts`).
//
// The tests use hand-rolled fakes — no mockk / mockito dependency. Per
// AGENTS.md, "Mock at the BT-trait boundary, never above it"; the fakes
// here are *deterministic* stand-ins for the platform-Bluetooth seam.
//
// Test-name convention: `<state>_<event>_<outcome>`. Each test asserts
// exactly one transition or one negative invariant.
package com.sy.syauth.android.pair

import com.sy.syauth.android.pair.api.BluetoothBondRemover
import com.sy.syauth.android.pair.api.BondPersister
import com.sy.syauth.android.pair.api.BondRecord
import com.sy.syauth.android.pair.api.LescResult
import com.sy.syauth.android.pair.api.OobCalculator
import com.sy.syauth.android.pair.api.PairBackend
import com.sy.syauth.android.pair.api.PeerHandle
import com.sy.syauth.android.pair.api.PersistError
import com.sy.syauth.android.pair.api.PickPeerResult
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Hand-rolled [PairBackend] fake. Configurable per-test; records call
 * counts so the assertions in the No-path tests can prove side-effects
 * happened (or did not).
 */
private class FakePairBackend(
    var pickResult: PickPeerResult = PickPeerResult.LescStarted(code = "000000"),
    var lescResult: LescResult = LescResult.Bonded(
        bondKey = ByteArray(BOND_KEY_LEN) { it.toByte() },
        peerName = "test-peer",
    ),
) : PairBackend {
    var startScanCount: Int = 0
        private set
    var stopScanCount: Int = 0
        private set
    var lastPickedPeer: PeerHandle? = null
        private set

    override fun startScan() {
        startScanCount += 1
    }

    override fun stopScan() {
        stopScanCount += 1
    }

    override fun pickPeer(peer: PeerHandle): PickPeerResult {
        lastPickedPeer = peer
        return pickResult
    }

    override fun awaitLescResult(): LescResult = lescResult
}

private const val BOND_KEY_LEN: Int = 32

/** Records every input to the calculator and returns the configured words. */
private class FakeOobCalculator(
    private val words: List<String> = listOf("alpha", "beta", "gamma", "delta"),
) : OobCalculator {
    val invocations: MutableList<ByteArray> = mutableListOf()
    override fun compute(bondKey: ByteArray): List<String> {
        invocations.add(bondKey.copyOf())
        return words
    }
}

/** Records every persist() call; can be configured to throw. */
private class FakeBondPersister(
    private val throwError: PersistError? = null,
) : BondPersister {
    val persisted: MutableList<BondRecord> = mutableListOf()
    override fun persist(record: BondRecord) {
        if (throwError != null) throw throwError
        persisted.add(record)
    }
}

/** Records every remove() call by peer id. */
private class FakeBondRemover(
    private val returnValue: Boolean = true,
) : BluetoothBondRemover {
    val removed: MutableList<String> = mutableListOf()
    override fun remove(peerId: String): Boolean {
        removed.add(peerId)
        return returnValue
    }
}

private fun newViewModel(
    backend: FakePairBackend = FakePairBackend(),
    oobCalculator: FakeOobCalculator = FakeOobCalculator(),
    bondPersister: FakeBondPersister = FakeBondPersister(),
    bondRemover: FakeBondRemover = FakeBondRemover(),
): Quad {
    val vm = PairingViewModel(
        backend = backend,
        oobCalculator = oobCalculator,
        bondPersister = bondPersister,
        bondRemover = bondRemover,
    )
    return Quad(vm, backend, oobCalculator, bondPersister, bondRemover)
}

/** Multi-return helper. */
private class Quad(
    val vm: PairingViewModel,
    val backend: FakePairBackend,
    val oobCalculator: FakeOobCalculator,
    val bondPersister: FakeBondPersister,
    val bondRemover: FakeBondRemover,
)

private val TEST_PEER: PeerHandle = PeerHandle(id = "AA:BB:CC:DD:EE:FF", name = "alex-desktop")

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class PairingViewModelTest {

    // ──── TC-01 ────
    @Test
    fun idle_then_start_scan_transitions_to_scanning() {
        val q = newViewModel()

        q.vm.onStartScanTapped()

        assertEquals(PairingState.Scanning, q.vm.state.value)
        assertEquals(1, q.backend.startScanCount)
    }

    // ──── TC-02 ────
    @Test
    fun scanning_then_lesc_unsupported_emits_failed_with_adapter_name() {
        val q = newViewModel(
            backend = FakePairBackend(
                pickResult = PickPeerResult.LescUnsupported(adapterName = "FakeAdapter-4.0"),
            ),
        )

        q.vm.onStartScanTapped()
        q.vm.onPeerPicked(TEST_PEER)

        val state = q.vm.state.value
        assertTrue("expected Failed, got $state", state is PairingState.Failed)
        val reason = (state as PairingState.Failed).reason
        assertTrue("reason should mention adapter name, got: $reason",
            reason.contains("FakeAdapter-4.0"))
        assertTrue("reason should mention LESC, got: $reason",
            reason.contains("LE Secure Connections"))
    }

    // ──── TC-03 ────
    @Test
    fun scanning_then_peer_picked_transitions_to_lesc_negotiating_with_code() {
        val q = newViewModel(
            backend = FakePairBackend(
                pickResult = PickPeerResult.LescStarted(code = "123456"),
            ),
        )

        q.vm.onStartScanTapped()
        q.vm.onPeerPicked(TEST_PEER)

        val state = q.vm.state.value
        assertTrue("expected LescNegotiating, got $state",
            state is PairingState.LescNegotiating)
        assertEquals("123456", (state as PairingState.LescNegotiating).code)
        assertEquals(TEST_PEER, q.backend.lastPickedPeer)
    }

    // ──── TC-04 ────
    @Test
    fun lesc_then_oob_computed_transitions_to_oob_confirming() {
        val expectedWords = listOf("alpha", "beta", "gamma", "delta")
        val bondKey = ByteArray(BOND_KEY_LEN) { (it + 1).toByte() }
        val q = newViewModel(
            backend = FakePairBackend(
                pickResult = PickPeerResult.LescStarted(code = "123456"),
            ),
            oobCalculator = FakeOobCalculator(words = expectedWords),
        )

        q.vm.onStartScanTapped()
        q.vm.onPeerPicked(TEST_PEER)
        q.vm.onLescResult(LescResult.Bonded(bondKey = bondKey, peerName = "alex-desktop"))

        val state = q.vm.state.value
        assertTrue("expected OobConfirming, got $state", state is PairingState.OobConfirming)
        assertEquals(expectedWords, (state as PairingState.OobConfirming).emoji)
        assertEquals(1, q.oobCalculator.invocations.size)
        assertTrue("calculator must see exact bondKey",
            q.oobCalculator.invocations[0].contentEquals(bondKey))
    }

    // ──── TC-05 ────
    @Test
    fun oob_yes_writes_bond_and_transitions_to_bonded() {
        val q = newViewModel(
            backend = FakePairBackend(
                pickResult = PickPeerResult.LescStarted(code = "123456"),
            ),
        )

        q.vm.onStartScanTapped()
        q.vm.onPeerPicked(TEST_PEER)
        q.vm.onLescResult(
            LescResult.Bonded(
                bondKey = ByteArray(BOND_KEY_LEN) { it.toByte() },
                peerName = "alex-desktop",
            ),
        )
        q.vm.onOobYesTapped()

        val state = q.vm.state.value
        assertTrue("expected Bonded, got $state", state is PairingState.Bonded)
        assertEquals("alex-desktop", (state as PairingState.Bonded).name)
        assertEquals(1, q.bondPersister.persisted.size)
        val record = q.bondPersister.persisted[0]
        assertEquals(TEST_PEER.id, record.peerId)
        assertEquals("alex-desktop", record.peerName)
        assertTrue("bondKey must round-trip into the record",
            record.bondKey.contentEquals(ByteArray(BOND_KEY_LEN) { it.toByte() }))
    }

    // ──── TC-06 ────
    @Test
    fun oob_no_calls_remover_and_transitions_to_failed() {
        val q = newViewModel(
            backend = FakePairBackend(
                pickResult = PickPeerResult.LescStarted(code = "123456"),
            ),
        )

        q.vm.onStartScanTapped()
        q.vm.onPeerPicked(TEST_PEER)
        q.vm.onLescResult(
            LescResult.Bonded(
                bondKey = ByteArray(BOND_KEY_LEN) { it.toByte() },
                peerName = "alex-desktop",
            ),
        )
        q.vm.onOobNoTapped()

        val state = q.vm.state.value
        assertTrue("expected Failed, got $state", state is PairingState.Failed)
        val reason = (state as PairingState.Failed).reason
        assertTrue("reason should mention OOB mismatch, got: $reason",
            reason.contains("OOB code did not match"))
        assertEquals(listOf(TEST_PEER.id), q.bondRemover.removed)
    }

    // ──── TC-07 ────
    @Test
    fun failed_state_does_not_persist_bond() {
        val q = newViewModel(
            backend = FakePairBackend(
                pickResult = PickPeerResult.LescStarted(code = "123456"),
            ),
        )

        q.vm.onStartScanTapped()
        q.vm.onPeerPicked(TEST_PEER)
        q.vm.onLescResult(
            LescResult.Bonded(
                bondKey = ByteArray(BOND_KEY_LEN) { it.toByte() },
                peerName = "alex-desktop",
            ),
        )
        q.vm.onOobNoTapped()

        assertTrue(q.vm.state.value is PairingState.Failed)
        // The Critical Invariant: BondPersister was NEVER called on the
        // No path. This is the SPEC §6 T-004 mitigation in code form.
        assertEquals(0, q.bondPersister.persisted.size)
    }

    // ──── Additional invariants ────

    @Test
    fun lesc_failure_emits_failed_and_removes_bt_bond() {
        val q = newViewModel(
            backend = FakePairBackend(
                pickResult = PickPeerResult.LescStarted(code = "123456"),
                lescResult = LescResult.Failed("LESC handshake failed"),
            ),
        )

        q.vm.onStartScanTapped()
        q.vm.onPeerPicked(TEST_PEER)
        q.vm.onLescResult(LescResult.Failed("LESC handshake failed"))

        val state = q.vm.state.value
        assertTrue("expected Failed, got $state", state is PairingState.Failed)
        assertEquals("LESC handshake failed", (state as PairingState.Failed).reason)
        assertEquals(listOf(TEST_PEER.id), q.bondRemover.removed)
        assertEquals(0, q.bondPersister.persisted.size)
    }

    @Test
    fun persist_failure_falls_through_to_failed_and_removes_bt_bond() {
        val q = newViewModel(
            backend = FakePairBackend(
                pickResult = PickPeerResult.LescStarted(code = "123456"),
            ),
            bondPersister = FakeBondPersister(throwError = PersistError("disk full")),
        )

        q.vm.onStartScanTapped()
        q.vm.onPeerPicked(TEST_PEER)
        q.vm.onLescResult(
            LescResult.Bonded(
                bondKey = ByteArray(BOND_KEY_LEN) { it.toByte() },
                peerName = "alex-desktop",
            ),
        )
        q.vm.onOobYesTapped()

        val state = q.vm.state.value
        assertTrue("expected Failed, got $state", state is PairingState.Failed)
        val reason = (state as PairingState.Failed).reason
        assertTrue("reason should mention persist prefix, got: $reason",
            reason.startsWith("could not persist bond:"))
        assertEquals(listOf(TEST_PEER.id), q.bondRemover.removed)
    }

    @Test
    fun cancel_from_scanning_returns_to_idle_and_stops_scan() {
        val q = newViewModel()

        q.vm.onStartScanTapped()
        assertEquals(PairingState.Scanning, q.vm.state.value)
        q.vm.onCancelTapped()

        assertEquals(PairingState.Idle, q.vm.state.value)
        assertEquals(1, q.backend.stopScanCount)
    }

    @Test
    fun events_in_unexpected_state_are_no_ops() {
        // Driving an OobYes from Idle must not transition; must not
        // call persister. Defense against accidental UI taps during
        // a state we don't expect.
        val q = newViewModel()
        q.vm.onOobYesTapped()
        q.vm.onOobNoTapped()
        q.vm.onLescResult(
            LescResult.Bonded(
                bondKey = ByteArray(BOND_KEY_LEN),
                peerName = "alex-desktop",
            ),
        )

        assertEquals(PairingState.Idle, q.vm.state.value)
        assertEquals(0, q.bondPersister.persisted.size)
        assertEquals(0, q.bondRemover.removed.size)
    }

    @Test
    fun start_scan_is_idempotent_within_scanning() {
        val q = newViewModel()
        q.vm.onStartScanTapped()
        q.vm.onStartScanTapped()
        // The second tap must not re-enter Scanning (no double-startScan).
        assertEquals(1, q.backend.startScanCount)
        assertEquals(PairingState.Scanning, q.vm.state.value)
    }

    @Test
    fun viewmodel_initial_state_is_idle() {
        val q = newViewModel()
        assertNotNull(q.vm.state)
        assertEquals(PairingState.Idle, q.vm.state.value)
    }
}
