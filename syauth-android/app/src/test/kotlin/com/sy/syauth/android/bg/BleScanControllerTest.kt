// DEV-003 — JVM tests for [SyauthBleScannerController].
//
// Mirrors the test seam pattern used by the deleted GattAdvertiser /
// GattServerControllerTest before DEV-003: every Android platform
// dependency lives behind a small interface; the tests construct
// fakes and assert the controller's surface without standing up the
// real BLE stack.
//
// Journey: specs/journeys/JOURNEY-DEV-003-invert-advertising.md (TC-04, TC-05, TC-08).
package com.sy.syauth.android.bg

import android.bluetooth.le.ScanFilter
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.util.UUID

private val FIXTURE_BOND_KEY: ByteArray = ByteArray(32) { it.toByte() }
private const val FIXTURE_PEER_ID: String = "abcdef0123456789abcdef0123456789"
private const val FIXTURE_DEVICE_ADDR: String = "AA:BB:CC:DD:EE:FF"

/** Returns a deterministic 16-byte slot UUID for a (key, minute) pair. */
private fun lookupStub(key: ByteArray, minute: Long): ByteArray {
    val out = ByteArray(16)
    for (i in 0 until 16) {
        out[i] = (key[i].toInt() xor minute.toInt() xor i).toByte()
    }
    return out
}

private class RecordingScanner : BleScannerHandle {
    var starts: Int = 0
        private set
    var stops: Int = 0
        private set
    var lastFilters: List<ScanFilter> = emptyList()
        private set
    var lastCallback: BleScanCallback? = null
        private set
    override fun startScan(filters: List<ScanFilter>, callback: BleScanCallback) {
        starts += 1
        lastFilters = filters
        lastCallback = callback
    }
    override fun stopScan(callback: BleScanCallback) {
        stops += 1
    }
}

