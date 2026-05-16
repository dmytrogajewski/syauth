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
import android.bluetooth.BluetoothGattDescriptor
import android.bluetooth.BluetoothGattService
import android.bluetooth.le.AdvertiseCallback
import android.bluetooth.le.AdvertiseData
import android.bluetooth.le.AdvertiseSettings
import android.companion.AssociationInfo
import android.os.ParcelUuid
import android.util.Log
import java.util.UUID
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

/** Logcat tag for the GATT server controller. */
internal const val GATT_SERVER_LOG_TAG: String = "syauth.gatt"

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
    /**
     * v0.1 demo: plain WRITE permission (no link encryption).
     *
     * The frame layer (BLAKE3-keyed MAC under the shared bond_key +
     * Ed25519-signed responses) is the authenticated boundary; link
     * encryption is defense-in-depth that v0.2 reinstates alongside
     * LESC pairing. Until then, requiring encryption blocks the
     * desktop's bluer GATT client because it never bonded with the
     * phone — operating mode lands on plaintext writes whose payload
     * itself carries the cryptographic protection.
     */
    val WRITE_ENCRYPTED: Int =
        BluetoothGattCharacteristic.PERMISSION_WRITE
    /** v0.1 demo: plain READ permission. Same rationale as WRITE. */
    val READ_ENCRYPTED: Int =
        BluetoothGattCharacteristic.PERMISSION_READ
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

    /**
     * Install the callback invoked when the desktop writes a frame to
     * the challenge characteristic. Production wires this to the real
     * `BluetoothGattServerCallback.onCharacteristicWriteRequest`; tests
     * call the handler synthetically. Pass `null` to clear.
     */
    public fun setWriteHandler(handler: ((deviceAddress: String, value: ByteArray) -> Unit)?)

    /**
     * Notify the connected desktop with [bytes] on the response
     * characteristic. Returns `true` when the notify call was queued by
     * the platform; `false` when no peer is connected or the notify
     * failed at the radio. The desktop's `bluer` notify stream picks
     * the bytes up on success.
     */
    public fun notifyResponse(bytes: ByteArray): Boolean

    /** Close the underlying server. */
    public fun close()
}

/**
 * Thin abstraction over `BluetoothLeAdvertiser`. Allows the always-on
 * GATT host service to advertise the syauth service UUID so the
 * desktop's `bluer` scanner can discover the phone without operator
 * pre-configuration of the MAC.
 *
 * Production wraps `BluetoothLeAdvertiser.startAdvertising` /
 * `stopAdvertising`; tests inject a fake to observe the AdvertiseData
 * the controller built.
 */
public interface BleAdvertiserHandle {
    /** Begin advertising with the supplied data + settings. */
    public fun startAdvertising(
        settings: AdvertiseSettings,
        data: AdvertiseData,
        callback: AdvertiseCallback,
    )

    /** Stop a previously started advertisement. */
    public fun stopAdvertising(callback: AdvertiseCallback)
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
    private val advertiserFactory: () -> BleAdvertiserHandle? = { null },
) : GattServerController {

    private val current: AtomicReference<GattServerHandle?> = AtomicReference(null)
    private val advertiser: AtomicReference<BleAdvertiserHandle?> = AtomicReference(null)
    private val advertising: AtomicBoolean = AtomicBoolean(false)
    private val advertiseCallback: AdvertiseCallback = object : AdvertiseCallback() {
        override fun onStartFailure(errorCode: Int) {
            Log.w(GATT_SERVER_LOG_TAG, "BLE advertise failed errorCode=$errorCode")
        }
    }

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
        // Install the bridge from `BluetoothGattServerCallback`'s
        // write-request path (production) or the test fake's synthetic
        // driver (tests) into the controller's `pendingChallengeCallback`
        // closure. The handle holds the function reference; the
        // controller does not need to drive writes itself.
        pendingChallengeCallback = onChallenge
        handle.setWriteHandler { deviceAddress, value -> onChallenge(deviceAddress, value) }
        startAdvertisingIfPossible()
    }

    override fun stop() {
        val handle = current.getAndSet(null) ?: return
        pendingChallengeCallback = null
        handle.setWriteHandler(null)
        stopAdvertisingIfActive()
        handle.close()
    }

    /**
     * Push [bytes] to the connected desktop on the response
     * characteristic. Returns `false` when the server is not started
     * or no peer is currently connected.
     */
    public fun notifyResponse(bytes: ByteArray): Boolean {
        val handle = current.get() ?: return false
        return handle.notifyResponse(bytes)
    }

    private fun startAdvertisingIfPossible() {
        if (!advertising.compareAndSet(false, true)) {
            return
        }
        val handle = runCatching { advertiserFactory() }
            .onFailure { Log.w(GATT_SERVER_LOG_TAG, "advertiser factory threw: ${it.message}") }
            .getOrNull()
        if (handle == null) {
            advertising.set(false)
            Log.w(GATT_SERVER_LOG_TAG, "advertiser unsupported on this adapter; GATT still up")
            return
        }
        advertiser.set(handle)
        val settings = AdvertiseSettings.Builder()
            .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_LOW_LATENCY)
            .setTxPowerLevel(AdvertiseSettings.ADVERTISE_TX_POWER_MEDIUM)
            .setConnectable(true)
            .build()
        val data = AdvertiseData.Builder()
            .setIncludeDeviceName(false)
            .addServiceUuid(ParcelUuid(SYAUTH_GATT_SERVICE_UUID))
            .build()
        runCatching { handle.startAdvertising(settings, data, advertiseCallback) }
            .onFailure {
                Log.w(GATT_SERVER_LOG_TAG, "startAdvertising threw: ${it.message}")
                advertising.set(false)
                advertiser.set(null)
            }
    }

    private fun stopAdvertisingIfActive() {
        if (!advertising.compareAndSet(true, false)) {
            return
        }
        val handle = advertiser.getAndSet(null) ?: return
        runCatching { handle.stopAdvertising(advertiseCallback) }
            .onFailure { Log.w(GATT_SERVER_LOG_TAG, "stopAdvertising threw: ${it.message}") }
    }

    /** True iff a BLE advertisement is currently active. Exposed for tests. */
    internal val isAdvertising: Boolean
        get() = advertising.get()

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
        // CCCD descriptor (0x2902) so the desktop's bluer client can
        // subscribe to NOTIFY on the response characteristic. Without
        // this descriptor, `Characteristic::notify()` on the desktop
        // errors at CCCD-write time and the entire response path stays
        // dead. The descriptor is writable so subscribe-writes land;
        // we accept any value and rely on the characteristic's NOTIFY
        // property to drive the actual stream.
        val cccd = BluetoothGattDescriptor(
            CCCD_DESCRIPTOR_UUID,
            BluetoothGattDescriptor.PERMISSION_READ or
                BluetoothGattDescriptor.PERMISSION_WRITE,
        )
        response.addDescriptor(cccd)
        service.addCharacteristic(challenge)
        service.addCharacteristic(response)
        return service
    }
}

/**
 * Client Characteristic Configuration Descriptor UUID per the Bluetooth
 * core spec. Required on every NOTIFY-bearing characteristic so a
 * client (the desktop in our case) can subscribe by writing the
 * subscribe bitfield. Hard-coded here rather than pulled from
 * `android.bluetooth.BluetoothGattDescriptor` because Android does not
 * expose it as a public constant.
 */
internal val CCCD_DESCRIPTOR_UUID: UUID =
    UUID.fromString("00002902-0000-1000-8000-00805f9b34fb")
