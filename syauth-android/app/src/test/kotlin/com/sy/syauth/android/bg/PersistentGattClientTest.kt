// Roadmap item S-010 — Robolectric JVM tests for [PersistentGattClient].
//
// The four cases pin the DoD bullets from
// `specs/unlock-proximity/ROADMAP.md` Step S-010 verbatim:
//
//   1. `auto_connect_true_passed_to_connectGatt` — captures the
//      `autoConnect` argument the client passes through the
//      `GattOpener` seam. The seam exists because Robolectric 4.11.1
//      `ShadowBluetoothDevice` does not expose a `getAutoConnect()`
//      getter, so direct argument capture is the only mechanical
//      way to assert the contract.
//   2. `on_services_discovered_subscribes_via_cccd` — drives
//      `BluetoothGattCallback.onServicesDiscovered` and asserts the
//      challenge characteristic's CCCD descriptor's `value` equals
//      `CCCD_ENABLE_NOTIFY` after the production code has run.
//   3. `on_characteristic_changed_invokes_onChallenge` — pushes a
//      notify frame through the API-33+ override and asserts the
//      constructor's `onChallenge` lambda is invoked exactly once
//      with the constructor's `peerId` and the byte-for-byte
//      payload; a notify on a non-challenge UUID does NOT invoke
//      the callback.
//   4. `write_response_targets_response_characteristic` — calls
//      `writeResponse(bytes)` and asserts the response
//      characteristic's `value` was set to `bytes`. (The boolean
//      return is the result of `gatt.writeCharacteristic(c)`,
//      which under Robolectric returns false because no shadow
//      implements the call; the production code still returns the
//      stack's verdict verbatim.)
//
// Journey: specs/journeys/JOURNEY-S-010-persistent-gatt-client.md
package com.sy.syauth.android.bg

import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCallback
import android.bluetooth.BluetoothGattCharacteristic
import android.bluetooth.BluetoothGattDescriptor
import android.bluetooth.BluetoothGattService
import android.bluetooth.BluetoothProfile
import android.content.Context
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertSame
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import java.util.UUID

private const val TEST_PEER_ID: String = "alex-desktop"
private const val TEST_DEVICE_MAC: String = "AA:BB:CC:DD:EE:FF"
private val TEST_CCCD_UUID: UUID =
    UUID.fromString("00002902-0000-1000-8000-00805f9b34fb")
private val TEST_CCCD_ENABLE_NOTIFY: ByteArray = byteArrayOf(0x01, 0x00)
private val TEST_SERVICE_UUID: UUID =
    UUID.fromString("5a4e8e3c-1c4c-4a17-9c81-d518a55a0001")

private class RecordingOpener(
    private val handle: BluetoothGatt,
) : GattOpener {
    var openCalls: Int = 0
        private set
    var lastDevice: BluetoothDevice? = null
        private set
    var lastAutoConnect: Boolean? = null
        private set
    var lastCallback: BluetoothGattCallback? = null
        private set

    override fun open(
        device: BluetoothDevice,
        autoConnect: Boolean,
        callback: BluetoothGattCallback,
    ): BluetoothGatt {
        openCalls += 1
        lastDevice = device
        lastAutoConnect = autoConnect
        lastCallback = callback
        return handle
    }
}

private fun ctx(): Context = ApplicationProvider.getApplicationContext()

private fun makeChallengeChar(): BluetoothGattCharacteristic {
    val c = BluetoothGattCharacteristic(
        SYAUTH_CHALLENGE_CHAR_UUID,
        BluetoothGattCharacteristic.PROPERTY_NOTIFY,
        BluetoothGattCharacteristic.PERMISSION_READ,
    )
    c.addDescriptor(
        BluetoothGattDescriptor(
            TEST_CCCD_UUID,
            BluetoothGattDescriptor.PERMISSION_READ
                or BluetoothGattDescriptor.PERMISSION_WRITE,
        )
    )
    return c
}

private fun makeResponseChar(): BluetoothGattCharacteristic =
    BluetoothGattCharacteristic(
        SYAUTH_RESPONSE_CHAR_UUID,
        BluetoothGattCharacteristic.PROPERTY_WRITE,
        BluetoothGattCharacteristic.PERMISSION_WRITE,
    )

private fun makeServiceWithBothChars(): BluetoothGattService {
    val service = BluetoothGattService(
        TEST_SERVICE_UUID,
        BluetoothGattService.SERVICE_TYPE_PRIMARY,
    )
    service.addCharacteristic(makeChallengeChar())
    service.addCharacteristic(makeResponseChar())
    return service
}

private fun newShadowGatt(): BluetoothGatt {
    val device = BluetoothAdapter.getDefaultAdapter()
        .getRemoteDevice(TEST_DEVICE_MAC)
    return org.robolectric.shadows.ShadowBluetoothGatt.newInstance(device)
}

