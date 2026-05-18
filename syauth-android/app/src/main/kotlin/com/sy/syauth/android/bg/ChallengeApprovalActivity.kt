// Roadmap item S-014 — `ChallengeApprovalActivity` (transparent,
// over-keyguard). Roadmap item S-015 — BiometricPrompt
// (`BIOMETRIC_STRONG`) + Keystore Ed25519 sign on Approve.
//
// `SyauthCompanionService` launches this activity via
// `PendingIntent.getActivity` on every fresh challenge frame the
// `PersistentGattClient.onChallenge` callback delivers. The activity
// is `noHistory="true"`, `launchMode="singleInstance"`, and
// declares `android:showWhenLocked="true"` / `android:turnScreenOn="true"`
// in `AndroidManifest.xml`; the OS therefore wakes the screen and
// renders the activity over the keyguard.
//
// The prompt copy is the SPEC §9 Q2 answer verbatim:
//
//     "$hostname is requesting sudo (peer_id $short)"
//
// where `$short` is the last three octets of the peer's MAC. The
// hostname comes from `EXTRA_HOSTNAME` (which the service populates
// from the bond record's `hostName` — never from the incoming frame),
// pinning the SPEC §9 Q2 guarantee at the activity boundary.
//
// On Cancel, the activity writes a denied frame back through the
// same GATT connection by handing the bytes to the
// `SyauthCompanionService` companion seam [CancelSink]; production
// wires the sink to `PersistentGattClient.writeResponse(...)`.
//
// On Approve (S-015), the activity hands the bond's
// `EXTRA_KEYSTORE_ALIAS` and the verbatim `EXTRA_CHALLENGE_BYTES` to
// the [BiometricGate] companion seam. The production
// implementation builds a `BiometricPrompt` whose
// `PromptInfo.allowedAuthenticators = BIOMETRIC_STRONG` (no
// DEVICE_CREDENTIAL fallback), opens the Keystore-resident Ed25519
// PrivateKey under the alias, wraps it in a `Signature` +
// `BiometricPrompt.CryptoObject`, runs the prompt, and — on
// `onAuthenticationSucceeded` — calls
// `sig.update(challengeBytes); sig.sign()` to produce the 64-byte
// response payload. The bytes flow to the [ResponseSink] companion
// seam; production wires it to
// `PersistentGattClient.writeResponse(...)` on the same connection
// the challenge arrived on. On biometric fail / cancel, the activity
// writes `DENIED_FRAME_BYTES` via the response sink. SPEC §3.2 D6
// per-use auth + §7 T-Relay defense pin this contract.
//
// The denied-frame wire shape is the SPEC v1 frame layout
// (`[version:1] || [nonce:16] || [signature:64] || [tag:16]`) with
// the signature payload filled with `DENIED_FRAME_BYTES` (64 zero
// bytes). The daemon's `verify_response` rejects with `BadSignature`
// → maps to `PAM_AUTH_ERR`, which is the right end-state for an
// explicit user denial (the alternative — sending nothing — would
// map to `PAM_AUTHINFO_UNAVAIL`, a fall-through, which is wrong for
// a user "no").
//
// Journey:
// - `specs/journeys/JOURNEY-S-014-challenge-approval-activity.md`
//   (activity lifecycle + Cancel path).
// - `specs/journeys/JOURNEY-S-015-biometric-keystore-sign.md`
//   (BiometricPrompt + Keystore sign).
package com.sy.syauth.android.bg

import android.app.Activity
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.util.Log
import androidx.activity.compose.setContent
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import com.sy.syauth.android.R
import com.sy.syauth.android.ui.theme.SyauthTheme
import java.security.KeyStore
import java.security.PrivateKey
import java.security.Signature

/** Intent extra carrying the bonded peer's stable id (the MAC). */
public const val EXTRA_PEER_ID: String = "syauth.peerId"

/** Intent extra carrying the bond's hostname, read from `BondRecord.hostName`. */
public const val EXTRA_HOSTNAME: String = "syauth.hostname"

