// Roadmap item S-018 — Approve notification builder.
//
// On every valid challenge frame the service builds a Notification
// that:
//
//   1. Lives on a high-importance channel (`syauth.approve.channel`)
//      so it shows as a heads-up even when the phone is locked.
//   2. Carries the challenge bytes (base64-encoded), the hostname,
//      and the peerId as intent extras so MainActivity can route to
//      the Approve screen with the necessary payload.
//   3. Uses a deterministic notification id derived from the peerId
//      so repeated challenges from the same peer coalesce into a
//      single heads-up rather than spamming the shade.
//   4. Declares a full-screen intent for the locked-screen case (the
//      OS will only honor it when battery optimisation is excluded
//      and the device is locked — exactly our threat model).
//
// The function is a `object`-level singleton so the implementation
// can be replaced in tests by injecting a different `NotificationCompat`
// channel id — though the JVM Robolectric test exercises the real
// object directly.
package com.sy.syauth.android.bg

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.util.Base64
import androidx.core.app.NotificationCompat

/**
 * Notification channel id. Pinned constant per AGENTS.md "no magic
 * literals" rule; the same string is asserted by the unit test
 * `ApproveNotificationTest` so a future rename does not silently
 * break the channel ABI (Android keys channels by string id).
 */
public const val APPROVE_NOTIFICATION_CHANNEL_ID: String = "syauth.approve.channel"

/**
 * Human-readable channel name surfaced in the system Settings ->
 * Apps -> syauth -> Notifications UI.
 */
public const val APPROVE_NOTIFICATION_CHANNEL_NAME: String = "Approve unlock"

/**
 * Channel description shown under the name in the system UI.
 */
public const val APPROVE_NOTIFICATION_CHANNEL_DESCRIPTION: String =
    "Notifications that ask you to approve a desktop unlock."

/**
 * Intent action used for the approve deep-link. Mirrors
 * `Intent.ACTION_VIEW`; pulled out as a named value so the test
 * surface does not have to import `android.content.Intent`. `val`
 * (not `const val`) because `Intent.ACTION_VIEW` is a platform
 * static, not a Kotlin compile-time constant.
 */
public val APPROVE_INTENT_ACTION: String = Intent.ACTION_VIEW

/**
 * Intent data scheme. Lets the OS render the intent in tools like
 * adb dumpsys as a recognisable deep-link.
 */
public const val APPROVE_INTENT_SCHEME: String = "syauth"

/**
 * Intent data host. Same rationale.
 */
public const val APPROVE_INTENT_HOST: String = "approve"

// S-018 notification-deep-link intent extras (distinct from the
// S-014 activity-launch extras in `ChallengeApprovalActivity.kt` —
// the prefix disambiguates so a future contributor cannot conflate
// the deep-link payload with the activity-extras payload).
public const val APPROVE_EXTRA_CHALLENGE_B64: String = "syauth.extra.challenge_b64"
public const val APPROVE_EXTRA_HOSTNAME: String = "syauth.extra.hostname"
public const val APPROVE_EXTRA_PEER_ID: String = "syauth.extra.peer_id"

/**
 * The `PendingIntent` request code is keyed on the notification id
 * to keep per-peer intents distinct under the
 * `FLAG_UPDATE_CURRENT | FLAG_IMMUTABLE` semantics.
 */
private const val REQUEST_CODE_BASE: Int = 0x5A4A

/**
 * Base64 flags. `NO_WRAP` and `URL_SAFE` keep the encoded value
 * intent-extra friendly and round-trippable through ADB tooling.
 * `val` (not `const val`) because `Base64.NO_WRAP`/`URL_SAFE` are
 * Android platform statics, not Kotlin compile-time constants.
 */
internal val B64_FLAGS: Int = Base64.NO_WRAP or Base64.URL_SAFE

/**
 * Notification id type alias for readability. Notification ids are
 * 32-bit ints in the platform API.
 */
public typealias NotificationId = Int

public object ApproveNotification {
    /**
     * Create the notification channel if it does not already exist.
     * Safe to call repeatedly — `NotificationManager.createNotificationChannel`
     * is idempotent for the same channel id.
     */
    public fun ensureChannel(context: Context) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
            return
        }
        val channel = NotificationChannel(
            APPROVE_NOTIFICATION_CHANNEL_ID,
            APPROVE_NOTIFICATION_CHANNEL_NAME,
            NotificationManager.IMPORTANCE_HIGH,
        ).apply {
            description = APPROVE_NOTIFICATION_CHANNEL_DESCRIPTION
            setShowBadge(true)
        }
        val manager = context.getSystemService(NotificationManager::class.java)
        manager?.createNotificationChannel(channel)
    }

    /**
     * Show (or update) the approve notification for [peerId]. The
     * returned [NotificationId] is the platform id under which the
     * notification was posted.
     */
    public fun show(
        context: Context,
        challengeBytes: ByteArray,
        hostname: String,
        peerId: String,
    ): NotificationId {
        ensureChannel(context)
        val notificationId = peerId.hashCode()
        val intent = buildApproveIntent(context, challengeBytes, hostname, peerId)
        val pending = PendingIntent.getActivity(
            context,
            REQUEST_CODE_BASE xor notificationId,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = build(context, hostname, pending)
        val manager = context.getSystemService(NotificationManager::class.java)
        manager?.notify(notificationId, notification)
        return notificationId
    }

    /**
     * Build the intent that MainActivity will react to. Exposed so
     * unit tests can inspect the extras without going through the
     * `PendingIntent` round-trip (which mutates extras on some API
     * levels).
     */
    public fun buildApproveIntent(
        context: Context,
        challengeBytes: ByteArray,
        hostname: String,
        peerId: String,
    ): Intent {
        val encoded = Base64.encodeToString(challengeBytes, B64_FLAGS)
        return Intent(APPROVE_INTENT_ACTION).apply {
            setClassName(context, "com.sy.syauth.android.MainActivity")
            data = Uri.fromParts(APPROVE_INTENT_SCHEME, APPROVE_INTENT_HOST, peerId)
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            addFlags(Intent.FLAG_RECEIVER_FOREGROUND)
            putExtra(APPROVE_EXTRA_CHALLENGE_B64, encoded)
            putExtra(APPROVE_EXTRA_HOSTNAME, hostname)
            putExtra(APPROVE_EXTRA_PEER_ID, peerId)
        }
    }

    private fun build(
        context: Context,
        hostname: String,
        contentIntent: PendingIntent,
    ): Notification {
        return NotificationCompat.Builder(context, APPROVE_NOTIFICATION_CHANNEL_ID)
            .setContentTitle("Approve unlock for $hostname?")
            .setContentText("Tap to review and approve.")
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setCategory(NotificationCompat.CATEGORY_CALL)
            .setSmallIcon(android.R.drawable.ic_lock_lock)
            .setContentIntent(contentIntent)
            .setFullScreenIntent(contentIntent, true)
            .setAutoCancel(true)
            .build()
    }
}
