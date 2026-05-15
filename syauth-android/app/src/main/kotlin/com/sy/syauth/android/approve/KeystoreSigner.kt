// Roadmap item S-017 — Keystore-backed gate signer.
//
// `KeystoreSigner` owns the lifecycle of an EC P-256 (secp256r1) signing
// key inside the Android Keystore. The key is generated with:
//
//   - `setUserAuthenticationRequired(true)` so the private key can only
//     be used after a fresh biometric (or device-credential) gesture
//     unlocked the `Signature` instance via `BiometricPrompt`.
//   - `setUnlockedDeviceRequired(true)` so the key cannot be used while
//     the device is in the locked state.
//   - `setIsStrongBoxBacked(true)` when StrongBox is available; on
//     `StrongBoxUnavailableException` we transparently fall back to the
//     regular TEE-backed Keystore. The boolean is recorded on `KeyInfo`
//     so callers (and audit log emission in S-018) can observe the
//     choice the device made.
//
// EC P-256 was chosen over Ed25519 because Android Keystore's Ed25519
// support landed in API 33 and is not present on every Android 13
// device; secp256r1 is supported all the way back to API 23. The
// wire-protocol signature is Ed25519 and produced by the Rust core via
// the UniFFI `signChallengeResponse` surface (S-014). The Keystore
// signature is the gate proof — it certifies "a fresh user gesture
// authenticated this signing operation" — and is logged for audit but
// not sent on the wire.
//
// This file deliberately separates the *lifecycle* of the key
// (generation, fetch, signature initialization) from the *signing*
// step. `prepareSignature` returns a `Signature` that the caller must
// hand to `BiometricPrompt.CryptoObject(signature)` so the OS gates the
// `sign()` call on biometric. `signGate` is only safe to call AFTER
// `BiometricPrompt` has reported success — calling it without the
// biometric unlock raises `UserNotAuthenticatedException`, which we
// surface as a typed `KeystoreSignerError`.
package com.sy.syauth.android.approve

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import androidx.annotation.RequiresApi
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.PrivateKey
import java.security.Signature
import java.security.spec.ECGenParameterSpec

/**
 * Android Keystore provider name. Pinned per
 * [Android Developers — KeyStore](https://developer.android.com/training/articles/keystore).
 */
private const val KEYSTORE_PROVIDER: String = "AndroidKeyStore"

/**
 * EC curve name used for the gate signing key. P-256 / secp256r1 is
 * universally supported by Android Keystore from API 23+. Ed25519
 * support is API 33+ only and we cannot rely on it being present on
 * every Android 13 device, so we pick the always-available curve.
 */
private const val EC_CURVE_NAME: String = "secp256r1"

/**
 * JCA signature algorithm used with the gate key. SHA256withECDSA is
 * the canonical pairing for P-256 keys in the Android Keystore.
 */
internal const val GATE_SIGNATURE_ALGORITHM: String = "SHA256withECDSA"

/**
 * Default alias used by the production `KeystoreSigner`. Each device
 * has one gate key; tests inject a different alias to isolate test
 * fixtures from production state.
 */
public const val DEFAULT_KEYSTORE_GATE_ALIAS: String = "syauth.gate.v1"

/**
 * Snapshot of the gate key's provenance. Recorded once at key
 * generation time and surfaced to callers so the audit log (S-018) can
 * record whether StrongBox was used.
 *
 * @property alias the Keystore alias the key was generated under.
 * @property strongBoxBacked `true` if the key lives inside a hardware
 *   StrongBox enclave; `false` if it fell back to the regular TEE.
 */
public data class KeyInfo(
    val alias: String,
    val strongBoxBacked: Boolean,
)

/**
 * Typed error surface for [KeystoreSigner]. Every error is a domain
 * value — no exceptions cross the seam to the ViewModel.
 */
public sealed class KeystoreSignerError(message: String) : RuntimeException(message) {
    /** The Keystore reported `UserNotAuthenticatedException`. */
    public class UserNotAuthenticated(message: String) : KeystoreSignerError(message)

    /**
     * The Keystore key was permanently invalidated (e.g., the user
     * enrolled a new biometric while the app was backgrounded). The
     * caller should regenerate the key on next launch.
     */
    public class KeyInvalidated(message: String) : KeystoreSignerError(message)

    /** Generic Keystore failure (cipher, provider, init). */
    public class CryptoFailure(message: String) : KeystoreSignerError(message)
}

/**
 * Contract for the Keystore-backed gate signer. Pulled out as an
 * interface so the ViewModel can be unit-tested against a fake without
 * touching real Android crypto.
 */
public interface KeystoreSignerBackend {
    /**
     * Generate (or return the existing) gate signing key under [alias]
     * with the documented authentication requirements. Idempotent —
     * calling twice does not regenerate the key.
     */
    public fun getOrCreateSigningKey(alias: String): KeyInfo

    /**
     * Prepare a [Signature] initialized for signing under the gate key
     * at [alias]. The returned `Signature` MUST be wrapped in a
     * `BiometricPrompt.CryptoObject` and unlocked via biometric before
     * [signGate] is called.
     */
    public fun prepareSignature(alias: String): Signature

    /**
     * Produce a gate-proof signature blob over [challenge]. The caller
     * MUST have unlocked [signature] via biometric first; otherwise
     * `UserNotAuthenticatedException` is raised (we translate it to
     * [KeystoreSignerError.UserNotAuthenticated]).
     */
    public fun signGate(signature: Signature, challenge: ByteArray): ByteArray
}

