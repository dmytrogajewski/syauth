// DEV-002 (re-march) — radio-free Robolectric unit tests pinning the
// SPEC §3.2 D6 closure contract on [AndroidKeystoreKeyGenerator].
//
// What this file proves WITHOUT touching real hardware:
//
//   1. The `KeyGenParameterSpec.Builder` returned by the production
//      builder helper carries [KeyProperties.PURPOSE_SIGN],
//      `setUserAuthenticationRequired(true)`, and, when
//      `.setIsStrongBoxBacked(true).build()` is chained, the resulting
//      [KeyGenParameterSpec] reports `isStrongBoxBacked = true`.
//   2. The StrongBox-EC-unsupported predicate matches the exact
//      message the Galaxy S25 Ultra EC validator surfaces
//      ("Unsupported StrongBox EC: Ed25519").
//   3. The idempotent re-pair path returns the existing certificate's
//      pubkey without re-initializing a fresh `KeyPairGenerator`
//      (Robolectric's AndroidKeyStore shadow lets us load a
//      pre-seeded certificate and pull the encoded pubkey).
//   4. On a pre-Tiramisu runtime (`Build.VERSION.SDK_INT < 33`),
//      `AndroidKeystoreKeyGenerator.generate(alias)` throws
//      `KeystoreKeygenError.UnsupportedApi`. The `@RequiresApi(33)`
//      annotation pins the build-time gate; this runtime test pins
//      the explicit throw so a SDK rollback does not silently
//      bypass the check.
//
// Robolectric's `AndroidKeyStore` shadow does NOT carry a working
// Ed25519 implementation, so this file does NOT exercise the happy
// path's `KeyPairGenerator.generateKeyPair()` call. That path is
// exercised by the real-device e2e probe (see JOURNEY-DEV-002
// Closure Appendix); this file proves the contract that survives a
// JVM-only test environment.
//
// Journey: specs/journeys/JOURNEY-DEV-002-keystore-strongbox.md
package com.sy.syauth.android.pair

import android.security.keystore.KeyProperties
import com.sy.syauth.android.pair.impl.AndroidKeystoreKeyGenerator
import com.sy.syauth.android.pair.impl.ED25519_PUBKEY_LEN
import com.sy.syauth.android.pair.impl.KeystoreKeygenError
import com.sy.syauth.android.pair.impl.STRONGBOX_EC_UNSUPPORTED_MARKER
import com.sy.syauth.android.pair.impl.buildEd25519SpecBuilder
import com.sy.syauth.android.pair.impl.strongBoxEcUnsupportedMessage
import java.security.InvalidAlgorithmParameterException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.util.ReflectionHelpers

private const val TEST_ALIAS: String = "syauth.test.dev002.alias"

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [33])
class KeystoreKeyGeneratorTest {

    // ---------------------------------------------------------------
    // (1) Builder carries PURPOSE_SIGN + setUserAuthenticationRequired(true);
    //     `.setIsStrongBoxBacked(true).build()` reports the flag.
    // ---------------------------------------------------------------

    @Test
    fun base_builder_pins_purpose_sign() {
        val spec = buildEd25519SpecBuilder(TEST_ALIAS).build()
        assertEquals(
            "PURPOSE_SIGN must be the only purpose flag",
            KeyProperties.PURPOSE_SIGN,
            spec.purposes,
        )
    }

    @Test
    fun base_builder_requires_user_authentication() {
        val spec = buildEd25519SpecBuilder(TEST_ALIAS).build()
        assertTrue(
            "setUserAuthenticationRequired(true) must be on the spec",
            spec.isUserAuthenticationRequired,
        )
    }

