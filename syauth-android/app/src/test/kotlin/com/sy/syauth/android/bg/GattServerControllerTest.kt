// Roadmap item S-018 — JVM tests for the production
// [BluerlessGattServerController]. Uses a `FakeGattServerHandle` to
// observe `addService` / `close` calls without instantiating a real
// `BluetoothGattServer`. The fake records the single service
// registered + every characteristic on it, so the test asserts our
// UUID contract at the seam level.
//
// Robolectric is used because constructing
// `android.bluetooth.BluetoothGattService` / `BluetoothGattCharacteristic`
// requires the framework's stub layer. The pure-JVM JNI is loaded
// lazily so `@Config(sdk = [34])` is enough to keep the test
// hermetic.
//
// The `AssociationInfo` parameter on `GattServerController.start` is
// passed as `null` — the production controller does not consume the
// association beyond signature compatibility; the service is the
// component that uses it for hostname lookup.
package com.sy.syauth.android.bg

import android.bluetooth.BluetoothGattService
import java.util.UUID
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Fake [GattServerHandle] that records every call so the test can
 * assert exact behavior without standing up a real BLE stack.
 */
private class FakeGattServerHandle : GattServerHandle {
    var addServiceCount: Int = 0
        private set
    var closeCount: Int = 0
        private set
    var lastAddedService: BluetoothGattService? = null
        private set
    var addServiceReturn: Boolean = true

    override fun addService(service: BluetoothGattService): Boolean {
        addServiceCount += 1
        lastAddedService = service
        return addServiceReturn
    }

    override fun close() {
        closeCount += 1
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class GattServerControllerTest {

    @Test
    fun start_registers_service_with_pinned_uuid_and_two_chars() {
        val handle = FakeGattServerHandle()
        val controller = BluerlessGattServerController { handle }

        controller.start(association = null) { _, _ -> }

        assertEquals(1, handle.addServiceCount)
        val service = handle.lastAddedService
        assertNotNull(service)
        assertEquals(SYAUTH_GATT_SERVICE_UUID, service!!.uuid)
        val uuids: Set<UUID> = service.characteristics.map { it.uuid }.toSet()
        assertTrue(
            "expected challenge UUID, got $uuids",
            uuids.contains(SYAUTH_CHALLENGE_CHAR_UUID),
        )
        assertTrue(
            "expected response UUID, got $uuids",
            uuids.contains(SYAUTH_RESPONSE_CHAR_UUID),
        )
    }

    @Test
    fun start_is_idempotent_does_not_register_twice() {
        val handle = FakeGattServerHandle()
        val controller = BluerlessGattServerController { handle }

        controller.start(association = null) { _, _ -> }
        controller.start(association = null) { _, _ -> }

        assertEquals(1, handle.addServiceCount)
    }

    @Test
    fun stop_closes_handle_and_clears_callback() {
        val handle = FakeGattServerHandle()
        val controller = BluerlessGattServerController { handle }

        controller.start(association = null) { _, _ -> }
        assertNotNull(controller.pendingChallengeCallback)
        controller.stop()

        assertEquals(1, handle.closeCount)
        assertNull(controller.pendingChallengeCallback)
    }

    @Test
    fun stop_on_idle_is_a_noop() {
        val handle = FakeGattServerHandle()
        val controller = BluerlessGattServerController { handle }

        controller.stop()

        assertEquals(0, handle.closeCount)
    }

    @Test
    fun restart_after_stop_acquires_fresh_handle() {
        var built = 0
        val controller = BluerlessGattServerController {
            built += 1
            FakeGattServerHandle()
        }

        controller.start(association = null) { _, _ -> }
        controller.stop()
        controller.start(association = null) { _, _ -> }

        assertEquals("expected exactly two handles built", 2, built)
    }

    @Test
    fun callback_is_retained_until_stop() {
        val handle = FakeGattServerHandle()
        val controller = BluerlessGattServerController { handle }
        val cb: (String, ByteArray) -> Unit = { _, _ -> }

        controller.start(association = null, onChallenge = cb)

        assertSame(cb, controller.pendingChallengeCallback)
    }
}