/** Intent extra carrying the verbatim challenge frame body bytes the daemon notified. */
public const val EXTRA_CHALLENGE_BYTES: String = "syauth.challengeBytes"

/**
 * Intent extra carrying the bond's Keystore alias for the per-bond
 * Ed25519 private key minted at pair time by
 * [com.sy.syauth.android.pair.impl.AndroidKeystoreKeyGenerator]
 * (DEV-002). S-015 reads this on Approve to open the
 * `PrivateKey` from the AndroidKeyStore.
 */
public const val EXTRA_KEYSTORE_ALIAS: String = "syauth.keystoreAlias"

/**
 * Logcat tag the activity emits under. Pinned constant so
 * `adb logcat -s syauth.bg.approve` is one grep away.
 */
internal const val APPROVAL_LOG_TAG: String = "syauth.bg.approve"

/**
 * Reason byte name surfaced in journey docs. Wired to the signature
 * payload below; kept as a named constant so the audit-trail link
 * "denied frame == 64 zero bytes" is a one-grep contract.
 */
public const val DENIED_FRAME_REASON: String = "denied"

/** Length in bytes of an Ed25519 signature; matches `syauth-core::sign::SIGNATURE_LEN`. */
internal const val SIGNATURE_LEN: Int = 64

/** Number of trailing peer-id chars rendered as `$short` in the SPEC §9 Q2 prompt. */
internal const val SHORT_PEER_ID_LEN: Int = 8

/**
 * SPEC §3.2 D6 + §7 T-Relay defense: the BiometricPrompt's allowed
 * authenticator bitmask. `BIOMETRIC_STRONG` ONLY — no
 * `DEVICE_CREDENTIAL` fallback. The SPEC §3 Decisions row
 * "Keystore auth window" forbids PIN/pattern/password because they
 * are weaker than Class-3 biometric and do not solve the
 * relay-tap-latency story.
 */
public const val STRONG_AUTHENTICATOR: Int =
    BiometricManager.Authenticators.BIOMETRIC_STRONG

/**
 * JCA signature algorithm name for the bond's Ed25519 private key.
 * The same string the host JVM (`SunEC` since JDK 15) and the
 * Android Keystore (`AndroidKeyStore` since API 33) both resolve.
 */
internal const val ED25519_ALGORITHM: String = "Ed25519"

/** Android Keystore provider name. */
internal const val KEYSTORE_PROVIDER: String = "AndroidKeyStore"

/** Resource id of the BiometricPrompt title string. */
public val PROMPT_TITLE_RES: Int = R.string.syauth_biometric_prompt_title

/** Resource id of the BiometricPrompt subtitle format string (`%1$s`=hostname, `%2$s`=short peer id). */
public val PROMPT_SUBTITLE_FMT: Int = R.string.syauth_biometric_prompt_subtitle_fmt

/** Resource id of the BiometricPrompt negative-button text. */
public val PROMPT_NEGATIVE_RES: Int = R.string.syauth_biometric_prompt_cancel

/**
 * Signature payload the activity writes back as the denied frame.
 *
 * The daemon's `verify_response` then rejects with `BadSignature`
 * → maps to `PAM_AUTH_ERR`. See the file header for the rationale
 * versus "send nothing" (`PAM_AUTHINFO_UNAVAIL`).
 */
public val DENIED_FRAME_BYTES: ByteArray = ByteArray(SIGNATURE_LEN) { 0 }

/**
 * Sink the activity calls on Cancel. Production wires it to a service-side
 * helper that resolves the per-peer `PersistentGattClient` and calls
 * `writeResponse(deniedFrameBytes)`; tests inject a recording fake.
 */
public fun interface CancelSink {
    public fun onCancel(peerId: String, deniedFrameBytes: ByteArray)
}