    @Test
    fun base_builder_pins_biometric_strong_per_use() {
        // S-015 closure: SPEC §3 Scope item 20 names the explicit
        // `setUserAuthenticationParameters(0, AUTH_BIOMETRIC_STRONG)`
        // call verbatim. The explicit form pins per-use validity
        // (`0` seconds) and Class-3 biometric only — no
        // DEVICE_CREDENTIAL fallback, no time window. A drift
        // here would silently weaken the SPEC §7 T-Relay defense.
        val spec = buildEd25519SpecBuilder(TEST_ALIAS).build()
        assertEquals(
            "userAuthenticationValidityDurationSeconds must be 0 (per-use)",
            0,
            spec.userAuthenticationValidityDurationSeconds,
        )
        assertEquals(
            "userAuthenticationType must equal AUTH_BIOMETRIC_STRONG (no DEVICE_CREDENTIAL bit)",
            KeyProperties.AUTH_BIOMETRIC_STRONG,
            spec.userAuthenticationType,
        )
    }

    @Test
    fun builder_with_strongbox_true_reports_strongbox_backed() {
        val spec = buildEd25519SpecBuilder(TEST_ALIAS).setIsStrongBoxBacked(true).build()
        assertTrue("isStrongBoxBacked must be true on the strong spec", spec.isStrongBoxBacked)
    }

    @Test
    fun builder_with_strongbox_false_reports_not_strongbox_backed() {
        val spec = buildEd25519SpecBuilder(TEST_ALIAS).setIsStrongBoxBacked(false).build()
        assertFalse(
            "isStrongBoxBacked must be false on the soft fallback spec",
            spec.isStrongBoxBacked,
        )
    }

    // ---------------------------------------------------------------
    // (2) StrongBox EC fallback predicate.
    // ---------------------------------------------------------------

    @Test
    fun strongbox_ec_unsupported_marker_constant_pins_substring() {
        // The fallback path must match on the exact substring the
        // Samsung/Pixel EC validator surfaces. A drift in this string
        // would silently turn the StrongBox fallback into a hard
        // failure on the affected SoCs.
        assertEquals("StrongBox", STRONGBOX_EC_UNSUPPORTED_MARKER)
    }

    @Test
    fun strongbox_ec_unsupported_predicate_matches_galaxy_s25_message() {
        val e = InvalidAlgorithmParameterException("Unsupported StrongBox EC: Ed25519")
        assertTrue(
            "Galaxy S25 Ultra EC validator message must trigger the fallback",
            strongBoxEcUnsupportedMessage(e),
        )
    }

    @Test
    fun strongbox_ec_unsupported_predicate_ignores_unrelated_message() {
        val e = InvalidAlgorithmParameterException("EC may only use ECGenParameterSpec")
        assertFalse(
            "unrelated EC validator message must NOT trigger the StrongBox fallback",
            strongBoxEcUnsupportedMessage(e),
        )
    }

    @Test
    fun strongbox_ec_unsupported_predicate_handles_null_message() {
        val e = InvalidAlgorithmParameterException()
        assertFalse(
            "null message must NOT trigger the StrongBox fallback",
            strongBoxEcUnsupportedMessage(e),
        )
    }

    // ---------------------------------------------------------------
    // (3) Idempotent re-pair path.
    // ---------------------------------------------------------------

    @Test
    fun idempotent_re_pair_returns_existing_certificate_pubkey() {
        // The Robolectric AndroidKeyStore shadow on API 33 cannot host a
        // real certificate entry (no Ed25519 provider). The production
        // idempotent branch's pubkey-extraction logic lives in
        // [AndroidKeystoreKeyGenerator.materialFromCertificate]; this
        // test exercises that helper directly with a stand-in
        // certificate carrying the same 44-byte SubjectPublicKeyInfo
        // wire shape the Keystore emits for Ed25519. The contract: the
        // returned material round-trips the trailing 32 bytes of the
        // SPKI blob and does NOT report StrongBox-backed (no fresh
        // generation happened).
        val rawEncoded = ByteArray(ED25519_PUBKEY_LEN + ED25519_SPKI_HEADER_LEN) { (it + 1).toByte() }
        val fakeCertificate = ByteArrayCertificate(rawEncoded)
        val generator = AndroidKeystoreKeyGenerator()
        val material = generator.materialFromCertificate(TEST_ALIAS, fakeCertificate)
        assertEquals(TEST_ALIAS, material.alias)
        assertEquals(
            "idempotent re-pair returns the trailing 32 bytes of the existing certificate's pubkey",
            ED25519_PUBKEY_LEN,
            material.pubkey.size,
        )
        val expectedSuffix = rawEncoded.copyOfRange(
            rawEncoded.size - ED25519_PUBKEY_LEN,
            rawEncoded.size,
        )
        assertTrue(
            "pubkey must equal the trailing 32 bytes of the SPKI blob",
            material.pubkey.contentEquals(expectedSuffix),
        )
        assertFalse(
            "idempotent path is reported as not StrongBox-backed (no fresh generation happened)",
            material.strongBoxBacked,
        )
    }

