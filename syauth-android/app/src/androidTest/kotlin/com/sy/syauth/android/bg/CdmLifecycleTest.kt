// Roadmap item S-018 — instrumented test for the
// CompanionDeviceService lifecycle.
//
// Requires connected device / emulator; CI is gated via the
// androidTest source set. The test verifies the service-binding
// lifecycle with a synthetic AssociationInfo fixture and a fake
// GattServerController; the framework's AssociationInfo has a hidden
// constructor on every API level we ship for, so the fixture goes
// through Parcel-round-trip via reflection. If construction fails
// (newer API levels may relock the constructor) the test
// `Assume.assumeTrue`'s out with a documented reason rather than
// failing.
package com.sy.syauth.android.bg

import android.companion.AssociationInfo
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Assume
import org.junit.Test

private const val FIXTURE_ASSOCIATION_ID: Long = 7L
private const val FIXTURE_PEER_ID: String = "AA:BB:CC:DD:EE:FF"

private class RecordingController : GattServerController {
    var startCalled: Boolean = false
        private set
    var stopCalled: Boolean = false
        private set
    var lastAssociation: AssociationInfo? = null
        private set

    override fun start(
        association: AssociationInfo?,
        onChallenge: (peerId: String, frameBytes: ByteArray) -> Unit,
    ) {
        startCalled = true
        lastAssociation = association
    }

    override fun stop() {
        stopCalled = true
    }
}

class CdmLifecycleTest {

    @After
    fun cleanup() {
        SyauthCompanionService.resetSeams()
    }

    /**
     * Build a synthetic [AssociationInfo] via reflection. The class
     * is `@hide` / @SystemApi on every API level we target; the
     * constructor signature changes between versions. We try a few
     * canonical shapes and `Assume.assumeTrue` out if none works on
     * the running device.
     */
    private fun fabricateAssociation(): AssociationInfo? = runCatching {
        val klass = AssociationInfo::class.java
        // Prefer the API 33+ constructor:
        //   AssociationInfo(int id, int userId, String packageName,
        //                   MacAddress deviceMacAddress, CharSequence displayName,
        //                   String deviceProfile, AssociatedDevice associatedDevice,
        //                   boolean selfManaged, boolean notifyOnDeviceNearby,
        //                   boolean revoked, long timeApprovedMs, long lastTimeConnectedMs,
        //                   int systemDataSyncFlags)
        // We don't actually invoke it because the parameter list
        // changes; instead try the simpler private ctor that takes
        // (int id, String packageName, MacAddress, CharSequence displayName, ...).
        // If reflection turns up no match, return null.
        val constructors = klass.declaredConstructors
        val ctor = constructors.firstOrNull { it.parameterCount >= 3 } ?: return@runCatching null
        ctor.isAccessible = true
        val args = arrayOfNulls<Any>(ctor.parameterCount)
        for ((i, paramType) in ctor.parameterTypes.withIndex()) {
            args[i] = defaultFor(paramType)
        }
        // Patch in the AssociationId and a recognisable display name
        // in slots 0 / 1 when we can. Best-effort; the controller
        // does not read these fields in the test fake.
        if (ctor.parameterTypes.isNotEmpty() && ctor.parameterTypes[0] == Int::class.javaPrimitiveType) {
            args[0] = FIXTURE_ASSOCIATION_ID.toInt()
        }
        ctor.newInstance(*args) as AssociationInfo
    }.getOrNull()

    private fun defaultFor(type: Class<*>): Any? = when {
        type == Int::class.javaPrimitiveType -> 0
        type == Long::class.javaPrimitiveType -> 0L
        type == Boolean::class.javaPrimitiveType -> false
        type == Byte::class.javaPrimitiveType -> 0.toByte()
        type == Short::class.javaPrimitiveType -> 0.toShort()
        type == Float::class.javaPrimitiveType -> 0.0f
        type == Double::class.javaPrimitiveType -> 0.0
        type == Char::class.javaPrimitiveType -> ' '
        type == String::class.java -> "syauth-test"
        type == CharSequence::class.java -> "syauth-test"
        else -> null
    }

    @Test
    fun on_device_appeared_starts_gatt_controller() {
        val assoc = fabricateAssociation()
        Assume.assumeTrue(
            "Skipping: this host's platform refused to construct AssociationInfo via reflection.",
            assoc != null,
        )
        val controller = RecordingController()
        SyauthCompanionService.gattControllerFactory = GattControllerFactory { _ -> controller }

        val service = SyauthCompanionService()
        service.onDeviceAppeared(assoc ?: return)

        assertTrue(controller.startCalled)
        assertNotNull("controller should have captured the association", controller.lastAssociation)
    }

    @Test
    fun on_device_disappeared_stops_gatt_controller() {
        val assoc = fabricateAssociation()
        Assume.assumeTrue(
            "Skipping: this host's platform refused to construct AssociationInfo via reflection.",
            assoc != null,
        )
        val controller = RecordingController()
        SyauthCompanionService.gattControllerFactory = GattControllerFactory { _ -> controller }

        val service = SyauthCompanionService()
        service.onDeviceAppeared(assoc ?: return)
        service.onDeviceDisappeared(assoc)

        assertTrue(controller.stopCalled)
    }

    @Test
    fun lifecycle_is_idempotent_disappear_without_appear() {
        val assoc = fabricateAssociation()
        Assume.assumeTrue(
            "Skipping: this host's platform refused to construct AssociationInfo via reflection.",
            assoc != null,
        )
        val controller = RecordingController()
        SyauthCompanionService.gattControllerFactory = GattControllerFactory { _ -> controller }

        val service = SyauthCompanionService()
        service.onDeviceDisappeared(assoc ?: return)

        assertEquals(false, controller.stopCalled)
    }
}
