// Roadmap item S-017 — BiometricPrompt seam.
//
// The Compose ViewModel does not invoke `BiometricPrompt` directly
// because the prompt is bound to an `Activity` / `Fragment` lifecycle
// and cannot be constructed in a pure-JVM unit test. Instead the
// ViewModel takes a [BiometricPresenter] interface; production wires
// `AndroidBiometricPresenter` (which uses
// `androidx.biometric:biometric:1.2.0-alpha05`) and tests pass a fake
// that returns a canned [BiometricResult].
//
// The presenter is responsible for:
//
//   1. Constructing the `BiometricPrompt.PromptInfo` with the
//      hostname-scoped title and `BIOMETRIC_STRONG | DEVICE_CREDENTIAL`
//      allowed authenticators.
//   2. Wrapping the Keystore-backed `Signature` into a
//      `BiometricPrompt.CryptoObject` so the OS gates the actual
//      `sign()` call on biometric.
//   3. Translating the platform's `AuthenticationCallback` into a
//      typed [BiometricResult].
//
// The interface intentionally does NOT expose any Android types in its
// signature — the only platform type that crosses is
// `java.security.Signature`, which exists in the standard JVM
// (`java.security.*`), so unit tests can construct one without an
// Android shim.
package com.sy.syauth.android.approve

import java.security.Signature

/**
 * Result of a single [BiometricPresenter.authenticate] invocation.
 *
 * `Success` carries the same `Signature` instance that was passed in —
 * after biometric unlock, this `Signature` is ready to invoke `sign()`
 * exactly once (per Android Keystore semantics).
 *
 * `Failed` and `Unavailable` are distinct so the ViewModel can map them
 * to `DenialReason.BiometricFailed` vs `DenialReason.BiometricUnavailable`
 * for observability.
 */
public sealed class BiometricResult {
    public data class Success(val signature: Signature) : BiometricResult()
    public data class Failed(val reason: String) : BiometricResult()
    public data class Unavailable(val reason: String) : BiometricResult()
}

/**
 * Contract for invoking `BiometricPrompt`. The single method is
 * `suspend` because the platform callback is async; production wraps
 * it in `suspendCancellableCoroutine`.
 */
public interface BiometricPresenter {
    /**
     * Show `BiometricPrompt` with [signature] wrapped in a
     * `CryptoObject` and return the result. Implementations MUST NOT
     * throw — every failure mode is a typed [BiometricResult.Failed]
     * or [BiometricResult.Unavailable] variant.
     */
    public suspend fun authenticate(signature: Signature): BiometricResult
}