/**
 * Sink the activity calls with the **approve** response — either the
 * 64-byte Ed25519 signature on success, or [DENIED_FRAME_BYTES] on
 * biometric fail / cancel. Production wires it to a service-side
 * helper that resolves the per-peer `PersistentGattClient` and
 * calls `writeResponse(responseBytes)` on the same GATT connection
 * the challenge arrived on; tests inject a recording fake.
 *
 * The separate seam (vs. reusing [CancelSink]) names the
 * approve-side semantics explicitly: the production wiring uses the
 * same `writeResponse` plumbing, but the audit trail distinguishes a
 * signed response from a denied one via the calling sink.
 */
public fun interface ResponseSink {
    public fun onResponse(peerId: String, responseBytes: ByteArray)
}

/**
 * Callback the [BiometricGate] invokes with the terminal outcome of
 * the BiometricPrompt round. Implementations MUST invoke exactly
 * one of [onSucceeded] / [onFailed] per `authenticate(...)` call.
 */
public interface BiometricGateCallback {
    /**
     * BiometricPrompt succeeded; [signatureBytes] is the 64-byte
     * Ed25519 signature over the challenge body the gate signed
     * with the Keystore-resident private key.
     */
    public fun onSucceeded(signatureBytes: ByteArray)

    /**
     * BiometricPrompt was cancelled, errored, or never opened
     * (e.g. no enrolled fingerprint). [reason] is the human-
     * readable cause for the logcat audit trail.
     */
    public fun onFailed(reason: String)
}

/**
 * Test seam wrapping the [BiometricPrompt] + Keystore-sign
 * lifecycle. In production [AndroidBiometricGate] builds a real
 * `BiometricPrompt` whose
 * `PromptInfo.allowedAuthenticators == BIOMETRIC_STRONG`, wraps the
 * Keystore-resident Ed25519 `PrivateKey` under [keystoreAlias] in a
 * `Signature.getInstance("Ed25519").apply { initSign(privateKey) }`,
 * passes that as `BiometricPrompt.CryptoObject(signature)`, and on
 * success calls `sig.update(challengeBytes); sig.sign()` to produce
 * the response payload.
 *
 * In tests a recording fake exposes `succeed(signatureBytes)` /
 * `fail(reason)` methods that drive the activity lifecycle without
 * firing a real prompt — the only way to exercise the activity
 * lifecycle on a Robolectric JVM.
 */
public interface BiometricGate {
    /**
     * Open a fresh BiometricPrompt round bound to a per-use
     * Keystore signing operation. MUST invoke exactly one callback
     * method per call; MUST NOT throw — every failure surface is a
     * typed [BiometricGateCallback.onFailed] call.
     */
    public fun authenticate(
        keystoreAlias: String,
        challengeBytes: ByteArray,
        callback: BiometricGateCallback,
    )
}

/**
 * Sign the [challengeBytes] under [privateKey] using
 * `Signature.getInstance("Ed25519")`. Returns the 64-byte
 * Ed25519 signature.
 *
 * The signed input is **exactly** [challengeBytes] — the daemon's
 * `verify_response` runs `verify_frame(pubkey, frame, sig)` which
 * verifies under `frame.body_bytes() = version || nonce || payload`
 * (see `crates/syauth-core/src/sign.rs::sign_frame`). The caller
 * (the [BiometricGate]) MUST pass the body bytes the daemon
 * notified, NOT the full encoded frame with the trailing tag.
 *
 * Top-level so the [KeystoreSignTest] can inject a JVM-generated
 * Ed25519 `PrivateKey` (the Robolectric AndroidKeyStore shadow does
 * not host Ed25519; the host JVM does since JDK 15) without
 * standing up an AndroidKeyStore alias.
 */
public fun signChallenge(privateKey: PrivateKey, challengeBytes: ByteArray): ByteArray {
    val signature = Signature.getInstance(ED25519_ALGORITHM)
    signature.initSign(privateKey)
    signature.update(challengeBytes)
    return signature.sign()
}

/**
 * Transparent, over-keyguard activity launched by
 * `SyauthCompanionService` on every fresh challenge frame.
 *
 * Extends `FragmentActivity` because `androidx.biometric:1.2.0-alpha05`'s
 * `BiometricPrompt` binds to the host fragment manager for its
 * dialog lifecycle; a `ComponentActivity` parent class would
 * compile but fail at runtime when the prompt tries to attach.
 */
