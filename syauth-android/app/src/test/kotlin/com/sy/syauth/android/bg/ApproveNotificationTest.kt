// Roadmap item S-018 — Robolectric tests for the approve
// notification builder. Asserts the channel-creation contract
// (id, importance, name) and the intent-payload contract (the three
// extras the MainActivity reads).
//
// Why Robolectric: NotificationCompat.Builder + the
// NotificationManager system service are platform classes the JVM
// alone cannot satisfy. `@Config(sdk = [34])` pins the framework
// version to the project's compileSdk.
package com.sy.syauth.android.bg

import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.util.Base64
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

private const val TEST_PEER_ID: String = "AA:BB:CC:DD:EE:FF"
private const val TEST_HOSTNAME: String = "alex-desktop"
private const val CHALLENGE_LEN: Int = 64
private val TEST_CHALLENGE: ByteArray = ByteArray(CHALLENGE_LEN) { it.toByte() }

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class ApproveNotificationTest {

    private fun ctx(): Context = ApplicationProvider.getApplicationContext()

    @Test
    fun ensure_channel_creates_high_importance_channel_with_pinned_id() {
        val context = ctx()
        ApproveNotification.ensureChannel(context)

        val nm = context.getSystemService(NotificationManager::class.java)
        val channel = nm.getNotificationChannel(APPROVE_NOTIFICATION_CHANNEL_ID)
        assertNotNull("channel must exist after ensureChannel", channel)
        assertEquals(NotificationManager.IMPORTANCE_HIGH, channel.importance)
        assertEquals(APPROVE_NOTIFICATION_CHANNEL_NAME, channel.name.toString())
    }

    @Test
    fun ensure_channel_is_idempotent() {
        val context = ctx()
        ApproveNotification.ensureChannel(context)
        ApproveNotification.ensureChannel(context)
        val nm = context.getSystemService(NotificationManager::class.java)
        // Robolectric does not expose `getNotificationChannels().size`
        // stably across versions, but the contract we care about is
        // that the second call did not throw and the channel still
        // exists with the right importance.
        val channel = nm.getNotificationChannel(APPROVE_NOTIFICATION_CHANNEL_ID)
        assertEquals(NotificationManager.IMPORTANCE_HIGH, channel.importance)
    }

    @Test
    fun build_approve_intent_carries_all_three_extras_and_action_view() {
        val context = ctx()
        val intent = ApproveNotification.buildApproveIntent(
            context = context,
            challengeBytes = TEST_CHALLENGE,
            hostname = TEST_HOSTNAME,
            peerId = TEST_PEER_ID,
        )

        assertEquals(Intent.ACTION_VIEW, intent.action)
        val b64 = intent.getStringExtra(EXTRA_CHALLENGE_B64)
        assertNotNull("EXTRA_CHALLENGE_B64 missing", b64)
        val decoded = Base64.decode(b64, B64_FLAGS)
        assertTrue("challenge must round-trip", decoded.contentEquals(TEST_CHALLENGE))
        assertEquals(TEST_HOSTNAME, intent.getStringExtra(EXTRA_HOSTNAME))
        assertEquals(TEST_PEER_ID, intent.getStringExtra(EXTRA_PEER_ID))
    }

    @Test
    fun show_posts_notification_with_deterministic_id_per_peer() {
        val context = ctx()
        val id1 = ApproveNotification.show(
            context = context,
            challengeBytes = TEST_CHALLENGE,
            hostname = TEST_HOSTNAME,
            peerId = TEST_PEER_ID,
        )
        val id2 = ApproveNotification.show(
            context = context,
            challengeBytes = TEST_CHALLENGE,
            hostname = TEST_HOSTNAME,
            peerId = TEST_PEER_ID,
        )
        assertEquals("same peer must coalesce", id1, id2)
    }

    @Test
    fun show_different_peers_yield_different_notification_ids() {
        val context = ctx()
        val idA = ApproveNotification.show(
            context = context,
            challengeBytes = TEST_CHALLENGE,
            hostname = TEST_HOSTNAME,
            peerId = "AA:AA:AA:AA:AA:AA",
        )
        val idB = ApproveNotification.show(
            context = context,
            challengeBytes = TEST_CHALLENGE,
            hostname = TEST_HOSTNAME,
            peerId = "BB:BB:BB:BB:BB:BB",
        )
        assertTrue(
            "distinct peers must not collide (got idA=$idA idB=$idB)",
            idA != idB,
        )
    }
}
