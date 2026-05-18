// Roadmap item S-018 — phone-side challenge-history notification.
//
// SPEC §3 scope item #23 (verbatim):
//
//   > Phone `SyauthCompanionService` writes a notification per
//   > challenge (suppressed if the operator dismisses; rate-limited
//   > to 1 per 5 s).
//
// This file holds the audit-history notification dispatcher. It is
// distinct from `ApproveNotification.kt` (the high-importance,
// full-screen-intent prompt that asks the operator to approve a
// fresh sudo) — the audit channel is low-importance and never
// interrupts the user; it only reports what already happened so the
// operator can audit it after the fact.
//
// Journey: specs/journeys/JOURNEY-S-018-phone-notification-history.md.
package com.sy.syauth.android.bg

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.util.Log
import androidx.core.app.NotificationCompat
import com.sy.syauth.android.history.ChallengeHistoryDao
import com.sy.syauth.android.history.ChallengeHistoryRecord
import java.time.Clock
import java.time.Duration
import java.util.UUID
import java.util.concurrent.atomic.AtomicLong

/**
 * Audit-history channel id. Pinned per SPEC §3 scope item #23 and the
 * task spec's named-constants list.
 */
public const val NOTIFICATION_CHANNEL_HISTORY: String = "syauth-challenge-history"

/** Human-readable channel name surfaced in `Settings → Apps → syauth → Notifications`. */
public const val NOTIFICATION_CHANNEL_HISTORY_NAME: String = "syauth challenge history"

/**
 * Channel description. Tells the operator the channel is the audit
 * surface and safe to mute — the unlock prompts use a separate,
 * high-importance channel.
 */
public const val NOTIFICATION_CHANNEL_HISTORY_DESCRIPTION: String =
    "Post-transaction audit log of every sudo challenge your desktop sent."

/**
 * Per-SPEC rate-limit window between visible audit posts. Audit
 * appends (DAO inserts) are NEVER rate-limited — only the visible
 * post is. SPEC text: "rate-limited to 1 per 5 s".
 */
public val NOTIFICATION_RATE_LIMIT: Duration = Duration.ofSeconds(5)

/** Outcome label vocabulary (matches the desktop's `last.log`). */
public const val HISTORY_OUTCOME_GRANTED: String = "granted"

/** See [HISTORY_OUTCOME_GRANTED]. */
public const val HISTORY_OUTCOME_DENIED: String = "denied"

/** See [HISTORY_OUTCOME_GRANTED]. */
public const val HISTORY_OUTCOME_TIMED_OUT: String = "timed-out"

/** Deep-link intent action used by the notification's content intent. */
public val HISTORY_ROUTE_INTENT_ACTION: String = Intent.ACTION_VIEW

/** Deep-link scheme; same scheme as the approve deep-link. */
public const val HISTORY_ROUTE_INTENT_SCHEME: String = "syauth"

/** Deep-link host; distinct from the approve deep-link's host (`approve`). */
public const val HISTORY_ROUTE_INTENT_HOST: String = "history"

/** Notification icon. Reused from the foreground service for visual continuity. */
internal val HISTORY_NOTIFICATION_ICON: Int = android.R.drawable.ic_lock_lock

/** Log tag for the dispatcher. */
internal const val HISTORY_LOG_TAG: String = "syauth.history"

/** Short-peer-id length (last N hex chars of the MAC, dashes preserved). */
internal const val HISTORY_SHORT_PEER_ID_LEN: Int = 8

/**
 * Return the short form of [peerId]. Mirrors the activity-side
 * formatter so the prompt copy and the audit row carry the same
 * short id.
 */
internal fun peerIdShort(peerId: String): String =
    if (peerId.length <= HISTORY_SHORT_PEER_ID_LEN) {
        peerId
    } else {
        peerId.substring(peerId.length - HISTORY_SHORT_PEER_ID_LEN)
    }

/**
 * Counter that mints unique notification ids per dispatch so the OS
 * does not collapse separate audit entries into one heads-up.
 */
private val historyNotificationIdCounter: AtomicLong = AtomicLong(System.currentTimeMillis())

private fun nextHistoryNotificationId(): Int =
    historyNotificationIdCounter.incrementAndGet().toInt()

