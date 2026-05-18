// DEV-003 — phone-side BLE scanner + GATT client controller.
//
// Implements [GattServerController] for the inverted role pair
// mandated by SPEC §3.2 D8: the **desktop** advertises a rotating
// session-bound UUID; the **phone** scans + connects. The interface
// name `GattServerController` predates DEV-003 — the abstraction
// kept because `SyauthCompanionService` and the instrumented
// `CdmLifecycleTest` consult it directly; the semantics are now
// "BLE controller for one association", regardless of whether the
// underlying impl is server-hosted (legacy) or client-driven (this
// one).
//
// Production lifecycle:
//
//   start(association, onChallenge):
//     1. Compute the rotating service UUID set the phone accepts —
//        `[sessionUuidForBond(bondKey, currentMinute),
//          sessionUuidForBond(bondKey, currentMinute - 1)]`.
//        The previous-minute slot is included to absorb up to one
//        minute of negative clock skew between desktop and phone.
//     2. Begin a `BluetoothLeScanner` scan with `ScanFilter`s
//        matching the slot UUIDs.
//     3. On `onScanResult(SCAN_FOUND)` matching one of the slots,
//        open a `BluetoothGatt` client connection.
//     4. Discover services → subscribe to the challenge
//        characteristic via CCCD write.
//     5. On `onCharacteristicChanged(challenge)`, hand the bytes to
//        `onChallenge(peerId, frameBytes)`.
//
//   stop():
//     Tear down the scanner and any open GATT client connection.
//
// Test seams: every Android platform dependency
// (`BluetoothLeScanner`, `BluetoothGatt`) lives behind a small
// interface so the JVM unit tests in
// `app/src/test/kotlin/.../bg/BleScanControllerTest.kt` can construct
// fakes and observe the controller surface without standing up a
// real BLE stack.
package com.sy.syauth.android.bg

import android.bluetooth.le.ScanFilter
import android.companion.AssociationInfo
import android.os.ParcelUuid
import android.util.Log
import java.util.UUID
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

/** Logcat tag for the scanner controller. */
internal const val BLE_SCAN_CONTROLLER_LOG_TAG: String = "syauth.scan"

/** Number of seconds in one wall-clock minute. */
internal const val SECONDS_PER_MINUTE: Long = 60L

/** Number of bytes in a 128-bit UUID. */
internal const val UUID_BYTE_LEN: Int = 16

/**
 * Compute the rotating slot UUIDs the phone is willing to accept for
 * a given `bondKey` at the current wall-clock time. The current
 * minute is included always; the previous minute is included as a
 * skew-absorption slot. Computation runs through the UniFFI
 * `sessionUuidForBond` surface so the bytes are byte-identical to
 * the desktop's `syauth_transport::session_uuid_for`.
 *
 * Pure function — `nowEpochSeconds` is injected so unit tests can
 * pin the slot pair without depending on the OS clock.
 */
public fun slotUuidsFor(
    bondKey: ByteArray,
    nowEpochSeconds: Long,
    sessionUuidLookup: (ByteArray, Long) -> ByteArray,
): List<UUID> {
    val currentMinute = nowEpochSeconds / SECONDS_PER_MINUTE
    val previousMinute = currentMinute - 1L
    val current = uuidFromBytes(sessionUuidLookup(bondKey, currentMinute))
    val previous = uuidFromBytes(sessionUuidLookup(bondKey, previousMinute))
    return listOf(current, previous)
}

/** Wrap 16 little-endian bytes into a `java.util.UUID`. */
internal fun uuidFromBytes(bytes: ByteArray): UUID {
    if (bytes.size != UUID_BYTE_LEN) {
        return UUID(0L, 0L)
    }
    var msb = 0L
    var lsb = 0L
    for (i in 0 until UUID_BYTE_LEN / 2) {
        msb = (msb shl UUID_BYTE_SHIFT) or (bytes[i].toLong() and UUID_BYTE_MASK)
    }
    for (i in UUID_BYTE_LEN / 2 until UUID_BYTE_LEN) {
        lsb = (lsb shl UUID_BYTE_SHIFT) or (bytes[i].toLong() and UUID_BYTE_MASK)
    }
    return UUID(msb, lsb)
}

