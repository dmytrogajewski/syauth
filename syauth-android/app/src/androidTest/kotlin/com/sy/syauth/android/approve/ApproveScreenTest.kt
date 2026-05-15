// Roadmap item S-017 — Compose UI test for ApproveScreen.
//
// Requires connected device / emulator; CI is gated via androidTest source set.
//
// The test verifies the screen renders:
//   - The hostname header (containing "Approve unlock for <hostname>?").
//   - An Approve button (test tag ApproveScreenTestTags.APPROVE_BUTTON).
//   - A Deny button (test tag ApproveScreenTestTags.DENY_BUTTON).
//   - A countdown line (test tag ApproveScreenTestTags.COUNTDOWN).
//
// The ViewModel is built with fakes that mirror the unit-test rig so
// the Compose layer is exercised in isolation from real Android crypto.
// See `app/src/test/.../ApproveViewModelTest.kt` for the same fakes
// used in JVM unit tests.
package com.sy.syauth.android.approve

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import java.security.KeyPairGenerator
import java.security.Signature
import java.security.spec.ECGenParameterSpec
import org.junit.Rule
import org.junit.Test

class ApproveScreenTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Test
    fun renders_hostname_buttons_and_countdown() {
        val viewModel = ApproveViewModel(
            hostname = TEST_HOSTNAME,
            challengeFrame = ByteArray(CHALLENGE_LEN) { it.toByte() },
            keystoreSigner = TestKeystoreSigner(),
            biometricPresenter = NoOpBiometricPresenter,
            signingKeyProvider = InMemorySigningKeyProvider(ByteArray(SEED_LEN) { 0x01 }),
            wireSigner = NoOpWireSigner,
            responseSender = NoOpResponseSender,
            timeoutMillis = LONG_TIMEOUT_MILLIS,
            tickMillis = LONG_TICK_MILLIS,
            keystoreAlias = TEST_ALIAS,
        )

        composeTestRule.setContent {
            ApproveScreen(viewModel = viewModel)
        }

        composeTestRule
            .onNodeWithText(TEST_HOSTNAME, substring = true)
            .assertIsDisplayed()

        composeTestRule
            .onNodeWithTag(ApproveScreenTestTags.HOSTNAME)
            .assertIsDisplayed()

        composeTestRule
            .onNodeWithTag(ApproveScreenTestTags.APPROVE_BUTTON)
            .assertIsDisplayed()
            .assertIsEnabled()

        composeTestRule
            .onNodeWithTag(ApproveScreenTestTags.DENY_BUTTON)
            .assertIsDisplayed()
            .assertIsEnabled()

        composeTestRule
            .onNodeWithTag(ApproveScreenTestTags.COUNTDOWN)
            .assertIsDisplayed()
    }

    private companion object {
        const val TEST_HOSTNAME: String = "dell-precision"
        const val TEST_ALIAS: String = "test.alias"
        const val SEED_LEN: Int = 32
        const val CHALLENGE_LEN: Int = 48
        const val LONG_TIMEOUT_MILLIS: Long = 60_000L
        const val LONG_TICK_MILLIS: Long = 1_000L
    }
}

private class TestKeystoreSigner : KeystoreSignerBackend {
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

private object NoOpBiometricPresenter : BiometricPresenter {
    override suspend fun authenticate(signature: Signature): BiometricResult =
        BiometricResult.Failed("test-no-op")
}

private object NoOpWireSigner : WireSigner {
    override suspend fun signWire(seed: ByteArray, frameBytes: ByteArray): WireSignResult =
        WireSignResult.Failure("test-no-op")
}

private object NoOpResponseSender : ResponseSender {
    override suspend fun sendApprove(responseFrame: ByteArray) = Unit
    override suspend fun sendDeny() = Unit
}
