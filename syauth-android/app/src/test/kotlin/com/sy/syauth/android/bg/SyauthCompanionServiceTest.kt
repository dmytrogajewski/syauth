// Roadmap item S-011 — Robolectric JVM tests for the foreground
// `SyauthCompanionService`. Pins the DoD bullets from
// `specs/unlock-proximity/ROADMAP.md` Step S-011 verbatim:
//
//   1. `starts_foreground_with_connected_device_type` — boots the
//      service via `Robolectric.buildService(...).create()` and
//      asserts the captured `lastForegroundType` field equals
//      `ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE`, the
//      shadow's `lastForegroundNotification` is non-null, and the
//      notification's channel id equals
//      `NOTIFICATION_CHANNEL_ID`. Robolectric 4.11.1's
//      `ShadowService` does not expose `getForegroundServiceType()`
//      directly, so we read the package-internal recording field the
//      service writes inside `startForeground`.
//   2. `injects_one_gatt_client_per_bond` — pre-seeds three bond
//      records via a `BondListProvider` seam and asserts the
//      recording `GattClientFactory` saw three `create(record)`
//      invocations whose peer ids match the fixtures.
//   3. `stops_clients_on_destroy` — drives `.destroy()` and asserts
//      every recording client saw exactly one `stop()` call.
//
// Journey: specs/journeys/JOURNEY-S-011-service-foreground-lifecycle.md
package com.sy.syauth.android.bg

import android.content.pm.ServiceInfo
import com.sy.syauth.android.bond.BondRecord
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

private const val FIXTURE_BOND_KEY_LEN: Int = 32
private const val FIXTURE_HOST: String = "alex-desktop"
private const val FIXTURE_PEER_A: String = "AA:AA:AA:AA:AA:AA"
private const val FIXTURE_PEER_B: String = "BB:BB:BB:BB:BB:BB"
private const val FIXTURE_PEER_C: String = "CC:CC:CC:CC:CC:CC"
private const val FIXTURE_KEYSTORE_ALIAS: String = "syauth.test.alias"

private fun bondFor(peerId: String): BondRecord = BondRecord(
    peerId = peerId,
    hostName = FIXTURE_HOST,
    bondKey = ByteArray(FIXTURE_BOND_KEY_LEN) { it.toByte() },
    keystoreAlias = FIXTURE_KEYSTORE_ALIAS,
    phonePubkey = ByteArray(FIXTURE_BOND_KEY_LEN) { it.toByte() },
)

private class RecordingManagedClient : ManagedClient {
    var stopCalls: Int = 0
        private set
    var startCalls: Int = 0
        private set
    override fun start() {
        startCalls += 1
    }
    override fun stop() {
        stopCalls += 1
    }
}