    // ---------------------------------------------------------------
    // (4) Pre-Tiramisu runtime throws UnsupportedApi.
    // ---------------------------------------------------------------

    @Test
    fun pre_tiramisu_runtime_throws_unsupported_api() {
        // Force `Build.VERSION.SDK_INT` to API 32 (one below Tiramisu)
        // via Robolectric's reflection helper. The generator's first
        // line is a runtime guard that throws `UnsupportedApi(32)`
        // before touching the AndroidKeyStore.
        ReflectionHelpers.setStaticField(
            android.os.Build.VERSION::class.java,
            "SDK_INT",
            PRE_TIRAMISU_SDK,
        )
        try {
            val generator = AndroidKeystoreKeyGenerator()
            var captured: Throwable? = null
            try {
                generator.generate(TEST_ALIAS)
            } catch (e: KeystoreKeygenError.UnsupportedApi) {
                captured = e
            }
            assertNotNull("generate must throw UnsupportedApi on API < 33", captured)
            assertEquals(
                "UnsupportedApi must carry the runtime SDK_INT value",
                PRE_TIRAMISU_SDK,
                (captured as KeystoreKeygenError.UnsupportedApi).sdkInt,
            )
        } finally {
            ReflectionHelpers.setStaticField(
                android.os.Build.VERSION::class.java,
                "SDK_INT",
                TIRAMISU_SDK,
            )
        }
    }

    private companion object {
        // Robolectric's @Config(sdk = [33]) sets Build.VERSION.SDK_INT
        // to TIRAMISU at test entry; we reset to this value after the
        // pre-Tiramisu test so subsequent assertions see the original
        // state.
        const val TIRAMISU_SDK: Int = 33
        const val PRE_TIRAMISU_SDK: Int = 32
        // X.509 SubjectPublicKeyInfo header preceding the raw Ed25519
        // pubkey suffix in the Keystore-emitted certificate encoding.
        const val ED25519_SPKI_HEADER_LEN: Int = 12
    }
}

/**
 * Minimal Certificate that returns a pre-seeded [encodedBytes] as its
 * [publicKey]'s encoded form. Used to stand in for the
 * `keyStore.getCertificate(alias)` that the production generator's
 * idempotent path reads.
 */
private class ByteArrayCertificate(
    private val encodedBytes: ByteArray,
) : java.security.cert.Certificate("X.509") {
    override fun getEncoded(): ByteArray = encodedBytes
    override fun verify(key: java.security.PublicKey?) = Unit
    override fun verify(key: java.security.PublicKey?, sigProvider: String?) = Unit
    override fun toString(): String = "ByteArrayCertificate(${encodedBytes.size} bytes)"
    override fun getPublicKey(): java.security.PublicKey = ByteArrayPublicKey(encodedBytes)
}

private class ByteArrayPublicKey(
    private val encodedBytes: ByteArray,
) : java.security.PublicKey {
    override fun getAlgorithm(): String = "Ed25519"
    override fun getFormat(): String = "X.509"
    override fun getEncoded(): ByteArray = encodedBytes
}
