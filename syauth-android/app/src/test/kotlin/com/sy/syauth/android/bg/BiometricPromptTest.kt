// Roadmap item S-015 — Robolectric JVM tests for the
// BiometricPrompt + Keystore-sign path on
// `ChallengeApprovalActivity`. Pins the DoD bullets from
// `specs/unlock-proximity/ROADMAP.md` Step S-015 verbatim:
//
//   1. `strong_authenticator_required` — the constructed
//      `BiometricPrompt.PromptInfo.allowedAuthenticators` equals
//      `BiometricManager.Authenticators.BIOMETRIC_STRONG` and
//      carries no DEVICE_CREDENTIAL bit.
//   2. `per_use_keystore_unlock` — a second Approve tap requires
//      a second `BiometricGate.authenticate(...)` call (the
//      Keystore key was released for exactly one use per
//      BiometricPrompt round; no cached signature reuse).
//   3. `cancel_writes_denied` — when the injected gate's
//      `fail(reason)` callback fires, the response sink receives
//      one call with bytes equal to `DENIED_FRAME_BYTES` and the
//      activity is `finishing`.
//
// The injected `BiometricGate` is the test seam the production
// flow goes through. In production the gate wraps the real
// `androidx.biometric.BiometricPrompt`; in tests a recording fake
// drives `succeed(signatureBytes)` / `fail(reason)` manually.
//
// Journey: specs/journeys/JOURNEY-S-015-biometric-keystore-sign.md
package com.sy.syauth.android.bg

import android.content.Intent
import androidx.biometric.BiometricManager
import androidx.test.core.app.ApplicationProvider
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

private const val FIXTURE_HOSTNAME: String = "alex-desktop"
private const val FIXTURE_PEER_ID: String = "AA:BB:CC:DD:EE:FF"
private const val FIXTURE_KEYSTORE_ALIAS: String = "syauth.test.s015.alias"
private const val FIXTURE_CHALLENGE_LEN: Int = 49
private val FIXTURE_SIGNATURE: ByteArray = ByteArray(SIGNATURE_LEN) { (it + 7).toByte() }
private val SECOND_FIXTURE_SIGNATURE: ByteArray = ByteArray(SIGNATURE_LEN) { (it + 13).toByte() }

private class RecordingResponseSink : ResponseSink {
    val calls: MutableList<Pair<String, ByteArray>> = mutableListOf()
    override fun onResponse(peerId: String, responseBytes: ByteArray) {
        calls += peerId to responseBytes
    }
}

private class RecordingBiometricGate : BiometricGate {
    var lastChallenge: ByteArray = ByteArray(0)
        private set
    var lastCallback: BiometricGateCallback? = null
        private set
    var callCount: Int = 0
        private set

    override fun authenticate(
        keystoreAlias: String,
        challengeBytes: ByteArray,
        callback: BiometricGateCallback,
    ) {
        callCount += 1
        lastChallenge = challengeBytes
        lastCallback = callback
    }

    fun succeed(signatureBytes: ByteArray) {
        val cb = lastCallback ?: error("gate.authenticate was not called")
        cb.onSucceeded(signatureBytes)
    }

    fun fail(reason: String) {
        val cb = lastCallback ?: error("gate.authenticate was not called")
        cb.onFailed(reason)
    }
}

private fun fixtureIntent(): Intent {
    val context = ApplicationProvider.getApplicationContext<android.content.Context>()
    return Intent(context, ChallengeApprovalActivity::class.java).apply {
        putExtra(EXTRA_PEER_ID, FIXTURE_PEER_ID)
        putExtra(EXTRA_HOSTNAME, FIXTURE_HOSTNAME)
        putExtra(EXTRA_CHALLENGE_BYTES, ByteArray(FIXTURE_CHALLENGE_LEN) { it.toByte() })
        putExtra(EXTRA_KEYSTORE_ALIAS, FIXTURE_KEYSTORE_ALIAS)
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class BiometricPromptTest {

    @After
    fun cleanup() {
        ChallengeApprovalActivity.resetSeams()
    }

    @Test
    fun strong_authenticator_required() {
        val controller = Robolectric.buildActivity(
            ChallengeApprovalActivity::class.java,
            fixtureIntent(),
        ).create().start().resume()
        val activity = controller.get()

        val promptInfo = activity.buildPromptInfoForTest()

        assertNotNull("PromptInfo must be non-null", promptInfo)
        assertEquals(
            "allowedAuthenticators must equal BIOMETRIC_STRONG only",
            BiometricManager.Authenticators.BIOMETRIC_STRONG,
            promptInfo.allowedAuthenticators,
        )
    }

    @Test
    fun per_use_keystore_unlock() {
        val gate = RecordingBiometricGate()
        val sink = RecordingResponseSink()
        ChallengeApprovalActivity.biometricGate = gate
        ChallengeApprovalActivity.responseSink = sink
        val controller = Robolectric.buildActivity(
            ChallengeApprovalActivity::class.java,
            fixtureIntent(),
        ).create().start().resume()
        val activity = controller.get()

        activity.onApproveClicked()
        assertEquals("first Approve invokes gate exactly once", 1, gate.callCount)
        gate.succeed(FIXTURE_SIGNATURE)

        // The activity should finish after the first sign;
        // a second Approve tap on the same activity would normally
        // be a no-op because `isFinishing == true`. Per-use
        // semantics: simulate a fresh activity round and assert a
        // second authenticate call is required.
        ChallengeApprovalActivity.biometricGate = gate
        val secondController = Robolectric.buildActivity(
            ChallengeApprovalActivity::class.java,
            fixtureIntent(),
        ).create().start().resume()
        val secondActivity = secondController.get()
        secondActivity.onApproveClicked()
        gate.succeed(SECOND_FIXTURE_SIGNATURE)

        assertEquals("each Approve round invokes gate exactly once", 2, gate.callCount)
        assertEquals(2, sink.calls.size)
        assertArrayEquals(
            "first response carries the first signature",
            FIXTURE_SIGNATURE,
            sink.calls[0].second,
        )
        assertArrayEquals(
            "second response carries the second signature",
            SECOND_FIXTURE_SIGNATURE,
            sink.calls[1].second,
        )
    }

    @Test
    fun cancel_writes_denied() {
        val gate = RecordingBiometricGate()
        val sink = RecordingResponseSink()
        ChallengeApprovalActivity.biometricGate = gate
        ChallengeApprovalActivity.responseSink = sink
        val controller = Robolectric.buildActivity(
            ChallengeApprovalActivity::class.java,
            fixtureIntent(),
        ).create().start().resume()
        val activity = controller.get()

        activity.onApproveClicked()
        gate.fail("user cancel")

        assertEquals("fail path writes exactly one response", 1, sink.calls.size)
        assertEquals(FIXTURE_PEER_ID, sink.calls[0].first)
        assertArrayEquals(
            "fail path writes DENIED_FRAME_BYTES",
            DENIED_FRAME_BYTES,
            sink.calls[0].second,
        )
        assertTrue("activity is finishing after biometric fail", activity.isFinishing)
    }
}
