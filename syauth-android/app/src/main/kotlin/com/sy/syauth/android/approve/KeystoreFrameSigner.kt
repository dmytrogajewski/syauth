// DEV-002: Keystore-backed [uniffi.syauth_mobile.FrameSigner].
//
// Implements the UniFFI callback interface exposed by the
// `syauth-mobile` Rust crate. The Rust side calls `sign(message)` to
// obtain the 64-byte Ed25519 signature over the unsigned response-frame
// body; this class opens the Keystore-resident Ed25519 `PrivateKey`
// under the per-bond [alias], builds a `Signature.getInstance("Ed25519")`,
// updates it with the message bytes, and returns the result. The
// private key NEVER appears as bytes in the JVM — that closes the
// SPEC §3.2 D6 gap row `docs/known-gaps.md` DEV-002.
//
// The Keystore key was generated at pair time by
// [com.sy.syauth.android.pair.impl.KeystoreKeyGenerator] with
// `setUserAuthenticationRequired(true)`, so the underlying `sign()`
// call only succeeds AFTER a fresh BiometricPrompt unlock; the
// approve view-model's existing biometric gate is what releases the
// key for the single signature this class produces. A second unlock
// would require a second biometric.
package com.sy.syauth.android.approve

import android.os.Build
import androidx.annotation.RequiresApi
import java.security.KeyStore
import java.security.PrivateKey
import java.security.Signature
import uniffi.syauth_mobile.FrameSigner

/**
 * Typed failure surface for [KeystoreFrameSigner]. The UniFFI Rust
 * side cannot distinguish variants — the only thing it sees is the
 * wrong-length signature blob and surfaces `MobileError::SignFailed`.
 * Logging here records the underlying cause for the audit log.
 */
public sealed class KeystoreFrameSignerError(message: String) : RuntimeException(message) {
    public class AliasMissing(alias: String) :
        KeystoreFrameSignerError("Keystore alias '$alias' is not present")
    public class NotAPrivateKey(alias: String) :
        KeystoreFrameSignerError("Keystore entry under '$alias' is not a PrivateKey")
    public class SignFailed(message: String, cause: Throwable?) :
        KeystoreFrameSignerError(message) {
        init {
            initCause(cause)
        }
    }
}

/**
 * Production [FrameSigner]. The constructor takes the per-bond
 * Keystore [alias] minted at pair time; every [sign] call opens a
 * fresh `Signature` instance so no shared mutable state crosses
 * threads.
 *
 * On failure the implementation returns an empty `ByteArray`: the
 * Rust side detects the wrong length and surfaces
 * `MobileError::SignFailed`, which the view-model maps onto
 * `DenialReason.SignError`. The underlying cause is logged via
 * `Log.w` so the audit trail records the real reason.
 *
 * Why API 33+ is required: Android Keystore supports the
 * `NamedParameterSpec("Ed25519")` curve only from API 33 (Tiramisu)
 * onwards. The pair-flow [KeystoreKeyGenerator] enforces the same
 * floor; production targets API 33+.
 */
@RequiresApi(Build.VERSION_CODES.TIRAMISU)
public class KeystoreFrameSigner(public val alias: String) : FrameSigner {

    override fun sign(message: ByteArray): ByteArray {
        return try {
            val key = openPrivateKey()
            val signature = Signature.getInstance(ED25519_SIGNATURE_ALGORITHM)
            signature.initSign(key)
            signature.update(message)
            signature.sign()
        } catch (e: KeystoreFrameSignerError) {
            android.util.Log.w(KEYSTORE_SIGNER_LOG_TAG, "keystore sign failed: ${e.message}")
            EMPTY_SIGNATURE
        } catch (e: java.security.GeneralSecurityException) {
            android.util.Log.w(KEYSTORE_SIGNER_LOG_TAG, "keystore sign failed: ${e.message}")
            EMPTY_SIGNATURE
        }
    }

    private fun openPrivateKey(): PrivateKey {
        val keystore = KeyStore.getInstance(KEYSTORE_PROVIDER).apply { load(null) }
        if (!keystore.containsAlias(alias)) {
            throw KeystoreFrameSignerError.AliasMissing(alias)
        }
        val entry = keystore.getKey(alias, null)
            ?: throw KeystoreFrameSignerError.AliasMissing(alias)
        return entry as? PrivateKey
            ?: throw KeystoreFrameSignerError.NotAPrivateKey(alias)
    }

    private companion object {
        /** Android Keystore provider name. */
        const val KEYSTORE_PROVIDER: String = "AndroidKeyStore"

        /** JCA signature algorithm name for Ed25519 (API 33+). */
        const val ED25519_SIGNATURE_ALGORITHM: String = "Ed25519"

        /** Logcat tag used by the production Keystore frame signer. */
        const val KEYSTORE_SIGNER_LOG_TAG: String = "syauth.keystore.signer"

        /** Empty signature blob returned on failure; the Rust side surfaces this as `MobileError::SignFailed`. */
        val EMPTY_SIGNATURE: ByteArray = ByteArray(0)
    }
}