public class ChallengeApprovalActivity : FragmentActivity() {

    /**
     * Latest `setShowWhenLocked` value the activity passed to the OS.
     * Robolectric 4.11.1's `ShadowActivity` does not expose
     * `isShowWhenLocked()` directly, so the DoD test reads this
     * package-internal field instead.
     */
    internal var lastShowWhenLockedFlag: Boolean = false
        private set

    /** Mirror of [lastShowWhenLockedFlag] for `setTurnScreenOn`. */
    internal var lastTurnScreenOnFlag: Boolean = false
        private set

    /**
     * Latest prompt-text string the Compose tree rendered. Held so the
     * DoD test `hostname_shown_in_prompt` can pin the SPEC §9 Q2 copy
     * without driving Compose's UI-test harness (which would require
     * `ui-test-junit4` on the JVM classpath).
     */
    internal var lastPromptText: String = ""
        private set

    private var resolvedPeerId: String = ""
    private var resolvedHostname: String = ""
    private var resolvedChallenge: ByteArray = ByteArray(0)
    private var resolvedKeystoreAlias: String = ""

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
            setShowWhenLocked(true)
            setTurnScreenOn(true)
            lastShowWhenLockedFlag = true
            lastTurnScreenOnFlag = true
        }
        val peerId = intent?.getStringExtra(EXTRA_PEER_ID).orEmpty()
        val hostname = intent?.getStringExtra(EXTRA_HOSTNAME).orEmpty()
        if (peerId.isEmpty() || hostname.isEmpty()) {
            Log.w(APPROVAL_LOG_TAG, "missing extras peer='$peerId' host='$hostname'; finishing")
            finish()
            return
        }
        resolvedPeerId = peerId
        resolvedHostname = hostname
        resolvedChallenge = intent?.getByteArrayExtra(EXTRA_CHALLENGE_BYTES) ?: ByteArray(0)
        resolvedKeystoreAlias = intent?.getStringExtra(EXTRA_KEYSTORE_ALIAS).orEmpty()
        val short = shortPeerId(peerId)
        val promptText = "$hostname is requesting sudo (peer_id $short)"
        lastPromptText = promptText
        Log.i(APPROVAL_LOG_TAG, "render peer=$peerId host=$hostname")
        setContent { ApprovalContent(promptText = promptText, onApprove = ::onApproveClicked, onCancel = ::onCancelClicked) }
    }

    /** Test seam: invoked by the Compose Cancel button. */
    internal fun onCancelClicked() {
        Log.i(APPROVAL_LOG_TAG, "cancel peer=$resolvedPeerId reason=$DENIED_FRAME_REASON")
        cancelSink?.onCancel(resolvedPeerId, DENIED_FRAME_BYTES)
        finish()
    }

    /**
     * Test seam: invoked by the Compose Approve button. S-015 wires
     * the [BiometricGate] + Keystore-sign + response-write path.
     * Each invocation produces exactly one BiometricPrompt round
     * (per-use Keystore key contract per SPEC §3.2 D6).
     */
    internal fun onApproveClicked() {
        Log.i(APPROVAL_LOG_TAG, "approve peer=$resolvedPeerId alias=$resolvedKeystoreAlias")
        // Test override on the companion seam takes precedence over
        // the per-instance production gate so a Robolectric JVM test
        // can drive succeed() / fail() without firing a real prompt.
        val gate = biometricGate ?: AndroidBiometricGate(
            activity = this,
            peerId = resolvedPeerId,
            hostname = resolvedHostname,
        )
        gate.authenticate(
            resolvedKeystoreAlias,
            resolvedChallenge,
            object : BiometricGateCallback {
                override fun onSucceeded(signatureBytes: ByteArray) {
                    Log.i(APPROVAL_LOG_TAG, "approve sig ok peer=$resolvedPeerId len=${signatureBytes.size}")
                    writeResponseAndFinish(signatureBytes)
                }

                override fun onFailed(reason: String) {
                    Log.i(APPROVAL_LOG_TAG, "approve fail peer=$resolvedPeerId reason=$reason")
                    writeResponseAndFinish(DENIED_FRAME_BYTES)
                }
            },
        )
    }

    private fun writeResponseAndFinish(responseBytes: ByteArray) {
        responseSink?.onResponse(resolvedPeerId, responseBytes)
        if (!isFinishing) {
            finish()
        }
    }

    /**
     * Test helper exposing the constructed
     * `BiometricPrompt.PromptInfo` so the DoD assertion
     * `BiometricPromptTest::strong_authenticator_required` can pin
     * `allowedAuthenticators == BIOMETRIC_STRONG` without firing a
     * real prompt. Package-internal so production callers cannot
     * observe the prompt info.
     */
    internal fun buildPromptInfoForTest(): BiometricPrompt.PromptInfo =
        buildPromptInfo(this, resolvedHostname, shortPeerId(resolvedPeerId))

    public companion object {
        /**
         * Sink the activity calls on Cancel. Production sets this from
         * `MainActivity.installCompanionSeams`; tests inject a recording
         * fake. `@Volatile` because the activity (main thread) reads and
         * the service-side installer (binder thread) writes.
         */
        @Volatile
        public var cancelSink: CancelSink? = null

        /**
         * Sink the activity calls with the approve-side response
         * (signed bytes on success, [DENIED_FRAME_BYTES] on fail).
         * Production wires it to the same per-peer
         * `PersistentGattClient.writeResponse(...)` plumbing as
         * [cancelSink].
         */
        @Volatile
        public var responseSink: ResponseSink? = null

        /**
         * [BiometricGate] the activity routes Approve through.
         * Production sets this to [AndroidBiometricGate] in
         * `MainActivity.installCompanionSeams`; tests inject a
         * recording fake that drives `succeed(...)` / `fail(...)`
         * manually.
         */
        @Volatile
        public var biometricGate: BiometricGate? = null

        /** Reset all seams to `null`. Used by Robolectric tests. */
        public fun resetSeams() {
            cancelSink = null
            responseSink = null
            biometricGate = null
        }
    }
}