private class RecordingGattClient : BleGattClientHandle {
    var connectCalls: Int = 0
        private set
    var disconnectCalls: Int = 0
        private set
    var lastAddress: String? = null
        private set
    var lastCallback: BleGattClientCallback? = null
        private set
    override fun connect(deviceAddress: String, callback: BleGattClientCallback) {
        connectCalls += 1
        lastAddress = deviceAddress
        lastCallback = callback
    }
    override fun disconnect() {
        disconnectCalls += 1
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class BleScanControllerTest {

    private fun makeController(
        scanner: RecordingScanner,
        client: RecordingGattClient,
        clockSeconds: Long = 1_800_000_000L,
        bondKey: ByteArray? = FIXTURE_BOND_KEY,
        peerId: String? = FIXTURE_PEER_ID,
    ): SyauthBleScannerController = SyauthBleScannerController(
        bondKeyResolver = { bondKey },
        peerIdResolver = { peerId },
        clock = { clockSeconds },
        sessionUuidLookup = ::lookupStub,
        scannerFactory = { scanner },
        gattClientFactory = { client },
    )

    @Test
    fun start_filters_on_current_and_previous_minute_slot_uuids() {
        val scanner = RecordingScanner()
        val client = RecordingGattClient()
        val controller = makeController(scanner, client)

        controller.start(association = null) { _, _ -> }

        assertEquals(1, scanner.starts)
        assertEquals(2, scanner.lastFilters.size)
        val expectedCurrent = uuidFromBytes(lookupStub(FIXTURE_BOND_KEY, 1_800_000_000L / 60))
        val expectedPrevious = uuidFromBytes(lookupStub(FIXTURE_BOND_KEY, 1_800_000_000L / 60 - 1))
        val installed: List<UUID> = controller.lastFilterUuids
        assertEquals(listOf(expectedCurrent, expectedPrevious), installed)
        // The two UUIDs must differ — pins the rotation cadence the
        // SPEC §3.2 D8 rationale demands.
        assertNotEquals(expectedCurrent, expectedPrevious)
    }

    @Test
    fun stop_tears_down_scanner_and_is_idempotent() {
        val scanner = RecordingScanner()
        val client = RecordingGattClient()
        val controller = makeController(scanner, client)

        controller.start(association = null) { _, _ -> }
        controller.stop()
        controller.stop()

        assertEquals(1, scanner.stops)
    }

    @Test
    fun start_is_idempotent_does_not_register_twice() {
        val scanner = RecordingScanner()
        val client = RecordingGattClient()
        val controller = makeController(scanner, client)

        controller.start(association = null) { _, _ -> }
        controller.start(association = null) { _, _ -> }

        assertEquals(1, scanner.starts)
    }

    @Test
    fun scan_match_opens_gatt_client_and_forwards_challenge() {
        val scanner = RecordingScanner()
        val client = RecordingGattClient()
        val received: MutableList<Pair<String, ByteArray>> = mutableListOf()
        val controller = makeController(scanner, client)

        controller.start(association = null) { peerId, frame ->
            received.add(peerId to frame)
        }
        val callback = scanner.lastCallback
        assertNotNull(callback)
        callback!!.onAdvertisementMatch(controller.lastFilterUuids.first(), FIXTURE_DEVICE_ADDR)

        assertEquals(1, client.connectCalls)
        assertEquals(FIXTURE_DEVICE_ADDR, client.lastAddress)
        val gattCb = client.lastCallback
        assertNotNull(gattCb)
        val frame = byteArrayOf(0x01, 0x02, 0x03)
        gattCb!!.onChallengeReceived(frame)
        assertEquals(1, received.size)
        assertEquals(FIXTURE_PEER_ID, received[0].first)
        assertTrue(frame.contentEquals(received[0].second))
    }

    @Test
    fun start_stays_idle_when_bond_key_missing() {
        val scanner = RecordingScanner()
        val client = RecordingGattClient()
        val controller = makeController(scanner, client, bondKey = null)

        controller.start(association = null) { _, _ -> }

        assertEquals(0, scanner.starts)
        assertEquals(emptyList<UUID>(), controller.lastFilterUuids)
    }

    @Test
    fun start_stays_idle_when_peer_id_missing() {
        val scanner = RecordingScanner()
        val client = RecordingGattClient()
        val controller = makeController(scanner, client, peerId = null)

        controller.start(association = null) { _, _ -> }

        assertEquals(0, scanner.starts)
    }

    @Test
    fun start_stays_idle_when_scanner_unavailable() {
        val scanner = RecordingScanner()
        val client = RecordingGattClient()
        val controller = SyauthBleScannerController(
            bondKeyResolver = { FIXTURE_BOND_KEY },
            peerIdResolver = { FIXTURE_PEER_ID },
            clock = { 1L },
            sessionUuidLookup = ::lookupStub,
            scannerFactory = { null },
            gattClientFactory = { client },
        )

        controller.start(association = null) { _, _ -> }

        assertEquals(0, scanner.starts)
        assertEquals(0, client.connectCalls)
    }

    @Test
    fun slot_uuids_replayed_at_next_minute_no_longer_match_filter() {
        // TC-04 — DEV-003 closure: a slot-N broadcast replayed at
        // minute N+1 is structurally excluded from the phone's
        // filter set. The set rolls forward to {N+1, N} at minute
        // N+1; the slot N-1 the attacker is replaying is no longer
        // a member.
        val nowSecondsAtN: Long = 60L * 1000L
        val nowSecondsAtNPlus1: Long = nowSecondsAtN + 60L
        val filtersAtN = slotUuidsFor(FIXTURE_BOND_KEY, nowSecondsAtN, ::lookupStub)
        val filtersAtNPlus1 = slotUuidsFor(FIXTURE_BOND_KEY, nowSecondsAtNPlus1, ::lookupStub)
        // The slot N-1 UUID was in the filter at minute N (as previous)
        // but is NOT in the filter at minute N+1.
        val attackerSlot = uuidFromBytes(lookupStub(FIXTURE_BOND_KEY, (nowSecondsAtN / 60) - 1L))
        assertTrue(attackerSlot in filtersAtN)
        assertFalse(attackerSlot in filtersAtNPlus1)
    }

    @Test
    fun slot_uuids_differ_per_bond_key() {
        // TC-05 — different bond keys produce different slot UUIDs
        // at the same minute. A second desktop derived from a
        // different bond key is structurally invisible to a phone
        // holding only the first bond key.
        val otherBond = ByteArray(32) { (it + 100).toByte() }
        val slotsA = slotUuidsFor(FIXTURE_BOND_KEY, 1_000_000L, ::lookupStub)
        val slotsB = slotUuidsFor(otherBond, 1_000_000L, ::lookupStub)
        // The two phones' filter sets are disjoint: not a single
        // overlapping UUID. This is the structural defense.
        for (uuidA in slotsA) {
            assertFalse("slot $uuidA must not match a different bond's filter", uuidA in slotsB)
        }
    }

    @Test
    fun stop_disconnects_gatt_client_when_open() {
        val scanner = RecordingScanner()
        val client = RecordingGattClient()
        val controller = makeController(scanner, client)

        controller.start(association = null) { _, _ -> }
        scanner.lastCallback!!.onAdvertisementMatch(controller.lastFilterUuids.first(), FIXTURE_DEVICE_ADDR)
        controller.stop()

        assertEquals(1, client.disconnectCalls)
    }

    @Test
    fun stop_without_start_is_noop() {
        val scanner = RecordingScanner()
        val client = RecordingGattClient()
        val controller = makeController(scanner, client)

        controller.stop()

        assertEquals(0, scanner.stops)
        assertEquals(0, client.disconnectCalls)
        assertNull(scanner.lastCallback)
    }

    @Test
    fun slot_uuids_helper_includes_current_minute_first() {
        // The current minute MUST be the first element so a tie-
        // breaker in the platform's filter ordering preserves the
        // freshest slot.
        val now: Long = 60L * 1234L
        val uuids = slotUuidsFor(FIXTURE_BOND_KEY, now, ::lookupStub)
        val expectedCurrent = uuidFromBytes(lookupStub(FIXTURE_BOND_KEY, 1234L))
        assertEquals(expectedCurrent, uuids.first())
    }
}
