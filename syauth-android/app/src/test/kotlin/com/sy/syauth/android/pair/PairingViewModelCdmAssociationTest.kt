// Roadmap item S-018 — PairingViewModel association tests.
//
// Asserts the three new behaviors the S-018 brief adds on top of S-016:
//
//   - Yes-path happy path: associator is called exactly once with the
//     correct peer, state ends in Bonded.
//   - Yes-path with associator failure: state ends in Failed with the
//     ASSOCIATE_PREFIX reason, bond is rolled back via the remover,
//     and the persister was called exactly once (we don't roll back
//     the persister entry in v0.1 — see PairingViewModel.kt rationale).
//   - No-path: associator is NEVER called.
//
// These tests live alongside the S-016 PairingViewModelTest.kt and
// share the package-private fakes via duplication — Robolectric runs
// each test class in isolation; sharing fakes across files would
// require a `helpers/` test-support source set that the project does
// not have today.
package com.sy.syauth.android.pair

import com.sy.syauth.android.pair.api.AssociationHandle
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
import kotlinx.coroutines.Dispatchers
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

private const val BOND_KEY_LEN: Int = 32
private val TEST_PEER: PeerHandle = PeerHandle(id = "AA:BB:CC:DD:EE:FF", name = "alex-desktop")

/** Records calls to assert exact count + arguments. */
private class RecordingAssociator(
    private val outcome: Result<AssociationHandle> = Result.success(
        AssociationHandle(associationId = 42L, peerId = TEST_PEER.id),
    ),
) : CompanionAssociator {
    var callCount: Int = 0
        private set
    var lastPeer: PeerHandle? = null
        private set

    override suspend fun associate(peer: PeerHandle): Result<AssociationHandle> {
        callCount += 1
        lastPeer = peer
        return outcome
    }
}

/** Trivial fakes; same shape as PairingViewModelTest.kt. */
private class StaticPairBackend(
    private val pickResult: PickPeerResult = PickPeerResult.LescStarted(code = "123456"),
) : PairBackend {
    override fun startScan() = Unit
    override fun stopScan() = Unit
    override fun pickPeer(peer: PeerHandle): PickPeerResult = pickResult
    override fun awaitLescResult(): LescResult =
        LescResult.Bonded(
            bondKey = ByteArray(BOND_KEY_LEN) { it.toByte() },
            peerName = TEST_PEER.name,
        )
}

private class FixedOobCalculator : OobCalculator {
    override fun compute(bondKey: ByteArray): List<String> =
        listOf("alpha", "beta", "gamma", "delta")
}

private class RecordingPersister(
    private val throwError: PersistError? = null,
) : BondPersister {
    val persisted: MutableList<BondRecord> = mutableListOf()
    override fun persist(record: BondRecord) {
        if (throwError != null) throw throwError
        persisted.add(record)
    }
}

private class RecordingRemover : BluetoothBondRemover {
    val removed: MutableList<String> = mutableListOf()
    override fun remove(peerId: String): Boolean {
        removed.add(peerId)
        return true
    }
}

private fun buildVm(
    associator: CompanionAssociator,
    persister: RecordingPersister = RecordingPersister(),
    remover: RecordingRemover = RecordingRemover(),
): PairingViewModel = PairingViewModel(
    backend = StaticPairBackend(),
    oobCalculator = FixedOobCalculator(),
    bondPersister = persister,
    bondRemover = remover,
    companionAssociator = associator,
    associateDispatcher = Dispatchers.Unconfined,
)

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class PairingViewModelCdmAssociationTest {

    private fun driveToOobConfirming(vm: PairingViewModel) {
        vm.onStartScanTapped()
        vm.onPeerPicked(TEST_PEER)
        vm.onLescResult(
            LescResult.Bonded(
                bondKey = ByteArray(BOND_KEY_LEN) { it.toByte() },
                peerName = TEST_PEER.name,
            ),
        )
    }

    @Test
    fun oob_yes_associates_then_transitions_to_bonded() {
        val associator = RecordingAssociator()
        val persister = RecordingPersister()
        val remover = RecordingRemover()
        val vm = buildVm(associator, persister, remover)

        driveToOobConfirming(vm)
        vm.onOobYesTapped()

        assertEquals(1, associator.callCount)
        assertEquals(TEST_PEER, associator.lastPeer)
        val state = vm.state.value
        assertTrue("expected Bonded, got $state", state is PairingState.Bonded)
        assertEquals(TEST_PEER.name, (state as PairingState.Bonded).name)
        assertEquals(1, persister.persisted.size)
        assertEquals(0, remover.removed.size)
    }

    @Test
    fun oob_yes_association_failure_emits_failed_and_rolls_back_bond() {
        val associator = RecordingAssociator(
            outcome = Result.failure(CompanionAssociationError("user rejected dialog")),
        )
        val persister = RecordingPersister()
        val remover = RecordingRemover()
        val vm = buildVm(associator, persister, remover)

        driveToOobConfirming(vm)
        vm.onOobYesTapped()

        assertEquals(1, associator.callCount)
        val state = vm.state.value
        assertTrue("expected Failed, got $state", state is PairingState.Failed)
        val reason = (state as PairingState.Failed).reason
        assertTrue(
            "reason should mention CDM rejection, got: $reason",
            reason.startsWith("companion-device association rejected:"),
        )
        assertTrue(
            "reason should include the inner cause, got: $reason",
            reason.contains("user rejected dialog"),
        )
        // BT bond is rolled back on the associate-failure path.
        assertEquals(listOf(TEST_PEER.id), remover.removed)
        // Persister was called once (before associate); the Kotlin
        // layer does not have a `remove(peerId)` on BondPersister in
        // v0.1 (documented residual in PairingViewModel.onOobYesTapped).
        assertEquals(1, persister.persisted.size)
    }

    @Test
    fun oob_no_does_not_associate() {
        val associator = RecordingAssociator()
        val persister = RecordingPersister()
        val remover = RecordingRemover()
        val vm = buildVm(associator, persister, remover)

        driveToOobConfirming(vm)
        vm.onOobNoTapped()

        assertEquals(0, associator.callCount)
        val state = vm.state.value
        assertTrue("expected Failed, got $state", state is PairingState.Failed)
        // S-016 invariant TC-07 still holds.
        assertEquals(0, persister.persisted.size)
        assertEquals(listOf(TEST_PEER.id), remover.removed)
    }

    @Test
    fun persist_failure_skips_associate_call() {
        val associator = RecordingAssociator()
        val persister = RecordingPersister(throwError = PersistError("disk full"))
        val remover = RecordingRemover()
        val vm = buildVm(associator, persister, remover)

        driveToOobConfirming(vm)
        vm.onOobYesTapped()

        // Persist failed -> associator is never reached.
        assertEquals(0, associator.callCount)
        val state = vm.state.value
        assertTrue("expected Failed, got $state", state is PairingState.Failed)
    }
}
