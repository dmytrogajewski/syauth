// Roadmap item S-017 — Approve screen ViewModel.
//
// The ViewModel orchestrates the user-visible state machine documented
// in `specs/journeys/JOURNEY-S-017-android-approve.md` §4. Every
// transition is observable via the public `StateFlow<ApproveUiState>`
// — tests read the flow directly and assert terminal values.
//
// Every external side-effect is injected behind an interface so the
// unit tests can run on a pure JVM (no Robolectric, no Android crypto,
// no UniFFI library loading). The five seams are:
//
//   1. [BiometricPresenter]        — wraps BiometricPrompt.
//   2. [KeystoreSignerBackend]     — wraps the Keystore EC gate.
//   3. [SigningKeyProvider]        — wraps the Ed25519 seed source.
//   4. [WireSigner]                — wraps the UniFFI Ed25519 surface.
//   5. [ResponseSender]            — wraps the GATT response transport.
//
// Plus two utility seams ([Clock] and the constructor-injected
// `tickMillis` / `timeoutMillis` constants) that let tests run the
// countdown in virtual time.
package com.sy.syauth.android.approve

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/** Default approve-window length. SPEC §4.3 budget for human reaction. */
public const val DEFAULT_TIMEOUT_MILLIS: Long = 30_000L

/** Default countdown tick. One second is the human-perceptible cadence. */
public const val DEFAULT_TICK_MILLIS: Long = 1_000L

/** Conversion factor used by the countdown to render seconds. */
private const val MILLIS_PER_SECOND: Long = 1_000L

/**
 * Why the approve flow terminated in a [ApproveUiState.Denied] state.
 * The desktop sees every variant identically as a `PeerDenied` wire
 * frame; the distinction exists for the phone-side audit log and the
 * Compose UI's "Denied: <reason>" line.
 */
public sealed class DenialReason {
    public data object UserDenied : DenialReason()
    public data object TimedOut : DenialReason()
    public data object BiometricFailed : DenialReason()
    public data object BiometricUnavailable : DenialReason()
    public data class SignError(val reason: String) : DenialReason()
}

/**
 * Public state surface of the Approve screen. Render the screen as a
 * pure function of this value; every transition is driven by the
 * ViewModel.
 */
public sealed class ApproveUiState {
    public data object Idle : ApproveUiState()
    public data class Counting(val remainingSeconds: Int) : ApproveUiState()
    public data object AwaitingBiometric : ApproveUiState()
    public data object Signing : ApproveUiState()

    /**
     * Approve flow completed; [responseFrame] is the 64-byte Ed25519
     * signature returned by UniFFI `signChallengeResponse`.
     */
    public data class Approved(val responseFrame: ByteArray) : ApproveUiState() {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Approved) return false
            return responseFrame.contentEquals(other.responseFrame)
        }

        override fun hashCode(): Int = responseFrame.contentHashCode()
    }

    public data class Denied(val reason: DenialReason) : ApproveUiState()
}

/**
 * Contract for the wire-signing call. Production wires this to
 * `uniffi.syauth_mobile.signChallengeResponse(seed, frame)`; tests
 * inject a fake that returns a canned `ByteArray`.
 *
 * The seam exists because the UniFFI binding loads a native `.so` at
 * class init; importing it in JVM-only unit tests fails with
 * `UnsatisfiedLinkError` on hosts without the AAR.
 */
public interface WireSigner {
    /**
     * Sign [frameBytes] with [seed] and return the 64-byte Ed25519
     * signature. Implementations MUST NOT throw — every failure becomes
     * a typed [WireSignResult.Failure].
     */
    public suspend fun signWire(seed: ByteArray, frameBytes: ByteArray): WireSignResult
}

/** Typed result of a [WireSigner.signWire] call. */
public sealed class WireSignResult {
    public data class Ok(val signature: ByteArray) : WireSignResult() {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Ok) return false
            return signature.contentEquals(other.signature)
        }

        override fun hashCode(): Int = signature.contentHashCode()
    }

    public data class Failure(val reason: String) : WireSignResult()
}

/**
 * Small clock seam. Reserved for future use by the audit log (S-018
 * wires the consumer) and held here so the ViewModel constructor
 * exposes the seam for callers that want to inject a fake clock for
 * deterministic timestamps. The countdown itself is driven by
 * `kotlinx.coroutines.delay`, which the kotlinx-coroutines-test virtual
 * scheduler controls directly — so the production `Clock.System`
 * implementation is not consulted in the unit tests.
 */
