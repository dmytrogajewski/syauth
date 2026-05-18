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