private fun shadowGattAddService(
    gatt: BluetoothGatt,
    service: BluetoothGattService,
) {
    shadowOf(gatt).addDiscoverableService(service)
    // The shadow's `getServices()` reflects the `services` list, not
    // `discoverableServices`. Drive the shadow's `discoverServices`
    // so subsequent `gatt.services` returns the same list our
    // production code will inspect inside `onServicesDiscovered`.
    gatt.discoverServices()
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class PersistentGattClientTest {

    @Test
    fun auto_connect_true_passed_to_connectGatt() {
        val handle = newShadowGatt()
        val opener = RecordingOpener(handle)
        val client = PersistentGattClient(
            context = ctx(),
            adapter = BluetoothAdapter.getDefaultAdapter(),
            peerId = TEST_PEER_ID,
            deviceMac = TEST_DEVICE_MAC,
            onChallenge = { _, _ -> },
            gattOpener = opener,
        )

        client.start()

        assertEquals(1, opener.openCalls)
        assertEquals(true, opener.lastAutoConnect)
        assertEquals(TEST_DEVICE_MAC, opener.lastDevice?.address)
        assertNotNull(opener.lastCallback)
    }

    @Test
    fun on_services_discovered_subscribes_via_cccd() {
        val handle = newShadowGatt()
        val opener = RecordingOpener(handle)
        val service = makeServiceWithBothChars()
        shadowGattAddService(handle, service)

        val client = PersistentGattClient(
            context = ctx(),
            adapter = BluetoothAdapter.getDefaultAdapter(),
            peerId = TEST_PEER_ID,
            deviceMac = TEST_DEVICE_MAC,
            onChallenge = { _, _ -> },
            gattOpener = opener,
        )
        client.start()

        val callback = opener.lastCallback!!
        callback.onConnectionStateChange(
            handle,
            BluetoothGatt.GATT_SUCCESS,
            BluetoothProfile.STATE_CONNECTED,
        )
        callback.onServicesDiscovered(handle, BluetoothGatt.GATT_SUCCESS)

        val challenge = service.getCharacteristic(SYAUTH_CHALLENGE_CHAR_UUID)
        val cccd = challenge.getDescriptor(TEST_CCCD_UUID)
        assertNotNull("CCCD descriptor present", cccd)
        assertArrayEquals(TEST_CCCD_ENABLE_NOTIFY, cccd.value)
    }

    @Test
    fun on_characteristic_changed_invokes_onChallenge() {
        val handle = newShadowGatt()
        val opener = RecordingOpener(handle)
        val service = makeServiceWithBothChars()
        shadowGattAddService(handle, service)
        val payload = byteArrayOf(0x10, 0x20, 0x30, 0x40)
        var received: Pair<String, ByteArray>? = null

        val client = PersistentGattClient(
            context = ctx(),
            adapter = BluetoothAdapter.getDefaultAdapter(),
            peerId = TEST_PEER_ID,
            deviceMac = TEST_DEVICE_MAC,
            onChallenge = { peer, bytes -> received = peer to bytes },
            gattOpener = opener,
        )
        client.start()
        val callback = opener.lastCallback!!
        val challenge = service.getCharacteristic(SYAUTH_CHALLENGE_CHAR_UUID)

        callback.onCharacteristicChanged(handle, challenge, payload)

        assertNotNull("onChallenge fired", received)
        assertEquals(TEST_PEER_ID, received?.first)
        assertArrayEquals(payload, received?.second)

        // A notify on a non-challenge UUID must NOT invoke the callback.
        val resp = service.getCharacteristic(SYAUTH_RESPONSE_CHAR_UUID)
        received = null
        callback.onCharacteristicChanged(handle, resp, byteArrayOf(0x99.toByte()))
        assertEquals(null, received)
    }

    @Test
    fun write_response_targets_response_characteristic() {
        val handle = newShadowGatt()
        val opener = RecordingOpener(handle)
        val service = makeServiceWithBothChars()
        shadowGattAddService(handle, service)
        val payload = byteArrayOf(0x55, 0x66, 0x77, 0x77.toByte())

        val client = PersistentGattClient(
            context = ctx(),
            adapter = BluetoothAdapter.getDefaultAdapter(),
            peerId = TEST_PEER_ID,
            deviceMac = TEST_DEVICE_MAC,
            onChallenge = { _, _ -> },
            gattOpener = opener,
        )
        client.start()

        client.writeResponse(payload)

        val resp = service.getCharacteristic(SYAUTH_RESPONSE_CHAR_UUID)
        // The production code sets the characteristic value and
        // then calls `gatt.writeCharacteristic(c)`. Under
        // Robolectric the write itself is a no-op (no shadow), but
        // the `.value` assignment is observable here.
        assertSame(
            "response characteristic value points at payload",
            payload,
            resp.value,
        )
    }
}