private fun shortPeerId(peerId: String): String =
    if (peerId.length <= SHORT_PEER_ID_LEN) peerId else peerId.substring(peerId.length - SHORT_PEER_ID_LEN)

/**
 * Build the `BiometricPrompt.PromptInfo` the activity hands to the
 * production [AndroidBiometricGate]. Pulled out as a top-level
 * helper so the unit test can pin
 * `allowedAuthenticators == BIOMETRIC_STRONG` without standing up a
 * real prompt.
 */
internal fun buildPromptInfo(
    activity: Activity,
    hostname: String,
    shortPeerId: String,
): BiometricPrompt.PromptInfo {
    val title = activity.getString(PROMPT_TITLE_RES)
    val subtitle = activity.getString(PROMPT_SUBTITLE_FMT, hostname, shortPeerId)
    val negative = activity.getString(PROMPT_NEGATIVE_RES)
    return BiometricPrompt.PromptInfo.Builder()
        .setTitle(title)
        .setSubtitle(subtitle)
        .setAllowedAuthenticators(STRONG_AUTHENTICATOR)
        .setNegativeButtonText(negative)
        .setConfirmationRequired(false)
        .build()
}

/**
 * Production [BiometricGate] backed by
 * `androidx.biometric:1.2.0-alpha05`'s `BiometricPrompt` and the
 * AndroidKeyStore-resident Ed25519 private key minted at pair time
 * by [com.sy.syauth.android.pair.impl.AndroidKeystoreKeyGenerator]
 * (DEV-002).
 *
 * Each `authenticate(...)` call:
 *
 *   1. Opens the `PrivateKey` from `AndroidKeyStore` under
 *      [keystoreAlias].
 *   2. Builds a `Signature.getInstance("Ed25519")` and calls
 *      `initSign(privateKey)`.
 *   3. Wraps the `Signature` in a `BiometricPrompt.CryptoObject` so
 *      the OS gates the `sign()` call on a fresh biometric tap (the
 *      Keystore key was generated with
 *      `setUserAuthenticationParameters(0, AUTH_BIOMETRIC_STRONG)`).
 *   4. Calls `prompt.authenticate(promptInfo, cryptoObject)`.
 *   5. On `onAuthenticationSucceeded`, calls
 *      `sig.update(challengeBytes); sig.sign()` and reports the
 *      bytes via `callback.onSucceeded(signatureBytes)`.
 *   6. On any error (cancel, lockout, hardware unavailable),
 *      reports via `callback.onFailed(reason)`.
 *
 * The class is `internal` because the only construction site is
 * the production wiring in `MainActivity.installCompanionSeams`;
 * tests use a recording fake `BiometricGate` instead.
 */
