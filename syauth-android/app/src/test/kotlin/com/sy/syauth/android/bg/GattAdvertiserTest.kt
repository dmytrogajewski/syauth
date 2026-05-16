// syauth — JVM tests for the BLE advertise plumbing added to
// [BluerlessGattServerController] for the v0.1 demo host service.
//
// The advertiser factory is injected behind a [BleAdvertiserHandle]
// seam; the tests assert that start() begins exactly one
// advertisement whose data carries [SYAUTH_GATT_SERVICE_UUID], and
// that stop() halts it (idempotently).
package com.sy.syauth.android.bg

import android.bluetooth.BluetoothGattService
import android.bluetooth.le.AdvertiseCallback
import android.bluetooth.le.AdvertiseData
import android.bluetooth.le.AdvertiseSettings
import android.os.ParcelUuid
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

private class CountingGattHandle : GattServerHandle {
    var added: Int = 0
    var closed: Int = 0
    override fun addService(service: BluetoothGattService): Boolean {
        added += 1
        return true
    }
    override fun setWriteHandler(handler: ((String, ByteArray) -> Unit)?) {}
    override fun notifyResponse(bytes: ByteArray): Boolean = false
    override fun close() {
        closed += 1
    }
}

private class RecordingAdvertiser : BleAdvertiserHandle {
    var starts: Int = 0
    var stops: Int = 0
    var lastData: AdvertiseData? = null
    var lastSettings: AdvertiseSettings? = null
    var lastCallback: AdvertiseCallback? = null
    override fun startAdvertising(
        settings: AdvertiseSettings,
        data: AdvertiseData,
        callback: AdvertiseCallback,
    ) {
        starts += 1
        lastSettings = settings
        lastData = data
        lastCallback = callback
    }
    override fun stopAdvertising(callback: AdvertiseCallback) {
        stops += 1
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class GattAdvertiserTest {

    @Test
    fun start_invokes_advertiser_with_syauth_service_uuid() {
        val advertiser = RecordingAdvertiser()
        val controller = BluerlessGattServerController(
            handleFactory = { CountingGattHandle() },
            advertiserFactory = { advertiser },
        )

        controller.start(association = null) { _, _ -> }

        assertEquals(1, advertiser.starts)
        val data = advertiser.lastData
        assertNotNull(data)
        val uuids: List<ParcelUuid> = data!!.serviceUuids ?: emptyList()
        assertTrue(
            "expected SYAUTH_GATT_SERVICE_UUID in advertise data, got $uuids",
            uuids.any { it.uuid == SYAUTH_GATT_SERVICE_UUID },
        )
        assertTrue(controller.isAdvertising)
    }

    @Test
    fun stop_stops_the_advertisement_and_is_idempotent() {
        val advertiser = RecordingAdvertiser()
        val controller = BluerlessGattServerController(
            handleFactory = { CountingGattHandle() },
            advertiserFactory = { advertiser },
        )

        controller.start(association = null) { _, _ -> }
        controller.stop()

        assertEquals(1, advertiser.stops)
        assertFalse(controller.isAdvertising)

        controller.stop()
        // Idempotent: still exactly one stopAdvertising call.
        assertEquals(1, advertiser.stops)
    }

    @Test
    fun null_advertiser_factory_does_not_block_start() {
        val gatt = CountingGattHandle()
        val controller = BluerlessGattServerController(
            handleFactory = { gatt },
            advertiserFactory = { null },
        )

        controller.start(association = null) { _, _ -> }

        // GATT registered, advertising flagged off.
        assertEquals(1, gatt.added)
        assertFalse(controller.isAdvertising)
    }

    @Test
    fun start_is_idempotent_for_advertising_too() {
        val advertiser = RecordingAdvertiser()
        val controller = BluerlessGattServerController(
            handleFactory = { CountingGattHandle() },
            advertiserFactory = { advertiser },
        )

        controller.start(association = null) { _, _ -> }
        controller.start(association = null) { _, _ -> }

        assertEquals(1, advertiser.starts)
    }
}
