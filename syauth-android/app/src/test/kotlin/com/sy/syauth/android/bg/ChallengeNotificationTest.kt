// Journey: specs/journeys/JOURNEY-S-018-phone-notification-history.md
//
// Robolectric tests for the audit-history notification dispatcher.
// Pins both DoD bullets:
//   * `posts_per_challenge`           — one dispatch → one heads-up.
//   * `rate_limited_to_one_per_five_seconds`
//                                     — three dispatches within
//                                       window → two visible, three
//                                       audit rows.
package com.sy.syauth.android.bg

import android.app.NotificationManager
import android.content.Context
import androidx.test.core.app.ApplicationProvider
import com.sy.syauth.android.history.ChallengeHistoryDao
import com.sy.syauth.android.history.HISTORY_DISPLAY_LIMIT
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.time.Clock
import java.time.Instant
import java.time.ZoneOffset

private const val FIXTURE_HOSTNAME: String = "alex-desktop"
private const val FIXTURE_PEER_ID: String = "AA:BB:CC:DD:EE:FF"
private const val FIXTURE_PEER_ID_SHORT: String = "DD:EE:FF"

private class MutableClock(initialInstant: Instant) : Clock() {
    private var current: Instant = initialInstant
    override fun instant(): Instant = current
    override fun getZone(): java.time.ZoneId = ZoneOffset.UTC
    override fun withZone(zone: java.time.ZoneId?): Clock = this
    override fun millis(): Long = current.toEpochMilli()
    fun advance(durationMs: Long) {
        current = current.plusMillis(durationMs)
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class ChallengeNotificationTest {

    @After
    fun tearDown() {
        val ctx: Context = ApplicationProvider.getApplicationContext()
        val nm = ctx.getSystemService(NotificationManager::class.java)
        nm?.cancelAll()
    }

    private fun freshDao(ctx: Context): ChallengeHistoryDao =
        ChallengeHistoryDao(filesDir = ctx.filesDir).also {
            // Wipe any prior test residue so `recent` is deterministic.
            ctx.filesDir.listFiles()?.forEach { f -> f.delete() }
        }

    @Test
    fun posts_per_challenge() {
        val ctx: Context = ApplicationProvider.getApplicationContext()
        val dao = freshDao(ctx)
        val clock = MutableClock(Instant.ofEpochMilli(1_700_000_000_000L))
        val dispatcher = ChallengeNotificationDispatcher(
            context = ctx,
            dao = dao,
            clock = clock,
        )

        dispatcher.dispatch(
            hostname = FIXTURE_HOSTNAME,
            peerId = FIXTURE_PEER_ID,
            outcome = HISTORY_OUTCOME_GRANTED,
        )

        val nm = ctx.getSystemService(NotificationManager::class.java)
        val active = nm.activeNotifications
        assertEquals(1, active.size)
        val n = active[0].notification
        val text = "${n.extras.getCharSequence(android.app.Notification.EXTRA_TITLE)} " +
            "${n.extras.getCharSequence(android.app.Notification.EXTRA_TEXT)}"
        assertTrue("hostname not surfaced: $text", text.contains(FIXTURE_HOSTNAME))
        assertTrue("short peer id not surfaced: $text", text.contains(FIXTURE_PEER_ID_SHORT))
        assertTrue("outcome not surfaced: $text", text.contains(HISTORY_OUTCOME_GRANTED))
        // Channel pinned.
        assertEquals(NOTIFICATION_CHANNEL_HISTORY, active[0].notification.channelId)
        // Audit row persisted.
        assertEquals(1, dao.recent(HISTORY_DISPLAY_LIMIT).size)
    }

    @Test
    fun rate_limited_to_one_per_five_seconds() {
        val ctx: Context = ApplicationProvider.getApplicationContext()
        val dao = freshDao(ctx)
        val clock = MutableClock(Instant.ofEpochMilli(1_700_000_000_000L))
        val dispatcher = ChallengeNotificationDispatcher(
            context = ctx,
            dao = dao,
            clock = clock,
        )

        dispatcher.dispatch(FIXTURE_HOSTNAME, FIXTURE_PEER_ID, HISTORY_OUTCOME_GRANTED)
        clock.advance(4_000L)
        dispatcher.dispatch(FIXTURE_HOSTNAME, FIXTURE_PEER_ID, HISTORY_OUTCOME_DENIED)
        clock.advance(2_000L) // total 6 s from first dispatch
        dispatcher.dispatch(FIXTURE_HOSTNAME, FIXTURE_PEER_ID, HISTORY_OUTCOME_GRANTED)

        val nm = ctx.getSystemService(NotificationManager::class.java)
        assertEquals(
            "second call within 5-s window must be suppressed",
            2,
            nm.activeNotifications.size,
        )
        assertEquals(
            "history must record all three regardless of rate limit",
            3,
            dao.recent(HISTORY_DISPLAY_LIMIT).size,
        )
    }
}