/**
 * Production implementation backed by the Android Keystore.
 *
 * The first `getOrCreateSigningKey` call generates the key with the
 * documented parameters; subsequent calls are no-ops that report the
 * existing `KeyInfo`. The `strongBoxBacked` flag is set at generation
 * time and survives across process restarts because the key alias is
 * the identity.
 */
@RequiresApi(Build.VERSION_CODES.M)
public class AndroidKeystoreSigner : KeystoreSignerBackend {
    /**
     * StrongBox snapshot per alias. Populated on
     * `getOrCreateSigningKey` and consulted by `prepareSignature` /
     * `signGate` for telemetry. Not persisted — on a cold start we
     * re-read the choice via `KeyInfo`'s flag from a fresh
     * `getOrCreateSigningKey`.
     */
    private val keyInfoByAlias: MutableMap<String, KeyInfo> = mutableMapOf()

    override fun getOrCreateSigningKey(alias: String): KeyInfo {
        val cached = keyInfoByAlias[alias]
        if (cached != null) {
            return cached
        }
        val keystore = KeyStore.getInstance(KEYSTORE_PROVIDER).apply { load(null) }
        if (keystore.containsAlias(alias)) {
            // Recovering an existing key: we cannot retroactively
            // determine whether it was StrongBox-backed via the
            // platform API (KeyInfo via KeyFactory works post-API 23
            // but querying `isInsideSecureHardware` returns a generic
            // boolean, not a StrongBox flag). We report it as
            // non-StrongBox conservatively and the next regenerate
            // would update the flag.
            val info = KeyInfo(alias = alias, strongBoxBacked = false)
            keyInfoByAlias[alias] = info
            return info
        }
        return generateKey(alias)
    }

    override fun prepareSignature(alias: String): Signature {
        val keystore = KeyStore.getInstance(KEYSTORE_PROVIDER).apply { load(null) }
        val key: PrivateKey = (keystore.getKey(alias, null) as? PrivateKey)
            ?: throw KeystoreSignerError.CryptoFailure(
                "key under alias '$alias' is not a PrivateKey or is missing",
            )
        val signature = Signature.getInstance(GATE_SIGNATURE_ALGORITHM)
        runCatchingSig { signature.initSign(key) }
        return signature
    }

    override fun signGate(signature: Signature, challenge: ByteArray): ByteArray {
        return runCatchingSig {
            signature.update(challenge)
            signature.sign()
        }
    }

    private fun generateKey(alias: String): KeyInfo {
        val purposes = KeyProperties.PURPOSE_SIGN or KeyProperties.PURPOSE_VERIFY
        val baseBuilder = KeyGenParameterSpec.Builder(alias, purposes)
            .setAlgorithmParameterSpec(ECGenParameterSpec(EC_CURVE_NAME))
            .setDigests(KeyProperties.DIGEST_SHA256)
            .setUserAuthenticationRequired(true)

        // `setUnlockedDeviceRequired` was added in API 28 (P). On older
        // hardware we fall through silently; the
        // `setUserAuthenticationRequired(true)` gate is the primary
        // defense.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            baseBuilder.setUnlockedDeviceRequired(true)
        }

        // StrongBox is API 28+. Older hardware falls back without an
        // exception being thrown; we still wrap the build in
        // try/catch on `StrongBoxUnavailableException` for API 28+
        // devices that lack the StrongBox HAL.
        val (spec, strongBoxBacked) = buildWithOptionalStrongBox(baseBuilder)

        val generator = KeyPairGenerator.getInstance(
            KeyProperties.KEY_ALGORITHM_EC,
            KEYSTORE_PROVIDER,
        )
        generator.initialize(spec)
        generator.generateKeyPair()
        val info = KeyInfo(alias = alias, strongBoxBacked = strongBoxBacked)
        keyInfoByAlias[alias] = info
        return info
    }

    private fun buildWithOptionalStrongBox(
        builder: KeyGenParameterSpec.Builder,
    ): Pair<KeyGenParameterSpec, Boolean> {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.P) {
            return builder.build() to false
        }
        return try {
            builder.setIsStrongBoxBacked(true).build() to true
        } catch (_: StrongBoxUnavailableException) {
            builder.setIsStrongBoxBacked(false).build() to false
        }
    }

    /**
     * Wrap a [Signature] operation in a `try`/`catch` that maps every
     * platform exception to a typed [KeystoreSignerError]. The
     * `android.security.keystore.UserNotAuthenticatedException` and
     * `KeyPermanentlyInvalidatedException` classes are referenced by
     * fully qualified name via reflection-friendly catch blocks so the
     * file compiles without pulling in API-specific imports on hosts
     * with older SDK shims.
     */
    private inline fun <T> runCatchingSig(block: () -> T): T {
        return try {
            block()
        } catch (e: android.security.keystore.UserNotAuthenticatedException) {
            throw KeystoreSignerError.UserNotAuthenticated(
                e.message ?: "user not authenticated",
            )
        } catch (e: android.security.keystore.KeyPermanentlyInvalidatedException) {
            throw KeystoreSignerError.KeyInvalidated(
                e.message ?: "key permanently invalidated",
            )
        } catch (e: java.security.GeneralSecurityException) {
            throw KeystoreSignerError.CryptoFailure(
                e.message ?: "keystore crypto failure",
            )
        }
    }
}