public interface Clock {
    public fun nowMillis(): Long

    public object System : Clock {
        override fun nowMillis(): Long = java.lang.System.currentTimeMillis()
    }
}

/**
 * Approve screen ViewModel. Constructed once per challenge, with the
 * already-verified [challengeFrame] from the background bridge and the
 * peer's friendly [hostname] for display.
 *
 * The countdown starts from [timeoutMillis] when [start] is invoked
 * (which the Compose screen calls in a `LaunchedEffect(Unit)`); the
 * tick cadence is [tickMillis]. Both are constructor-injected so the
 * test can supply 3_000 / 1_000 for a fast-running test.
 *
 * @param hostname the peer's friendly name (rendered on the screen).
 * @param challengeFrame the already-MAC-verified wire-frame bytes.
 * @param keystoreSigner Keystore-backed gate signer.
 * @param biometricPresenter wraps BiometricPrompt.
 * @param signingKeyProvider source for the Ed25519 seed.
 * @param wireSigner UniFFI surface adapter.
 * @param responseSender ships the terminal frame back to the desktop.
 * @param clock test-injectable clock (unused outside tests today).
 * @param timeoutMillis total countdown length.
 * @param tickMillis countdown cadence.
 * @param ioDispatcher dispatcher for the suspend operations; tests
 *   supply the `runTest` scheduler.
 */
