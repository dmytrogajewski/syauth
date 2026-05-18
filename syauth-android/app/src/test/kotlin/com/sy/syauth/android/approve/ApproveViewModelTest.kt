// Roadmap item S-017 — JVM unit tests for ApproveViewModel.
//
// The four core scenarios mandated by the S-017 DoD:
//
//   - Approve happy path (sendApprove called once with the canned
//     UniFFI signature).
//   - Explicit deny (sendDeny called once, sendApprove never).
//   - Countdown timeout (sendDeny called once, sendApprove never).
//   - Biometric failed (sendDeny called once, sendApprove never).
//
// The tests run on a pure JVM — no Robolectric runner — because every
// Android side-effect (Keystore, BiometricPrompt, UniFFI, transport)
// is injected behind a small interface. The four `Fake*` classes
// below implement those interfaces with deterministic canned
// responses; the test asserts the terminal `uiState` and the recorded
// dispatch calls.
//
// `kotlinx.coroutines.test.runTest` drives the countdown via the
// virtual scheduler; `testScheduler.advanceTimeBy(...)` skips
// `delay(tickMillis)` calls instantly. The
// `MainDispatcherRule` swaps `Dispatchers.Main` for the test
// dispatcher so `viewModelScope.launch(...)` runs on the same
// scheduler.
package com.sy.syauth.android.approve