/** Number of bits per packed byte in the UUID assembly. */
internal const val UUID_BYTE_SHIFT: Int = 8

/** Mask used to widen a signed byte to its unsigned long representation. */
internal const val UUID_BYTE_MASK: Long = 0xFFL

/**
 * Provider that resolves a bond's `bondKey` bytes from the
 * persistent bond store. Production wires the disk-backed
 * `BondStore.load` path; tests inject a fixed mapping.
 */
public fun interface AssociationBondKeyResolver {
    public fun bondKeyFor(association: AssociationInfo?): ByteArray?
}

/**
 * Provider that resolves a bond's stable `peerId` for the
 * `onChallenge` callback. Distinct from the BLE-layer device
 * address so the dispatched intent carries the bond's logical
 * identity, not the radio one.
 */
public fun interface AssociationPeerIdResolver {
    public fun peerIdFor(association: AssociationInfo?): String?
}

/**
 * Source of wall-clock time the controller uses to compute the
 * current minute. Production injects `System::currentTimeMillis`;
 * tests pin a fixed value.
 */
public fun interface EpochSecondsSource {
    public fun nowEpochSeconds(): Long
}

/**
 * Minimal abstraction over the Android `BluetoothLeScanner` surface
 * the controller drives. Production wraps the real platform
 * scanner; tests inject a fake to observe filter sets and drive
 * synthetic scan callbacks.
 */
public interface BleScannerHandle {
    /**
     * Start a scan filtered on the given `filters` slot UUIDs. The
     * controller installs a [BleScanCallback] that fires on every
     * filter match.
     */
    public fun startScan(filters: List<ScanFilter>, callback: BleScanCallback)

    /** Stop the active scan. */
    public fun stopScan(callback: BleScanCallback)
}

/**
 * Trimmed scan callback the controller uses. Production wraps the
 * real `ScanCallback` lifecycle; tests call the synthetic shape
 * directly.
 */
public fun interface BleScanCallback {
    public fun onAdvertisementMatch(matchedUuid: UUID, deviceAddress: String)
}

/**
 * Minimal abstraction over the per-device `BluetoothGatt` client
 * connection. Production wraps `BluetoothDevice.connectGatt` and the
 * resulting `BluetoothGatt`; tests inject a fake that delivers a
 * synthetic challenge through `simulateChallenge`.
 */
public interface BleGattClientHandle {
    /** Open a GATT client connection to `deviceAddress`. */
    public fun connect(deviceAddress: String, callback: BleGattClientCallback)

    /** Disconnect + release the GATT client. */
    public fun disconnect()
}

/**
 * Callback the controller installs on every GATT client connection.
 * Methods are called by the production wrapper from
 * `BluetoothGattCallback` events; tests invoke them directly.
 */
public interface BleGattClientCallback {
    public fun onChallengeReceived(frameBytes: ByteArray)
    public fun onConnectionFailed(reason: String)
}

/**
 * Factory for building per-association handles. Production opens
 * real Android platform handles; tests inject fakes. Kept as two
 * separate factories so the scanner and client can be substituted
 * independently.
 */
public fun interface BleScannerFactory {
    public fun open(): BleScannerHandle?
}
public fun interface BleGattClientFactory {
    public fun open(): BleGattClientHandle?
}

/**
 * Production [GattServerController] for the post-DEV-003 phone-side
 * BLE flow.
 *
 * Every external dependency is injected so JVM unit tests can
 * substitute fakes without standing up the Android Bluetooth stack.
 */
