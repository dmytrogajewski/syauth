// Roadmap item S-018 — GATT server controller.
//
// The phone-side GATT server is the inverse of the SPEC §3.D8 desktop
// advertise-and-scan model: the desktop writes a wire-frame challenge
// to our characteristic, we verify it via the UniFFI
// `verifyChallengeFrame(bondKey, frameBytes)` call, and (on success)
// invoke a callback that the service translates into a notification.
// On user approve, the response signature is written back over the
// SYAUTH_RESPONSE_CHAR_UUID characteristic.
//
// The controller is split into a fun interface ([GattServerController])
// and a production implementation ([BluerlessGattServerController])
// that delegates to a thin abstraction over `BluetoothGattServer`
// ([GattServerHandle]). The abstraction exists for two reasons:
//
//   1. JVM-only unit tests can construct a fake handle and assert
//      `addService` was called with the right UUIDs without dragging
//      `android.bluetooth.*` runtime classes into the classpath.
//   2. The seam isolates the small surface we actually use from the
//      `BluetoothGattServer` god-object that exposes ~30 methods.
//
// The wire protocol is the SPEC §4.1 frame format: `[ver=1][nonce:16]
// [payload:?][tag:16]`. Verification is done in Rust via UniFFI; we
// pass the bond key and the raw bytes and receive the payload (or a
// `MobileException` that we silently drop).
package com.sy.syauth.android.bg

import android.bluetooth.BluetoothGattCharacteristic
import android.bluetooth.BluetoothGattService
import android.companion.AssociationInfo
import java.util.UUID
import java.util.concurrent.atomic.AtomicReference

/**
 * Service UUID our GATT server publishes. Mirrors the desktop's
 * advertised service UUID (SPEC §3 transport contract). Picked from
 * the syauth-reserved range and pinned here as a named constant per
 * the AGENTS.md "no magic literals" rule.
 *
 * The UUID is deterministic across builds — the desktop discovers us
 * by exactly this value.
 */
public val SYAUTH_GATT_SERVICE_UUID: UUID =
    UUID.fromString("5a4e8e3c-1c4c-4a17-9c81-d518a55a0001")

/**
 * Characteristic the desktop writes challenge frames to. The
 * characteristic is `WRITE` + `NOTIFY` so the server-side BLE stack
 * delivers writes and allows us to push state changes back if
 * needed.
 */
public val SYAUTH_CHALLENGE_CHAR_UUID: UUID =
    UUID.fromString("5a4e8e3c-1c4c-4a17-9c81-d518a55a0002")

/**
 * Characteristic the phone writes signed responses to. Direction is
 * phone -> desktop, so on our server side the characteristic is
 * configured as `READ` + `NOTIFY` (the desktop subscribes; we notify).
 */
public val SYAUTH_RESPONSE_CHAR_UUID: UUID =
    UUID.fromString("5a4e8e3c-1c4c-4a17-9c81-d518a55a0003")

/**
 * Permission bits used on our two characteristics. Pulled out as
 * named values so a future reader does not have to grep
 * `BluetoothGattCharacteristic` for the raw int values. `val`
 * (not `const val`) because the constants reference Android
 * platform statics that are not Kotlin compile-time constants.
 */
internal object GattPermissions {
    /** Write requires an authenticated, encrypted link. */
    val WRITE_ENCRYPTED: Int =
        BluetoothGattCharacteristic.PERMISSION_WRITE_ENCRYPTED
    /** Read requires an authenticated, encrypted link. */
    val READ_ENCRYPTED: Int =
        BluetoothGattCharacteristic.PERMISSION_READ_ENCRYPTED
}

/**
 * Property bits used on our two characteristics. Same rationale.
 */
internal object GattProperties {
    /** Challenge: client writes (with response). */
    val CHALLENGE: Int =
        BluetoothGattCharacteristic.PROPERTY_WRITE or
            BluetoothGattCharacteristic.PROPERTY_NOTIFY
    /** Response: client subscribes; we notify. */
    val RESPONSE: Int =
        BluetoothGattCharacteristic.PROPERTY_READ or
            BluetoothGattCharacteristic.PROPERTY_NOTIFY
}

/**
 * Sentinel exception type for the very few framework callbacks we
 * cannot model with a typed return. Carries a plain message so the
 * tracer can render it.
 */
internal class GattServerInitError(message: String) : RuntimeException(message)

/**
 * Contract the [SyauthCompanionService] uses to talk to a GATT
 * server. Note that the controller does not consume the
 * [AssociationInfo] itself — the service uses it for hostname
 * lookup and the response-sender registry; the controller only
 * needs the peerId, which the `onChallenge` callback carries.
 */