@Suppress("LongParameterList")
public class ApproveViewModel(
    public val hostname: String,
    public val challengeFrame: ByteArray,
    private val keystoreSigner: KeystoreSignerBackend,
    private val biometricPresenter: BiometricPresenter,
    private val signingKeyProvider: SigningKeyProvider,
    private val wireSigner: WireSigner,
    private val responseSender: ResponseSender,
    private val clock: Clock = Clock.System,
    private val timeoutMillis: Long = DEFAULT_TIMEOUT_MILLIS,
    private val tickMillis: Long = DEFAULT_TICK_MILLIS,
    private val ioDispatcher: CoroutineDispatcher = Dispatchers.Default,
    private val keystoreAlias: String = DEFAULT_KEYSTORE_GATE_ALIAS,
) : ViewModel() {

    private val _uiState: MutableStateFlow<ApproveUiState> = MutableStateFlow(ApproveUiState.Idle)
    public val uiState: StateFlow<ApproveUiState> = _uiState.asStateFlow()

    /**
     * Job for the countdown coroutine. Cancelled when the state leaves
     * `Counting` so a late tick cannot transition a terminal state.
     */
    private var countdownJob: Job? = null

    /**
     * Cached `KeyInfo` from the first `getOrCreateSigningKey` call,
     * exposed for the audit log (S-018 wires the consumer) and for
     * tests that want to assert the StrongBox flag.
     */
    public var keyInfo: KeyInfo? = null
        private set

    init {
        // Ensure the Keystore key exists before the user can tap
        // Approve. On a real device this is microseconds; we still
        // guard against a generation failure by surfacing it as a
        // terminal SignError on the first tap. Storing `keyInfo` lets
        // tests assert the StrongBox boolean.
        keyInfo = runCatching { keystoreSigner.getOrCreateSigningKey(keystoreAlias) }.getOrNull()
    }

    /**
     * Start the countdown. The Compose screen calls this from a
     * `LaunchedEffect(Unit)` so it fires exactly once per screen
     * lifecycle. Idempotent: a second call is ignored.
     */
    public fun start() {
        if (_uiState.value !is ApproveUiState.Idle) {
            return
        }
        val initialSeconds = millisToSeconds(timeoutMillis)
        _uiState.value = ApproveUiState.Counting(initialSeconds)
        countdownJob = viewModelScope.launch(ioDispatcher) {
            runCountdown()
        }
    }

    /**
     * Drive the countdown off remaining-millis rather than
     * remaining-ticks so a non-1000 ms tick still reaches zero at
     * exactly `timeoutMillis` total elapsed time. Renders the
     * remaining seconds at every tick boundary.
     */
    private suspend fun runCountdown() {
        var remainingMillis = timeoutMillis
        while (remainingMillis > 0) {
            delay(tickMillis)
            // If the state already moved away from Counting (user
            // tapped Approve or Deny), stop ticking — we must not
            // overwrite a terminal state with a stale Counting value.
            val current = _uiState.value
            if (current !is ApproveUiState.Counting) {
                return
            }
            remainingMillis -= tickMillis
            if (remainingMillis <= 0) {
                onTimeout()
                return
            }
            _uiState.value = ApproveUiState.Counting(millisToSeconds(remainingMillis))
        }
    }

    private fun millisToSeconds(millis: Long): Int {
        val rounded = (millis + MILLIS_PER_SECOND - 1) / MILLIS_PER_SECOND
        return rounded.toInt()
    }

    /**
     * User tapped Approve. Transitions Counting → AwaitingBiometric →
     * Signing → Approved / Denied(SignError|BiometricFailed).
     *
     * Ignored if the state is not `Counting` (defensive — the screen
     * disables the button outside `Counting`).
     */
    public fun onApproveClicked() {
        val current = _uiState.value
        if (current !is ApproveUiState.Counting) {
            return
        }
        countdownJob?.cancel()
        _uiState.value = ApproveUiState.AwaitingBiometric
        viewModelScope.launch(ioDispatcher) {
            runApproveFlow()
        }
    }

    private suspend fun runApproveFlow() {
        val signature = runCatching { keystoreSigner.prepareSignature(keystoreAlias) }
            .getOrElse { throwable ->
                emitSignError(throwable.message ?: "prepareSignature failed")
                return
            }

        val biometricResult = biometricPresenter.authenticate(signature)
        if (biometricResult is BiometricResult.Unavailable) {
            transitionToDenied(DenialReason.BiometricUnavailable)
            return
        }
        if (biometricResult !is BiometricResult.Success) {
            transitionToDenied(DenialReason.BiometricFailed)
            return
        }

        _uiState.value = ApproveUiState.Signing

        // Run the Keystore gate sign as audit proof that biometric
        // really happened. We do not put the gate signature on the
        // wire — its sole purpose is to fail loudly if the Keystore
        // refused to release the key (which would mean the
        // BiometricPrompt callback fired with `Success` but the
        // Keystore disagreed — a contract violation we want to
        // observe).
        val gateOk = runCatching {
            keystoreSigner.signGate(biometricResult.signature, challengeFrame)
        }
        if (gateOk.isFailure) {
            val msg = gateOk.exceptionOrNull()?.message ?: "gate sign failed"
            emitSignError(msg)
            return
        }

        val seedResult = signingKeyProvider.loadSeed()
        if (seedResult !is SigningKeyResult.Ok) {
            val msg = (seedResult as SigningKeyResult.Missing).reason
            emitSignError(msg)
            return
        }

        val wireResult = wireSigner.signWire(seedResult.seed, challengeFrame)
        if (wireResult !is WireSignResult.Ok) {
            val msg = (wireResult as WireSignResult.Failure).reason
            emitSignError(msg)
            return
        }

        _uiState.value = ApproveUiState.Approved(wireResult.signature)
        responseSender.sendApprove(wireResult.signature)
    }

    /**
     * User tapped Deny. Transitions immediately to
     * `Denied(UserDenied)`. Ignored if the state is already terminal.
     */
    public fun onDenyClicked() {
        val current = _uiState.value
        if (current !is ApproveUiState.Counting) {
            return
        }
        countdownJob?.cancel()
        transitionToDenied(DenialReason.UserDenied)
    }

    private fun onTimeout() {
        if (_uiState.value !is ApproveUiState.Counting) {
            return
        }
        transitionToDenied(DenialReason.TimedOut)
    }

    private fun transitionToDenied(reason: DenialReason) {
        _uiState.value = ApproveUiState.Denied(reason)
        // Launch the sender on a separate coroutine so the caller
        // (which may be the countdown tick or the UI thread) is not
        // blocked on the GATT write.
        viewModelScope.launch(ioDispatcher) {
            responseSender.sendDeny()
        }
    }

    private fun emitSignError(reason: String) {
        _uiState.value = ApproveUiState.Denied(DenialReason.SignError(reason))
        viewModelScope.launch(ioDispatcher) {
            responseSender.sendDeny()
        }
    }

    /**
     * Surface the injected clock so a future consumer (the audit log
     * in S-018) can record the wall-clock time of each transition.
     * Exposed publicly so observers don't have to reach into the
     * ViewModel's private state.
     */
    public fun clockNowMillis(): Long = clock.nowMillis()
}
