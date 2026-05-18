// Roadmap item S-015 — JVM-side test pinning the `signChallenge`
// helper's sign-input convention and the SPEC §3 Scope item 20
// "the response frame is the Ed25519 signature over the challenge
// bytes" contract.
//
// The test deliberately bypasses the AndroidKeyStore shadow.
// Robolectric's `AndroidKeyStore` provider does NOT carry a
// working Ed25519 implementation (see KeystoreFrameSignerTest's
// header for the gory details), but the host JVM (OpenJDK 17+)
// does — Ed25519 has been in `SunEC` since JDK 15. The test
// generates a fresh Ed25519 keypair via the JVM's
// `KeyPairGenerator.getInstance("Ed25519")`, injects the resulting
// `PrivateKey` into the production `signChallenge(privateKey,
// challengeBytes)` helper, then verifies the returned signature
// against the matching `PublicKey` via
// `Signature.getInstance("Ed25519").apply { initVerify(pubkey);
// update(challengeBytes); verify(signatureBytes) }`.
//
// The signed message is exactly the challenge body bytes the
// daemon notified — `version(1) || nonce(16) || payload(challenge)`,
// matching `syauth-core::frame::Frame::body_bytes`. The Frame's
// trailing 16-byte tag is NOT in the signed input. See SPEC §3
// Scope item 20 and `crates/syauth-core/src/sign.rs::sign_frame`.
//
// Journey: specs/journeys/JOURNEY-S-015-biometric-keystore-sign.md
package com.sy.syauth.android.bg

import java.security.KeyPairGenerator
import java.security.Signature
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

private const val ED25519_ALGORITHM: String = "Ed25519"
private const val FIXTURE_CHALLENGE_LEN: Int = 49

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class KeystoreSignTest {

    @Test
    fun signs_challenge_with_bond_key() {
        // Mint a fresh Ed25519 keypair on the host JVM (the
        // AndroidKeyStore shadow lacks Ed25519; the host JVM has
        // it). The production signChallenge helper takes a
        // `PrivateKey` so the test can inject the JVM key
        // without standing up a Keystore alias.
        val keypair = KeyPairGenerator.getInstance(ED25519_ALGORITHM).generateKeyPair()
        val challenge = ByteArray(FIXTURE_CHALLENGE_LEN) { (it + 1).toByte() }

        val signature = signChallenge(keypair.private, challenge)

        assertEquals(
            "signChallenge must return SIGNATURE_LEN bytes",
            SIGNATURE_LEN,
            signature.size,
        )
        val verifier = Signature.getInstance(ED25519_ALGORITHM).apply {
            initVerify(keypair.public)
            update(challenge)
        }
        assertTrue(
            "the signature must verify under the matching public key over the same challenge bytes",
            verifier.verify(signature),
        )
    }
}