public class SyauthBleScannerController(
    private val bondKeyResolver: AssociationBondKeyResolver,
    private val peerIdResolver: AssociationPeerIdResolver,
    private val clock: EpochSecondsSource,
    private val sessionUuidLookup: (ByteArray, Long) -> ByteArray,
    private val scannerFactory: BleScannerFactory,
    private val gattClientFactory: BleGattClientFactory,
) : GattServerController {

    private val started: AtomicBoolean = AtomicBoolean(false)
    private val scanner: AtomicReference<BleScannerHandle?> = AtomicReference(null)
    private val gattClient: AtomicReference<BleGattClientHandle?> = AtomicReference(null)
    private val scanCallback: AtomicReference<BleScanCallback?> = AtomicReference(null)
    private val gattCallback: AtomicReference<BleGattClientCallback?> = AtomicReference(null)

    /**
     * Slot UUIDs the controller is currently filtering for. Exposed
     * for tests so they can assert the (current, previous) slot
     * inclusion without parsing a `ScanFilter`.
     */
    @Volatile
    internal var lastFilterUuids: List<UUID> = emptyList()
        private set

    override fun start(
        association: AssociationInfo?,
        onChallenge: (peerId: String, frameBytes: ByteArray) -> Unit,
    ) {
        if (!started.compareAndSet(false, true)) {
            return
        }
        val bondKey = bondKeyResolver.bondKeyFor(association)
        if (bondKey == null) {
            Log.w(BLE_SCAN_CONTROLLER_LOG_TAG, "no bond key for association; staying idle")
            started.set(false)
            return
        }
        val peerId = peerIdResolver.peerIdFor(association)
        if (peerId == null) {
            Log.w(BLE_SCAN_CONTROLLER_LOG_TAG, "no peer id for association; staying idle")
            started.set(false)
            return
        }
        val handle = scannerFactory.open()
        if (handle == null) {
            Log.w(BLE_SCAN_CONTROLLER_LOG_TAG, "scanner unavailable on this adapter; staying idle")
            started.set(false)
            return
        }
        val slotUuids = slotUuidsFor(bondKey, clock.nowEpochSeconds(), sessionUuidLookup)
        lastFilterUuids = slotUuids
        val filters = slotUuids.map { uuid ->
            ScanFilter.Builder().setServiceUuid(ParcelUuid(uuid)).build()
        }
        val onScanMatch = BleScanCallback { matchedUuid, deviceAddress ->
            Log.i(BLE_SCAN_CONTROLLER_LOG_TAG, "matched uuid=$matchedUuid addr=$deviceAddress")
            openClient(deviceAddress = deviceAddress, peerId = peerId, onChallenge = onChallenge)
        }
        scanner.set(handle)
        scanCallback.set(onScanMatch)
        handle.startScan(filters, onScanMatch)
    }

    private fun openClient(
        deviceAddress: String,
        peerId: String,
        onChallenge: (peerId: String, frameBytes: ByteArray) -> Unit,
    ) {
        val client = gattClientFactory.open() ?: run {
            Log.w(BLE_SCAN_CONTROLLER_LOG_TAG, "gatt client factory returned null; dropping match")
            return
        }
        val onConnect = object : BleGattClientCallback {
            override fun onChallengeReceived(frameBytes: ByteArray) {
                onChallenge(peerId, frameBytes)
            }
            override fun onConnectionFailed(reason: String) {
                Log.w(BLE_SCAN_CONTROLLER_LOG_TAG, "gatt client failed addr=$deviceAddress reason=$reason")
            }
        }
        gattClient.set(client)
        gattCallback.set(onConnect)
        client.connect(deviceAddress, onConnect)
    }

    override fun stop() {
        if (!started.compareAndSet(true, false)) {
            return
        }
        val handle = scanner.getAndSet(null)
        val cb = scanCallback.getAndSet(null)
        if (handle != null && cb != null) {
            handle.stopScan(cb)
        }
        gattCallback.set(null)
        val client = gattClient.getAndSet(null)
        client?.disconnect()
    }
}