/**
 * Build the deep-link intent the audit notification carries. Exposed
 * for tests that prefer to inspect extras directly.
 */
public fun buildHistoryDeepLinkIntent(context: Context): Intent =
    Intent(HISTORY_ROUTE_INTENT_ACTION).apply {
        setClassName(context, "com.sy.syauth.android.MainActivity")
        data = Uri.fromParts(HISTORY_ROUTE_INTENT_SCHEME, HISTORY_ROUTE_INTENT_HOST, null)
        addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    }

/**
 * Dispatcher invoked AFTER `PersistentGattClient.writeResponse`
 * returns (success or denied). Appends one audit row to [dao] and,
 * subject to the [NOTIFICATION_RATE_LIMIT] gate, posts one
 * low-priority notification on [NOTIFICATION_CHANNEL_HISTORY].
 *
 * The [clock] seam exists for tests; production wires
 * `Clock.systemUTC()`.
 */
public class ChallengeNotificationDispatcher(
    private val context: Context,
    private val dao: ChallengeHistoryDao,
    private val clock: Clock,
) {
    private val lastPostMs: AtomicLong = AtomicLong(Long.MIN_VALUE)

    /** Public for production callers that wire `Clock.systemUTC()`. */
    public constructor(context: Context, dao: ChallengeHistoryDao) :
        this(context = context, dao = dao, clock = Clock.systemUTC())

    /**
     * Append one audit row for the resolved transaction, then post
     * (or suppress) the visible notification.
     */
    public fun dispatch(hostname: String, peerId: String, outcome: String) {
        val now = clock.millis()
        val record = ChallengeHistoryRecord(
            id = UUID.randomUUID().toString(),
            peerId = peerId,
            peerIdShort = peerIdShort(peerId),
            hostname = hostname,
            outcome = outcome,
            timestampMs = now,
        )
        dao.insert(record)
        Log.i(HISTORY_LOG_TAG, "history record id=${record.id} outcome=$outcome")
        if (shouldSuppressPost(now)) {
            Log.i(HISTORY_LOG_TAG, "post suppressed (rate limit)")
            return
        }
        ensureChannel(context)
        val notification = buildNotification(context, record)
        val nm = context.getSystemService(NotificationManager::class.java) ?: return
        val notificationId = nextHistoryNotificationId()
        nm.notify(notificationId, notification)
        lastPostMs.set(now)
    }

    private fun shouldSuppressPost(now: Long): Boolean {
        val last = lastPostMs.get()
        if (last == Long.MIN_VALUE) return false
        return (now - last) < NOTIFICATION_RATE_LIMIT.toMillis()
    }
}

/**
 * Create the audit channel if it does not already exist. Idempotent
 * — `NotificationManager.createNotificationChannel` re-uses the
 * existing channel for the same id.
 */
public fun ensureChannel(context: Context) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
    val nm = context.getSystemService(NotificationManager::class.java) ?: return
    val existing = nm.getNotificationChannel(NOTIFICATION_CHANNEL_HISTORY)
    if (existing != null) return
    val channel = NotificationChannel(
        NOTIFICATION_CHANNEL_HISTORY,
        NOTIFICATION_CHANNEL_HISTORY_NAME,
        NotificationManager.IMPORTANCE_LOW,
    ).apply {
        description = NOTIFICATION_CHANNEL_HISTORY_DESCRIPTION
        setShowBadge(false)
    }
    nm.createNotificationChannel(channel)
}

private fun buildNotification(
    context: Context,
    record: ChallengeHistoryRecord,
): Notification {
    val intent = buildHistoryDeepLinkIntent(context)
    val pending = PendingIntent.getActivity(
        context,
        record.id.hashCode(),
        intent,
        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )
    val title = "syauth: ${record.outcome} by ${record.hostname}"
    val text = "peer ${record.peerIdShort}"
    return NotificationCompat.Builder(context, NOTIFICATION_CHANNEL_HISTORY)
        .setContentTitle(title)
        .setContentText(text)
        .setSmallIcon(HISTORY_NOTIFICATION_ICON)
        .setPriority(NotificationCompat.PRIORITY_LOW)
        .setContentIntent(pending)
        .setAutoCancel(true)
        .setOnlyAlertOnce(true)
        .build()
}