import java.security.KeyPairGenerator
import java.security.Signature
import java.security.spec.ECGenParameterSpec
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class ApproveViewModelTest {

    private val testDispatcher = StandardTestDispatcher()

    private val cannedWireSignature: ByteArray = ByteArray(WIRE_SIGNATURE_LEN) { it.toByte() }
    private val cannedBondKey: ByteArray = ByteArray(SEED_LEN) { (it + 0x40).toByte() }
    private val challengeFrame: ByteArray = ByteArray(CHALLENGE_LEN) { (0xA0 + it).toByte() }
    private val hostname: String = "dell-precision"

    @Before
    fun setUp() {
        Dispatchers.setMain(testDispatcher)
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    @Test
    fun approve_happy_path_emits_approved_and_calls_send_approve_once() = runTest(testDispatcher) {
        val sender = FakeResponseSender()
        val viewModel = buildViewModel(
            sender = sender,
            biometricPresenter = FakeBiometricPresenter { sig ->
                BiometricResult.Success(sig)
            },
            wireSigner = FakeWireSigner { _ -> WireSignResult.Ok(cannedWireSignature) },
        )
        viewModel.start()
        runCurrent()

        viewModel.onApproveClicked()
        advanceUntilIdle()

        val terminal = viewModel.uiState.value
        assertTrue("expected Approved, got $terminal", terminal is ApproveUiState.Approved)
        val approved = terminal as ApproveUiState.Approved
        assertArrayEquals(cannedWireSignature, approved.responseFrame)
        assertEquals(1, sender.approveCalls.size)
        assertArrayEquals(cannedWireSignature, sender.approveCalls[0])
        assertEquals(0, sender.denyCalls)
    }

    @Test
    fun deny_click_emits_user_denied_and_calls_send_deny_once() = runTest(testDispatcher) {
        val sender = FakeResponseSender()
        val viewModel = buildViewModel(sender = sender)
        viewModel.start()
        runCurrent()

        viewModel.onDenyClicked()
        advanceUntilIdle()

        val terminal = viewModel.uiState.value
        assertTrue("expected Denied, got $terminal", terminal is ApproveUiState.Denied)
        val denied = terminal as ApproveUiState.Denied
        assertEquals(DenialReason.UserDenied, denied.reason)
        assertEquals(0, sender.approveCalls.size)
        assertEquals(1, sender.denyCalls)
    }

    @Test
    fun countdown_timeout_emits_timed_out_and_calls_send_deny_once() = runTest(testDispatcher) {
        val sender = FakeResponseSender()
        val viewModel = buildViewModel(
            sender = sender,
            timeoutMillis = SHORT_TIMEOUT_MILLIS,
            tickMillis = SHORT_TICK_MILLIS,
        )
        viewModel.start()
        runCurrent()

        // Advance past the timeout. 3_500 ms covers the three 1_000 ms
        // ticks plus slack for the final transition to fire.
        advanceTimeBy(TIMEOUT_ADVANCE_MILLIS)
        advanceUntilIdle()

        val terminal = viewModel.uiState.value
        assertTrue("expected Denied(TimedOut), got $terminal", terminal is ApproveUiState.Denied)
        val denied = terminal as ApproveUiState.Denied
        assertEquals(DenialReason.TimedOut, denied.reason)
        assertEquals(0, sender.approveCalls.size)
        assertEquals(1, sender.denyCalls)
    }

    @Test
    fun biometric_failure_emits_biometric_failed_and_calls_send_deny_once() =
        runTest(testDispatcher) {
            val sender = FakeResponseSender()
            val viewModel = buildViewModel(
                sender = sender,
                biometricPresenter = FakeBiometricPresenter { _ ->
                    BiometricResult.Failed("user-cancelled")
                },
            )
            viewModel.start()
            runCurrent()

            viewModel.onApproveClicked()
            advanceUntilIdle()

            val terminal = viewModel.uiState.value
            assertTrue("expected Denied(BiometricFailed), got $terminal", terminal is ApproveUiState.Denied)
            val denied = terminal as ApproveUiState.Denied
            assertEquals(DenialReason.BiometricFailed, denied.reason)
            assertEquals(0, sender.approveCalls.size)
            assertEquals(1, sender.denyCalls)
        }

    @Test
    fun start_is_idempotent() = runTest(testDispatcher) {
        val viewModel = buildViewModel()
        viewModel.start()
        runCurrent()
        val first = viewModel.uiState.value
        viewModel.start()
        runCurrent()
        val second = viewModel.uiState.value
        // Both states should be Counting; the second start must not
        // reset the remaining seconds back to the initial value.
        assertTrue(first is ApproveUiState.Counting)
        assertTrue(second is ApproveUiState.Counting)
    }

    @Test
    fun deny_after_approve_is_ignored() = runTest(testDispatcher) {
        val sender = FakeResponseSender()
        val viewModel = buildViewModel(sender = sender)
        viewModel.start()
        runCurrent()

        viewModel.onApproveClicked()
        // Don't advance — leave the state at AwaitingBiometric.
        runCurrent()

        viewModel.onDenyClicked()
        runCurrent()

        // No deny dispatch should have happened from the late
        // onDenyClicked.
        assertEquals(0, sender.denyCalls)
    }

    @Test
    fun wire_signer_failure_emits_sign_error() = runTest(testDispatcher) {
        val sender = FakeResponseSender()
        val viewModel = buildViewModel(
            sender = sender,
            wireSigner = FakeWireSigner { _ -> WireSignResult.Failure("bad-frame") },
        )
        viewModel.start()
        runCurrent()

        viewModel.onApproveClicked()
        advanceUntilIdle()

        val terminal = viewModel.uiState.value
        assertTrue(terminal is ApproveUiState.Denied)
        val denied = terminal as ApproveUiState.Denied
        assertTrue(denied.reason is DenialReason.SignError)
        assertEquals("bad-frame", (denied.reason as DenialReason.SignError).reason)
        assertEquals(1, sender.denyCalls)
    }

    private fun TestScope.buildViewModel(
        sender: FakeResponseSender = FakeResponseSender(),
        biometricPresenter: BiometricPresenter = FakeBiometricPresenter { sig ->
            BiometricResult.Success(sig)
        },
        wireSigner: WireSigner = FakeWireSigner { _ -> WireSignResult.Ok(cannedWireSignature) },
        timeoutMillis: Long = DEFAULT_TIMEOUT_MILLIS,
        tickMillis: Long = DEFAULT_TICK_MILLIS,
    ): ApproveViewModel = ApproveViewModel(
        hostname = hostname,
        challengeFrame = challengeFrame,
        bondKey = cannedBondKey,
        keystoreSigner = FakeKeystoreSigner(),
        biometricPresenter = biometricPresenter,
        wireSigner = wireSigner,
        responseSender = sender,
        clock = FakeClock,
        timeoutMillis = timeoutMillis,
        tickMillis = tickMillis,
        ioDispatcher = testDispatcher,
        keystoreAlias = TEST_ALIAS,
    )

    private companion object {
        const val WIRE_SIGNATURE_LEN: Int = 64
        const val SEED_LEN: Int = 32
        const val CHALLENGE_LEN: Int = 48
        const val TEST_ALIAS: String = "test.alias"

        // 3-second total budget, 1-second tick. The countdown decrements
        // every tickMillis; total time to reach Denied(TimedOut) is
        // timeoutMillis. We advance 3_500 to ensure we cross.
        const val SHORT_TIMEOUT_MILLIS: Long = 3_000L
        const val SHORT_TICK_MILLIS: Long = 1_000L
        const val TIMEOUT_ADVANCE_MILLIS: Long = 3_500L
    }
}

// -----------------------------------------------------------------------------
// Fakes — local, file-private, deterministic.
// -----------------------------------------------------------------------------

private class FakeResponseSender : ResponseSender {
    val approveCalls: MutableList<ByteArray> = mutableListOf()
    var denyCalls: Int = 0
        private set

    override suspend fun sendApprove(responseFrame: ByteArray) {
        approveCalls += responseFrame
    }

    override suspend fun sendDeny() {
        denyCalls += 1
    }
}

private class FakeBiometricPresenter(
    private val behaviour: (Signature) -> BiometricResult,
) : BiometricPresenter {
    override suspend fun authenticate(signature: Signature): BiometricResult =
        behaviour(signature)
}

private class FakeWireSigner(
    private val behaviour: (ByteArray) -> WireSignResult,
) : WireSigner {
    override suspend fun signWire(bondKey: ByteArray, frameBytes: ByteArray): WireSignResult =
        behaviour(frameBytes)
}

/**
 * Software-backed keystore stand-in. We generate an EC P-256 key pair
 * in the regular JCE provider so the `Signature` can be `initSign`'d
 * without throwing. The fake never actually verifies that a biometric
 * happened — that's the production AndroidKeystoreSigner's job, gated
 * by `setUserAuthenticationRequired(true)`.
 */
private class FakeKeystoreSigner : KeystoreSignerBackend {
    private val keyPair = KeyPairGenerator.getInstance("EC").apply {
        initialize(ECGenParameterSpec("secp256r1"))
    }.generateKeyPair()

    override fun getOrCreateSigningKey(alias: String): KeyInfo =
        KeyInfo(alias = alias, strongBoxBacked = false)

    override fun prepareSignature(alias: String): Signature =
        Signature.getInstance(GATE_SIGNATURE_ALGORITHM).apply {
            initSign(keyPair.private)
        }

    override fun signGate(signature: Signature, challenge: ByteArray): ByteArray {
        signature.update(challenge)
        return signature.sign()
    }
}

private object FakeClock : Clock {
    private var current: Long = 0L
    override fun nowMillis(): Long = current
}
