// Roadmap item S-017 — production BiometricPrompt presenter.
//
// Bridges the [BiometricPresenter] interface to AndroidX's
// `androidx.biometric:biometric:1.2.0-alpha05` BiometricPrompt API.
// The class is constructed with the host `FragmentActivity` and a
// hostname (used for the prompt title); each call to `authenticate`
// wraps the Keystore-backed `Signature` in a `CryptoObject` and
// suspends until the OS reports a terminal result.
//
// The prompt's allowed authenticators are
// `BIOMETRIC_STRONG | DEVICE_CREDENTIAL` per the SPEC §3.D6 and the
// S-017 DoD. `BIOMETRIC_STRONG` is required to use a CryptoObject
// (BIOMETRIC_WEAK / Class 2 biometrics cannot bind a Keystore key).
// Adding `DEVICE_CREDENTIAL` lets users without enrolled biometrics
// fall through to PIN / pattern / password rather than failing
// outright.
package com.sy.syauth.android.approve

import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import java.security.Signature
import kotlin.coroutines.resume
import kotlinx.coroutines.suspendCancellableCoroutine

/**
 * Allowed-authenticators bitmask used by the production prompt.
 *
 * Per `androidx.biometric:biometric:1.2.0-alpha05`:
 *
 *   - `BIOMETRIC_STRONG` — Class 3 biometric (fingerprint, face on
 *     supported hardware). Required for a `CryptoObject` binding.
 *   - `DEVICE_CREDENTIAL` — PIN / pattern / password. Lets a user
 *     without a fingerprint still authenticate.
 *
 * On API 28-29 the combination `STRONG | DEVICE_CREDENTIAL` is not
 * always supported; the AndroidX biometric library handles that
 * fallback internally as of 1.2.0-alpha05.
 */
public const val ALLOWED_AUTHENTICATORS: Int =
    BiometricManager.Authenticators.BIOMETRIC_STRONG or
        BiometricManager.Authenticators.DEVICE_CREDENTIAL

/**
 * Production [BiometricPresenter] backed by `androidx.biometric`.
 *
 * @param activity the host FragmentActivity. Held weakly via the
 *   `BiometricPrompt` constructor's lifecycle observer; callers must
 *   construct a fresh presenter per activity instance.
 * @param hostname the peer's friendly name; included in the prompt
 *   title.
 */
public class AndroidBiometricPresenter(
    private val activity: FragmentActivity,
    private val hostname: String,
) : BiometricPresenter {

    override suspend fun authenticate(signature: Signature): BiometricResult =
        suspendCancellableCoroutine { continuation ->
            val executor = ContextCompat.getMainExecutor(activity)
            val callback = object : BiometricPrompt.AuthenticationCallback() {
                override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                    val sig = result.cryptoObject?.signature
                    if (sig == null) {
                        continuation.resume(
                            BiometricResult.Failed("crypto object missing signature"),
                        )
                    } else {
                        continuation.resume(BiometricResult.Success(sig))
                    }
                }

                override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                    val reason = "[$errorCode] $errString"
                    val mapped = when (errorCode) {
                        BiometricPrompt.ERROR_NO_BIOMETRICS,
                        BiometricPrompt.ERROR_HW_NOT_PRESENT,
                        BiometricPrompt.ERROR_HW_UNAVAILABLE,
                        BiometricPrompt.ERROR_NO_DEVICE_CREDENTIAL,
                        BiometricPrompt.ERROR_SECURITY_UPDATE_REQUIRED ->
                            BiometricResult.Unavailable(reason)
                        else -> BiometricResult.Failed(reason)
                    }
                    continuation.resume(mapped)
                }

                override fun onAuthenticationFailed() {
                    // Soft failure (e.g., bad fingerprint). The prompt
                    // remains open until the user explicitly cancels
                    // or succeeds; we do not resume here.
                }
            }
            val prompt = BiometricPrompt(activity, executor, callback)
            val promptInfo = BiometricPrompt.PromptInfo.Builder()
                .setTitle("Approve unlock")
                .setSubtitle(hostname)
                .setAllowedAuthenticators(ALLOWED_AUTHENTICATORS)
                .build()
            prompt.authenticate(promptInfo, BiometricPrompt.CryptoObject(signature))
            continuation.invokeOnCancellation { prompt.cancelAuthentication() }
        }
}
