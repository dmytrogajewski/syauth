// DEV-002: pair-time Ed25519 keypair generation inside the Android
// Keystore (STRONGBOX-preferred).
//
// Closes the SPEC §3.2 D6 gap row `docs/known-gaps.md` DEV-002 — the
// Ed25519 private key NEVER appears as bytes anywhere. The pair flow
// minted the key on the desktop and shipped the seed across the LESC
// link before; the DEV-002 closure inverts that: the phone generates
// the keypair inside its own Keystore, extracts the 32-byte Ed25519
// public key from the resulting `Certificate`, and sends only the
// pubkey across the LESC link. The desktop verifies signatures
// against the pubkey on every unlock.
//
// API floor: the `NamedParameterSpec("Ed25519")` curve descriptor is
// available only on API 33 (Tiramisu) and later. The app's
// `minSdk = 26`, so on API 26-32 devices this generator returns
// [KeystoreKeygenError.UnsupportedApi] and the pair flow aborts with
// a typed "phone too old for syauth" reason that surfaces on the
// pairing screen.
package com.sy.syauth.android.pair.impl

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import androidx.annotation.RequiresApi
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.spec.ECGenParameterSpec

/** Android Keystore provider name. */
internal const val KEYSTORE_PROVIDER: String = "AndroidKeyStore"

/** Ed25519 named-curve descriptor per API 33's [NamedParameterSpec] contract. */
internal const val ED25519_CURVE_NAME: String = "Ed25519"

/** Length in bytes of the Ed25519 public key extracted from the certificate. */
public const val ED25519_PUBKEY_LEN: Int = 32

/**
 * Result of a successful keypair generation. The [alias] is what the
 * unlock path's [com.sy.syauth.android.approve.KeystoreFrameSigner]
 * uses to open the private key; [pubkey] is the 32-byte Ed25519
 * public key shipped to the desktop over the LESC link; [strongBoxBacked]
 * is the audit-log boolean indicating whether the key sits inside the
 * dedicated StrongBox secure element or in the regular TEE.
 */
public data class KeystoreEd25519KeyMaterial(
    val alias: String,
    val pubkey: ByteArray,
    val strongBoxBacked: Boolean,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is KeystoreEd25519KeyMaterial) return false
        return alias == other.alias &&
            pubkey.contentEquals(other.pubkey) &&
            strongBoxBacked == other.strongBoxBacked
    }

    override fun hashCode(): Int {
        var result = alias.hashCode()
        result = 31 * result + pubkey.contentHashCode()
        result = 31 * result + strongBoxBacked.hashCode()
        return result
    }
}

/** Typed failure surface for [KeystoreKeyGenerator]. */
public sealed class KeystoreKeygenError(message: String) : RuntimeException(message) {
    /** The runtime SDK is below API 33; Ed25519 in Keystore is unavailable. */
    public class UnsupportedApi(public val sdkInt: Int) :
        KeystoreKeygenError("Android Keystore Ed25519 requires API 33+; runtime is API $sdkInt")
    /** The alias is already taken; the caller must revoke or pick a fresh peer id. */
    public class AliasAlreadyExists(public val alias: String) :
        KeystoreKeygenError("Keystore alias '$alias' already in use; revoke before re-pair")
    /** Generic Keystore failure (provider, init, certificate retrieval). */
    public class CryptoFailure(message: String, cause: Throwable?) :
        KeystoreKeygenError(message) {
        init {
            initCause(cause)
        }
    }
}

/**
 * Contract for generating the pair-time Ed25519 keypair inside the
 * Android Keystore. Pulled out as an interface so the pair-flow
 * driver (`RealPairBackend`) can be unit-tested against a fake.
 */
public interface KeystoreKeyGenerator {
    /**
     * Generate (or open) the Ed25519 keypair under [alias]. Idempotent:
     * if the alias already resolves to a Keystore entry the call
     * returns [KeystoreKeygenError.AliasAlreadyExists] (the pair flow
     * surfaces this as "revoke before re-pair").
     */
    public fun generate(alias: String): KeystoreEd25519KeyMaterial
}

/**
 * Substring the StrongBox EC validator on Pixel / Galaxy SoCs surfaces
 * inside an `InvalidAlgorithmParameterException` when StrongBox lacks
 * an Ed25519 implementation (e.g. Galaxy S25 Ultra). The fallback path
 * matches case-insensitively on this substring before reissuing the
 * generator without StrongBox.
 */
internal const val STRONGBOX_EC_UNSUPPORTED_MARKER: String = "StrongBox"

/**
 * True iff [e]'s message points at the StrongBox-specific EC validator
 * surface. Top-level so a Robolectric unit test can pin the predicate
 * without standing up an `AndroidKeystoreKeyGenerator` instance.
 */