private class RecordingGattClientFactory : GattClientFactory {
    val created: MutableList<Pair<String, RecordingManagedClient>> = mutableListOf()
    override fun create(bond: BondRecord): ManagedClient {
        val client = RecordingManagedClient()
        created += bond.peerId to client
        return client
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class SyauthCompanionServiceTest {

    @After
    fun cleanup() {
        SyauthCompanionService.resetSeams()
        ChallengeApprovalActivity.resetSeams()
    }

    @Test
    fun starts_foreground_with_connected_device_type() {
        SyauthCompanionService.bondListProvider = BondListProvider { emptyList() }
        SyauthCompanionService.gattClientFactory = RecordingGattClientFactory()

        val controller = Robolectric.buildService(SyauthCompanionService::class.java).create()
        val service = controller.get()

        assertNotNull("service created", service)
        assertEquals(
            ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE,
            service.lastForegroundType,
        )
        val notification = shadowOf(service).lastForegroundNotification
        assertNotNull("foreground notification posted", notification)
        assertEquals(NOTIFICATION_CHANNEL_ID, notification.channelId)
    }

    @Test
    fun injects_one_gatt_client_per_bond() {
        val bonds = listOf(
            bondFor(FIXTURE_PEER_A),
            bondFor(FIXTURE_PEER_B),
            bondFor(FIXTURE_PEER_C),
        )
        val factory = RecordingGattClientFactory()
        SyauthCompanionService.bondListProvider = BondListProvider { bonds }
        SyauthCompanionService.gattClientFactory = factory

        Robolectric.buildService(SyauthCompanionService::class.java).create()

        assertEquals(3, factory.created.size)
        assertEquals(FIXTURE_PEER_A, factory.created[0].first)
        assertEquals(FIXTURE_PEER_B, factory.created[1].first)
        assertEquals(FIXTURE_PEER_C, factory.created[2].first)
        for ((_, client) in factory.created) {
            assertTrue("client started", client.startCalls >= 1)
        }
    }

    /**
     * BUG-20260522-0130: when Android kills the app process under memory
     * pressure and `START_STICKY` brings `SyauthCompanionService` back
     * without going through `MainActivity`, the JVM-static
     * [SyauthCompanionService.gattClientFactory] is `null`. The previous
     * `injectClientsForBonds()` early-returned in that case, leaving
     * the resurrected service alive (foreground notification visible,
     * `isRunning=true`) but with `clients = []` — every challenge
     * from the desktop instant-failed `transport-error` until the user
     * manually opened the app UI. Regression guard: `onCreate` must
     * install a default factory itself so a process-restarted service
     * is self-sufficient.
     */
    @Test
    fun on_create_installs_default_gatt_client_factory_when_none_preset() {
        // Empty bond list keeps the test JVM-pure — we're asserting the
        // factory got installed, not exercising connectGatt. The
        // factory's `create` lambda is never invoked here.
        SyauthCompanionService.bondListProvider = BondListProvider { emptyList() }
        // gattClientFactory intentionally left null — simulates the
        // post-process-restart cold-start.

        Robolectric.buildService(SyauthCompanionService::class.java).create()

        assertNotNull(
            "onCreate must install a default GattClientFactory so a service " +
                "resurrected via START_STICKY can connect without MainActivity",
            SyauthCompanionService.gattClientFactory,
        )
    }

    /**
     * BUG-20260522-0138 (extension): even with `gattClientFactory`
     * defaulted, the approval path still failed on a process-restarted
     * service because four other JVM-static seams owned by
     * `MainActivity.installCompanionSeams` were null. The user observed
     * "tap Approve → app closes without biometric" — the activity bailed
     * with `alias=''` (`keystoreAliasResolver=null`) before reaching
     * BiometricPrompt. Regression guard: every load-bearing companion
     * seam must be non-null after `onCreate`.
     */
    @Test
    fun on_create_installs_default_companion_seams_when_none_preset() {
        SyauthCompanionService.bondListProvider = BondListProvider { emptyList() }
        // All seams intentionally left null — simulates the
        // post-process-restart cold-start where MainActivity has not
        // had a chance to call installCompanionSeams.

        Robolectric.buildService(SyauthCompanionService::class.java).create()

        assertNotNull(
            "onCreate must default bondKeyProvider",
            SyauthCompanionService.bondKeyProvider,
        )
        assertNotNull(
            "onCreate must default hostnameResolver",
            SyauthCompanionService.hostnameResolver,
        )
        assertNotNull(
            "onCreate must default keystoreAliasResolver (load-bearing for Approve)",
            SyauthCompanionService.keystoreAliasResolver,
        )
        assertNotNull(
            "onCreate must default challengeVerifier",
            SyauthCompanionService.challengeVerifier,
        )
        assertNotNull(
            "onCreate must default ChallengeApprovalActivity.responseSink " +
                "(load-bearing for Approve to deliver signature to host)",
            ChallengeApprovalActivity.responseSink,
        )
        assertNotNull(
            "onCreate must default ChallengeApprovalActivity.cancelSink",
            ChallengeApprovalActivity.cancelSink,
        )
    }

    @Test
    fun stops_clients_on_destroy() {
        val bonds = listOf(
            bondFor(FIXTURE_PEER_A),
            bondFor(FIXTURE_PEER_B),
            bondFor(FIXTURE_PEER_C),
        )
        val factory = RecordingGattClientFactory()
        SyauthCompanionService.bondListProvider = BondListProvider { bonds }
        SyauthCompanionService.gattClientFactory = factory

        val controller = Robolectric.buildService(SyauthCompanionService::class.java).create()
        controller.destroy()

        assertEquals(3, factory.created.size)
        for ((_, client) in factory.created) {
            assertEquals(1, client.stopCalls)
        }
    }
}