internal class AndroidBiometricGate(
    private val activity: FragmentActivity,
    private val peerId: String,
    private val hostname: String,
) : BiometricGate {

    override fun authenticate(
        keystoreAlias: String,
        challengeBytes: ByteArray,
        callback: BiometricGateCallback,
    ) {
        val privateKey = openPrivateKey(keystoreAlias)
        if (privateKey == null) {
            callback.onFailed("no PrivateKey under alias '$keystoreAlias'")
            return
        }
        val signature = Signature.getInstance(ED25519_ALGORITHM).apply { initSign(privateKey) }
        val executor = ContextCompat.getMainExecutor(activity)
        val authCallback = object : BiometricPrompt.AuthenticationCallback() {
            override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                val sig = result.cryptoObject?.signature
                if (sig == null) {
                    callback.onFailed("crypto object missing signature")
                    return
                }
                val bytes = runCatching {
                    sig.update(challengeBytes)
                    sig.sign()
                }.getOrElse {
                    callback.onFailed("sign failed: ${it.message}")
                    return
                }
                callback.onSucceeded(bytes)
            }

            override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                callback.onFailed("[$errorCode] $errString")
            }

            override fun onAuthenticationFailed() {
                // Soft failure (bad fingerprint). The OS keeps the
                // prompt up; we do NOT route this to onFailed so the
                // user can retry until cancel/lockout fires
                // onAuthenticationError.
                Log.i(APPROVAL_LOG_TAG, "biometric soft-failed peer=$peerId; user can retry")
            }
        }
        val prompt = BiometricPrompt(activity, executor, authCallback)
        val promptInfo = buildPromptInfo(activity, hostname, shortPeerIdInternal(peerId))
        prompt.authenticate(promptInfo, BiometricPrompt.CryptoObject(signature))
    }

    private fun openPrivateKey(alias: String): PrivateKey? {
        if (alias.isEmpty()) {
            return null
        }
        val keystore = runCatching {
            KeyStore.getInstance(KEYSTORE_PROVIDER).apply { load(null) }
        }.getOrNull() ?: return null
        if (!keystore.containsAlias(alias)) {
            return null
        }
        return keystore.getKey(alias, null) as? PrivateKey
    }

    private fun shortPeerIdInternal(peerId: String): String = shortPeerId(peerId)
}

/** Title surfaced in the auth-screen TopAppBar; centralised so a future copy edit lands in one place. */
private const val APPROVAL_SCREEN_TITLE: String = "syauth"

/** Outer screen padding. Matches `approve.ApproveScreen.SCREEN_PADDING_DP`. */
private val APPROVAL_PADDING_DP = 24.dp

/** Vertical gap between sections (icon → prompt → buttons). */
private val APPROVAL_SECTION_SPACING_DP = 16.dp

/** Vertical gap between the Approve and Cancel buttons. */
private val APPROVAL_BUTTON_SPACING_DP = 12.dp

/** Lock-icon diameter at the top of the screen. */
private val APPROVAL_ICON_SIZE_DP = 72.dp