public interface GattServerController {
    /**
     * Bring up the GATT server. Routes every incoming challenge
     * frame through [onChallenge] (which receives the peer's
     * stringified id and the raw frame bytes). [association] is
     * accepted for the service-side bookkeeping that wraps the
     * controller and may be `null` when the controller is exercised
     * in a unit test that does not need to materialise an
     * AssociationInfo.
     *
     * The implementation MUST be idempotent: calling `start` twice
     * back-to-back must not throw and must leave a single GATT
     * service registered.
     */
    public fun start(
        association: AssociationInfo?,
        onChallenge: (peerId: String, frameBytes: ByteArray) -> Unit,
    )

    /**
     * Tear down the GATT server. Idempotent — calling `stop` on a
     * stopped controller is a no-op.
     */
    public fun stop()
}

/**
 * Minimal abstraction over `BluetoothGattServer`. Production wires a
 * thin adapter; tests inject a fake to observe `addService` /
 * `close` invocations and drive `onCharacteristicWriteRequest`
 * synthetically.
 */
public interface GattServerHandle {
    /** Register a GATT service. */
    public fun addService(service: BluetoothGattService): Boolean

    /** Close the underlying server. */
    public fun close()
}

/**
 * Production [GattServerController]. The "Bluerless" prefix calls
 * out that it talks to the Android Bluetooth stack directly, not via
 * the desktop-only `bluer` Rust crate — there is no Kotlin-side
 * `bluer` binding and the SPEC §4.6 dep table never promised one.
 *
 * The class delegates resource ownership to a single
 * `AtomicReference<GattServerHandle?>` so concurrent calls to
 * `start` / `stop` are race-free at the controller level. The
 * underlying `BluetoothGattServer` is itself thread-safe per
 * Android docs.
 *
 * Tests pass a `handleFactory` that returns a fake; production
 * passes a factory that calls
 * `BluetoothManager.openGattServer(context, callback)` and wraps
 * the result in an adapter that satisfies [GattServerHandle].
 */
public class BluerlessGattServerController(
    private val handleFactory: () -> GattServerHandle,
) : GattServerController {

    private val current: AtomicReference<GattServerHandle?> = AtomicReference(null)

    override fun start(
        association: AssociationInfo?,
        onChallenge: (peerId: String, frameBytes: ByteArray) -> Unit,
    ) {
        if (current.get() != null) {
            return
        }
        val handle = handleFactory()
        if (!current.compareAndSet(null, handle)) {
            // A concurrent start raced us; release our handle and
            // honor the existing one.
            handle.close()
            return
        }
        val service = buildService()
        val ok = handle.addService(service)
        if (!ok) {
            throw GattServerInitError(
                "BluetoothGattServer.addService rejected SYAUTH_GATT_SERVICE_UUID",
            )
        }
        // The `onChallenge` callback is wired through the production
        // adapter's `BluetoothGattServerCallback.onCharacteristicWriteRequest`
        // path; for tests the fake handle drives the callback
        // directly via its own surface. We retain a reference to
        // the callback by stashing it under
        // [pendingChallengeCallback] so the production adapter can
        // call it.
        pendingChallengeCallback = onChallenge
    }

    override fun stop() {
        val handle = current.getAndSet(null) ?: return
        pendingChallengeCallback = null
        handle.close()
    }

    /**
     * Stashed reference so the production adapter (and the test
     * fake, via reflection in the unit test) can drive the
     * callback. Volatile because the GATT callback may fire on a
     * binder thread distinct from the one that called `start`.
     */
    @Volatile
    internal var pendingChallengeCallback: (
        (peerId: String, frameBytes: ByteArray) -> Unit
    )? = null

    private fun buildService(): BluetoothGattService {
        val service = BluetoothGattService(
            SYAUTH_GATT_SERVICE_UUID,
            BluetoothGattService.SERVICE_TYPE_PRIMARY,
        )
        val challenge = BluetoothGattCharacteristic(
            SYAUTH_CHALLENGE_CHAR_UUID,
            GattProperties.CHALLENGE,
            GattPermissions.WRITE_ENCRYPTED,
        )
        val response = BluetoothGattCharacteristic(
            SYAUTH_RESPONSE_CHAR_UUID,
            GattProperties.RESPONSE,
            GattPermissions.READ_ENCRYPTED,
        )
        service.addCharacteristic(challenge)
        service.addCharacteristic(response)
        return service
    }
}