internal fun strongBoxEcUnsupportedMessage(e: java.security.InvalidAlgorithmParameterException): Boolean =
    (e.message ?: "").contains(STRONGBOX_EC_UNSUPPORTED_MARKER, ignoreCase = true)

/**
 * Build a fresh [KeyGenParameterSpec.Builder] preloaded with the
 * SPEC §3.2 D6 closure-condition flags:
 *
 *   - [KeyProperties.PURPOSE_SIGN] — the key may only sign, never decrypt
 *     or wrap (defense against an attacker who would otherwise use the
 *     keystore key for an unintended primitive).
 *   - [ECGenParameterSpec] with the `Ed25519` curve name (the Android
 *     Keystore EC validator rejects `NamedParameterSpec("Ed25519")` with
 *     `InvalidAlgorithmParameterException: EC may only use ECGenParameterSpec`).
 *   - [KeyProperties.DIGEST_NONE] — Ed25519 hashes the message internally,
 *     so the Keystore must NOT pre-hash.
 *   - `setUserAuthenticationRequired(true)` — the SPEC §3.2 D6 hardware
 *     gate; the key cannot sign without a fresh BiometricPrompt unlock.
 *   - `setUserAuthenticationParameters(0, AUTH_BIOMETRIC_STRONG)` — the
 *     SPEC §3 Scope item 20 + §3 Decisions row "Keystore auth window"
 *     contract: per-use validity (`0` seconds, no time window) AND
 *     Class-3 biometric only (no DEVICE_CREDENTIAL fallback). This is
 *     the SPEC §7 T-Relay defense: the human-tap latency on a Class-3
 *     fingerprint sensor dominates the relay RTT cap, and a 5-minute
 *     validity bucket would make every relayed sudo within the bucket
 *     free. The call is API 30+ (Android 11); the
 *     `@RequiresApi(TIRAMISU)` floor on [AndroidKeystoreKeyGenerator]
 *     already pins the runtime to API 33+ so the call is always
 *     resolvable.
 *
 * The caller chains `.setIsStrongBoxBacked(true|false).build()` to
 * select the StrongBox / TEE path. Top-level so a Robolectric unit
 * test can call it without standing up the production generator.
 */
internal fun buildEd25519SpecBuilder(alias: String): KeyGenParameterSpec.Builder =
    KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_SIGN)
        .setAlgorithmParameterSpec(ECGenParameterSpec(ED25519_CURVE_NAME))
        .setDigests(KeyProperties.DIGEST_NONE)
        .setUserAuthenticationRequired(true)
        .setUserAuthenticationParameters(
            KEYSTORE_AUTH_VALIDITY_PER_USE_SECONDS,
            KeyProperties.AUTH_BIOMETRIC_STRONG,
        )

/**
 * Per-use validity duration in seconds passed to
 * [KeyGenParameterSpec.Builder.setUserAuthenticationParameters].
 *
 * `0` means "the Keystore releases the private key for exactly one
 * signing operation per BiometricPrompt round". The SPEC §3 Decisions
 * row "Keystore auth window" forbids any non-zero validity window —
 * see SPEC §7 T-Relay for the rationale.
 */
internal const val KEYSTORE_AUTH_VALIDITY_PER_USE_SECONDS: Int = 0

/**
 * Production [KeystoreKeyGenerator] backed by [KeyPairGenerator] +
 * `AndroidKeyStore`. STRONGBOX-preferred: builds the spec with
 * `setIsStrongBoxBacked(true)` and falls back to non-STRONGBOX on
 * `StrongBoxUnavailableException`.
 */
@RequiresApi(Build.VERSION_CODES.TIRAMISU)
public class AndroidKeystoreKeyGenerator : KeystoreKeyGenerator {