/** Primary-button height; matches prrr-android's Connect button (56.dp). */
private val APPROVAL_BUTTON_HEIGHT_DP = 56.dp

/** Horizontal inset around the full-width Approve / Cancel buttons. */
private val APPROVAL_BUTTON_HORIZONTAL_PADDING_DP = 24.dp

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ApprovalContent(promptText: String, onApprove: () -> Unit, onCancel: () -> Unit) {
    SyauthTheme {
        Scaffold(
            topBar = {
                TopAppBar(
                    title = { Text(APPROVAL_SCREEN_TITLE) },
                    colors = TopAppBarDefaults.topAppBarColors(
                        containerColor = MaterialTheme.colorScheme.background,
                        titleContentColor = MaterialTheme.colorScheme.onBackground,
                    ),
                )
            },
            containerColor = MaterialTheme.colorScheme.background,
        ) { paddingValues ->
            Surface(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(paddingValues),
                color = MaterialTheme.colorScheme.background,
            ) {
                Column(
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(horizontal = APPROVAL_PADDING_DP),
                    verticalArrangement = Arrangement.Top,
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    Spacer(modifier = Modifier.height(APPROVAL_SECTION_SPACING_DP))
                    Icon(
                        imageVector = Icons.Filled.Lock,
                        contentDescription = null,
                        modifier = Modifier.size(APPROVAL_ICON_SIZE_DP),
                        tint = MaterialTheme.colorScheme.primary,
                    )
                    Spacer(modifier = Modifier.height(APPROVAL_SECTION_SPACING_DP))
                    Text(
                        text = promptText,
                        style = MaterialTheme.typography.titleMedium,
                        color = MaterialTheme.colorScheme.onBackground,
                        textAlign = TextAlign.Center,
                    )
                    Spacer(modifier = Modifier.weight(1f))
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(horizontal = APPROVAL_BUTTON_HORIZONTAL_PADDING_DP),
                        horizontalAlignment = Alignment.CenterHorizontally,
                    ) {
                        Button(
                            onClick = onApprove,
                            modifier = Modifier
                                .fillMaxWidth()
                                .height(APPROVAL_BUTTON_HEIGHT_DP),
                            colors = ButtonDefaults.buttonColors(
                                containerColor = MaterialTheme.colorScheme.primary,
                                contentColor = MaterialTheme.colorScheme.onPrimary,
                            ),
                        ) {
                            Text(
                                text = "Approve",
                                style = MaterialTheme.typography.titleMedium,
                            )
                        }
                        Spacer(modifier = Modifier.height(APPROVAL_BUTTON_SPACING_DP))
                        OutlinedButton(
                            onClick = onCancel,
                            modifier = Modifier
                                .fillMaxWidth()
                                .height(APPROVAL_BUTTON_HEIGHT_DP),
                            colors = ButtonDefaults.outlinedButtonColors(
                                contentColor = MaterialTheme.colorScheme.error,
                            ),
                        ) {
                            Text(
                                text = "Cancel",
                                style = MaterialTheme.typography.titleMedium,
                            )
                        }
                    }
                    Spacer(modifier = Modifier.height(APPROVAL_SECTION_SPACING_DP))
                }
            }
        }
    }
}

/**
 * Build a fresh [Intent] that the production
 * `SyauthCompanionService.launchApprovalActivity` (and the future
 * direct caller) uses to start the activity with all the extras
 * S-015 needs. Centralised here so the test harness and the
 * production launch site share one builder.
 */
internal fun buildApprovalIntent(
    context: android.content.Context,
    peerId: String,
    hostname: String,
    challengeBytes: ByteArray,
    keystoreAlias: String,
): Intent = Intent(context, ChallengeApprovalActivity::class.java).apply {
    addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    putExtra(EXTRA_PEER_ID, peerId)
    putExtra(EXTRA_HOSTNAME, hostname)
    putExtra(EXTRA_CHALLENGE_BYTES, challengeBytes)
    putExtra(EXTRA_KEYSTORE_ALIAS, keystoreAlias)
}