    override fun generate(alias: String): KeystoreEd25519KeyMaterial {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
            throw KeystoreKeygenError.UnsupportedApi(Build.VERSION.SDK_INT)
        }
        val keystore = KeyStore.getInstance(KEYSTORE_PROVIDER).apply { load(null) }
        if (keystore.containsAlias(alias)) {
            // Idempotent re-pair: an existing entry under this alias
            // came from a prior successful generation on the same peer
            // MAC. The phone's KEY MATERIAL never appears outside the
            // Keystore, so the only stable way to recover the pubkey is
            // to load the existing certificate.
            val existingCert = keystore.getCertificate(alias)
                ?: throw KeystoreKeygenError.CryptoFailure("no certificate for existing alias '$alias'", null)
            return materialFromCertificate(alias, existingCert)
        }
        val generator = try {
            KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, KEYSTORE_PROVIDER)
        } catch (e: java.security.NoSuchAlgorithmException) {
            throw KeystoreKeygenError.CryptoFailure("KeyPairGenerator EC unavailable", e)
        } catch (e: java.security.NoSuchProviderException) {
            throw KeystoreKeygenError.CryptoFailure("AndroidKeyStore provider missing", e)
        }
        // StrongBox on many Pixel/Galaxy SoCs rejects Ed25519 with
        // `InvalidAlgorithmParameterException: Unsupported StrongBox EC:
        // Ed25519` rather than `StrongBoxUnavailableException`; retry
        // without StrongBox when the message points at that path.
        var strongBoxBacked = false
        try {
            val strongSpec = baseBuilder(alias).setIsStrongBoxBacked(true).build()
            try {
                generator.initialize(strongSpec)
                generator.generateKeyPair()
                strongBoxBacked = true
            } catch (e: java.security.InvalidAlgorithmParameterException) {
                if ((e.message ?: "").contains("StrongBox", ignoreCase = true)) {
                    val softSpec = baseBuilder(alias).setIsStrongBoxBacked(false).build()
                    generator.initialize(softSpec)
                    generator.generateKeyPair()
                } else {
                    throw e
                }
            } catch (_: StrongBoxUnavailableException) {
                val softSpec = baseBuilder(alias).setIsStrongBoxBacked(false).build()
                generator.initialize(softSpec)
                generator.generateKeyPair()
            }
        } catch (e: StrongBoxUnavailableException) {
            val softSpec = baseBuilder(alias).setIsStrongBoxBacked(false).build()
            try {
                generator.initialize(softSpec)
                generator.generateKeyPair()
            } catch (e2: java.security.GeneralSecurityException) {
                throw KeystoreKeygenError.CryptoFailure("generateKeyPair failed", e2)
            }
        } catch (e: java.security.GeneralSecurityException) {
            throw KeystoreKeygenError.CryptoFailure("generateKeyPair failed", e)
        }
        val cert = keystore.getCertificate(alias)
            ?: throw KeystoreKeygenError.CryptoFailure("no certificate emitted for alias '$alias'", null)
        val raw = cert.publicKey.encoded
            ?: throw KeystoreKeygenError.CryptoFailure("certificate publicKey has no encoded form", null)
        val pubkey = extractEd25519Pubkey(raw)
        return KeystoreEd25519KeyMaterial(
            alias = alias,
            pubkey = pubkey,
            strongBoxBacked = strongBoxBacked,
        )
    }

    /** Fresh [KeyGenParameterSpec.Builder] preloaded with the DEV-002 closure-condition flags. */
    internal fun baseBuilder(alias: String): KeyGenParameterSpec.Builder = buildEd25519SpecBuilder(alias)

    /**
     * True iff [e] is the message pattern the StrongBox EC validator on
     * Pixel / Galaxy SoCs raises when StrongBox lacks an Ed25519
     * implementation (instead of the StrongBoxUnavailableException
     * surface). Pulled out so the unit test can pin the predicate.
     */
    internal fun isStrongBoxEcUnsupported(e: java.security.InvalidAlgorithmParameterException): Boolean =
        strongBoxEcUnsupportedMessage(e)

    /**
     * Build the [KeystoreEd25519KeyMaterial] surface from an existing
     * Keystore [certificate]. Used by the idempotent re-pair branch
     * (when the alias already resolves). Exposed as `internal` so the
     * Robolectric unit test can drive this path without standing up a
     * real `AndroidKeyStore` provider — the provider is unavailable in
     * the Robolectric shadow.
     */
    internal fun materialFromCertificate(
        alias: String,
        certificate: java.security.cert.Certificate,
    ): KeystoreEd25519KeyMaterial {
        val raw = certificate.publicKey.encoded
            ?: throw KeystoreKeygenError.CryptoFailure(
                "existing certificate publicKey has no encoded form",
                null,
            )
        return KeystoreEd25519KeyMaterial(
            alias = alias,
            pubkey = extractEd25519Pubkey(raw),
            strongBoxBacked = false,
        )
    }

    /**
     * Extract the 32-byte raw Ed25519 public key from the X.509
     * `SubjectPublicKeyInfo` DER blob the Android Keystore emits in
     * [java.security.cert.Certificate.getPublicKey].`encoded`. The
     * DER structure is:
     *
     *     SubjectPublicKeyInfo ::= SEQUENCE { algorithm AlgorithmIdentifier, subjectPublicKey BIT STRING }
     *
     * The Ed25519 case is fixed-length (44 bytes total, 32-byte key
     * suffix); the slice extracts the trailing 32 bytes.
     */
    internal fun extractEd25519Pubkey(encoded: ByteArray): ByteArray {
        if (encoded.size < ED25519_PUBKEY_LEN) {
            throw KeystoreKeygenError.CryptoFailure(
                "encoded publicKey shorter than $ED25519_PUBKEY_LEN bytes (got ${encoded.size})",
                null,
            )
        }
        return encoded.copyOfRange(encoded.size - ED25519_PUBKEY_LEN, encoded.size)
    }
}
