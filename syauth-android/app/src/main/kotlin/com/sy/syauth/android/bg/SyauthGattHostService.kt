// syauth — always-on GATT host foreground service.
//
// GAP: DEV-003 — this whole module exists only because the BLE
// advertising direction is inverted vs. SPEC §3.2 D8. The SPEC says
// the desktop advertises a rotating session-bound UUID and the phone
// scans + connects; instead, this service makes the phone host a
// GATT server and advertise a fixed service UUID. Closure plan:
// delete this file; the only foreground service path becomes
// [SyauthCompanionService] (CDM-bound), gated by a real LESC pair
// (closes DEV-001 first). See `docs/known-gaps.md` rows DEV-001 and
// DEV-003.
//
// Routing decision (see "On incoming challenge" below): when the
// desktop writes to the challenge characteristic, we start
// [com.sy.syauth.android.MainActivity] directly with the deep-link
// intent the existing [ApproveNotification] builder produces. This is
// simpler than raising a heads-up notification + full-screen-intent —
// since this service is the temporary phone-advertises path, the
// activity directly handles the approve UI.
package com.sy.syauth.android.bg

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCharacteristic
import android.bluetooth.BluetoothGattDescriptor
import android.bluetooth.BluetoothGattServer
import android.bluetooth.BluetoothGattServerCallback
import android.bluetooth.BluetoothGattService
import android.bluetooth.BluetoothManager
import android.bluetooth.BluetoothProfile
import android.bluetooth.BluetoothStatusCodes
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.annotation.RequiresApi
import androidx.core.app.NotificationCompat
import com.sy.syauth.android.provision.BondRecord
import com.sy.syauth.android.provision.bootstrapBond
import java.util.concurrent.atomic.AtomicReference

/** Foreground notification id for the host service. */
public const val HOST_NOTIFICATION_ID: Int = 0x5A41

/** Channel id for the foreground notification. Low-importance: ambient ready-state. */
public const val HOST_NOTIFICATION_CHANNEL_ID: String = "syauth.host.channel"

/** Human-readable channel name surfaced in system Settings. */
public const val HOST_NOTIFICATION_CHANNEL_NAME: String = "syauth host (ready)"

/** Content text of the always-on foreground notification. */
public const val HOST_NOTIFICATION_TEXT: String = "syauth host — ready"

/** Logcat tag the host service uses for every span. */
public const val HOST_SERVICE_LOG_TAG: String = "syauth.host"

/**
 * Factory for the per-launch [GattServerController]. Production opens
 * a real `BluetoothGattServer`; tests inject a fake to observe the
 * controller surface without a radio.
 */
public fun interface HostControllerFactory {
    public fun create(context: Context, record: BondRecord): GattServerController
}

/**
 * Adapter so the service can fire an intent at MainActivity without
 * statically referencing the class (so unit tests can swap it for a
 * recorder).
 */
public fun interface ApproveIntentDispatcher {
    public fun dispatch(context: Context, intent: Intent)
}

/**
 * Default [ApproveIntentDispatcher] that calls
 * `context.startActivity(intent)` with the new-task flags every
 * background-launched activity needs.
 */
public class DefaultApproveIntentDispatcher : ApproveIntentDispatcher {
    override fun dispatch(context: Context, intent: Intent) {
        val withFlags = Intent(intent).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP)
        }
        context.startActivity(withFlags)
    }
}

@RequiresApi(Build.VERSION_CODES.O)
public class SyauthGattHostService : Service() {

    /** Active controller (null when service is between start/stop). */
    @Volatile
    private var controller: GattServerController? = null

    /** Active bond record (null when bootstrap failed and we are exiting). */
    @Volatile
    private var bondRecord: BondRecord? = null

    /** Registered transport peer id, so onDestroy can unregister it. */
    @Volatile
    private var registeredPeerId: String? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        val record = bootstrapBond(this)
        if (record == null) {
            Log.w(HOST_SERVICE_LOG_TAG, "no bond record; stopping host service")
            stopSelf()
            return
        }
        bondRecord = record
        installCompanionServiceSeams(record)
        promoteToForeground()
        val factory = controllerFactory ?: defaultControllerFactory()
        val controller = factory.create(this, record)
        this.controller = controller
        // Wire the response transport so the ApproveViewModel's
        // `GattResponseSender` push lands on the live GATT server via
        // `notifyCharacteristicChanged`. The sink is the
        // `BluerlessGattServerController.notifyResponse` method; for
        // test/fake controllers that do not extend the concrete class,
        // the sink degrades to `false` and the transport reports a
        // typed failure.
        val notifySink: (ByteArray) -> Boolean = { bytes ->
            (controller as? BluerlessGattServerController)?.notifyResponse(bytes) ?: false
        }
        val transport = HostGattResponseTransport(peerId = record.peerId, notifySink = notifySink)
        GattResponseTransports.register(record.peerId, transport)
        registeredPeerId = record.peerId
        controller.start(association = null) { peerId, frameBytes ->
            handleChallenge(record = record, peerId = peerId, frameBytes = frameBytes)
        }
        Log.i(HOST_SERVICE_LOG_TAG, "host service started for peer=${record.peerId}")
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // Sticky so the OS re-creates us after a process kill while a
        // bond exists. We re-bootstrap from disk on every start.
        return START_STICKY
    }

    override fun onDestroy() {
        super.onDestroy()
        controller?.stop()
        controller = null
        val peerId = registeredPeerId
        if (peerId != null) {
            GattResponseTransports.unregister(peerId)
            registeredPeerId = null
        }
        bondRecord = null
    }

    private fun installCompanionServiceSeams(record: BondRecord) {
        // The existing companion-service seams are global and read by
        // the ApproveViewModel path. Installing them here means the
        // approve-route signing path uses the same bond key + verifier
        // as the CDM-bound flow once that lands.
        SyauthCompanionService.bondKeyProvider = BondKeyProvider { peerId ->
            if (peerId == record.peerId) record.bondKey else null
        }
        SyauthCompanionService.hostnameResolver = HostnameResolver { peerId ->
            if (peerId == record.peerId) record.hostName else peerId
        }
        if (SyauthCompanionService.challengeVerifier == null) {
            SyauthCompanionService.challengeVerifier = UniffiChallengeVerifier()
        }
    }

    private fun handleChallenge(record: BondRecord, peerId: String, frameBytes: ByteArray) {
        val verifier = SyauthCompanionService.challengeVerifier ?: UniffiChallengeVerifier()
        val payload = verifier.verify(record.bondKey, frameBytes)
        if (payload == null) {
            Log.w(HOST_SERVICE_LOG_TAG, "frame verify failed peer=$peerId; dropping")
            return
        }
        // The BLE callback parameter `peerId` carries the desktop's MAC
        // address, not the bond's logical peer_id. Use the bond's
        // peer_id in the dispatched intent so the approve route's
        // `GattResponseSender.lookup(peerId)` resolves to the
        // transport we registered under `record.peerId`. The MAC has
        // no business being on the wire — the bond key is the
        // cryptographic identity.
        val effectivePeerId = record.peerId
        val intent = ApproveNotification.buildApproveIntent(
            context = this,
            challengeBytes = frameBytes,
            hostname = record.hostName,
            peerId = effectivePeerId,
        )
        val dispatcher = approveIntentDispatcher ?: DefaultApproveIntentDispatcher()
        runCatching { dispatcher.dispatch(this, intent) }
            .onFailure {
                Log.w(HOST_SERVICE_LOG_TAG, "could not start MainActivity: ${it.message}")
            }
        Log.i(
            HOST_SERVICE_LOG_TAG,
            "challenge dispatched peer=$effectivePeerId payloadLen=${payload.size}",
        )
    }

    private fun defaultControllerFactory(): HostControllerFactory =
        HostControllerFactory { context, _ ->
            val manager = context.getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager
                ?: error("BluetoothManager unavailable; cannot start GATT host")
            BluerlessGattServerController(
                handleFactory = { BluetoothGattServerHandle.open(context, manager) },
                advertiserFactory = {
                    val adapter = manager.adapter
                    val advertiser = adapter?.bluetoothLeAdvertiser
                    if (advertiser == null) {
                        null
                    } else {
                        BluetoothLeAdvertiserHandle(advertiser)
                    }
                },
            )
        }

    private fun promoteToForeground() {
        val notification = buildForegroundNotification()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            startForeground(
                HOST_NOTIFICATION_ID,
                notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE,
            )
        } else {
            startForeground(HOST_NOTIFICATION_ID, notification)
        }
    }

    private fun buildForegroundNotification(): Notification {
        ensureChannel(this)
        return NotificationCompat.Builder(this, HOST_NOTIFICATION_CHANNEL_ID)
            .setContentTitle(HOST_NOTIFICATION_TEXT)
            .setSmallIcon(android.R.drawable.ic_lock_idle_lock)
            .setOngoing(true)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .build()
    }

    private fun ensureChannel(context: Context) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val channel = NotificationChannel(
            HOST_NOTIFICATION_CHANNEL_ID,
            HOST_NOTIFICATION_CHANNEL_NAME,
            NotificationManager.IMPORTANCE_LOW,
        )
        val manager = context.getSystemService(NotificationManager::class.java)
        manager?.createNotificationChannel(channel)
    }

    public companion object {
        /**
         * Override for the controller factory. Tests install a fake;
         * production uses the default opener that consults
         * `BluetoothManager`.
         */
        @Volatile
        public var controllerFactory: HostControllerFactory? = null

        /** Override for the activity-start dispatcher. */
        @Volatile
        public var approveIntentDispatcher: ApproveIntentDispatcher? = null

        /** Reset seams (used by tests between cases). */
        public fun resetSeams() {
            controllerFactory = null
            approveIntentDispatcher = null
        }
    }
}

/**
 * Response transport used by the always-on host service. Delegates the
 * actual notify-characteristic write to a sink function the host
 * service wires to `BluerlessGattServerController.notifyResponse`. The
 * controller calls `BluetoothGattServer.notifyCharacteristicChanged`
 * with the connected desktop as the recipient.
 */
private class HostGattResponseTransport(
    private val peerId: String,
    private val notifySink: (ByteArray) -> Boolean,
) : GattResponseTransport {
    override suspend fun pushApprove(bytes: ByteArray): Result<Unit> {
        val ok = notifySink(bytes)
        return if (ok) {
            Log.i(
                HOST_SERVICE_LOG_TAG,
                "host transport approve peer=$peerId len=${bytes.size}",
            )
            Result.success(Unit)
        } else {
            Log.w(
                HOST_SERVICE_LOG_TAG,
                "host transport approve REJECTED peer=$peerId; no peer connected or notify failed",
            )
            Result.failure(ResponseSendError.GattWriteFailed("notify rejected"))
        }
    }

    override suspend fun pushDeny(): Result<Unit> {
        // Deny is signalled by a frame the ApproveViewModel constructs;
        // the wire push reuses pushApprove since the desktop only
        // distinguishes by frame contents, not by descriptor.
        Log.i(HOST_SERVICE_LOG_TAG, "host transport deny peer=$peerId")
        return Result.success(Unit)
    }
}

/**
 * Production adapter wrapping `BluetoothGattServer`. Owns a real
 * `BluetoothGattServerCallback` that tracks the connected desktop and
 * delivers `onCharacteristicWriteRequest` payloads into the
 * controller-installed write handler. `notifyResponse` calls
 * `notifyCharacteristicChanged` on the same server using the tracked
 * device + response characteristic — that is what surfaces bytes on
 * the desktop's bluer notify stream.
 */
private class BluetoothGattServerHandle private constructor() : GattServerHandle {
    private val serverRef: AtomicReference<BluetoothGattServer?> = AtomicReference(null)
    private val connectedDevice: AtomicReference<BluetoothDevice?> = AtomicReference(null)
    private val responseChar: AtomicReference<BluetoothGattCharacteristic?> = AtomicReference(null)
    private val writeHandler: AtomicReference<((String, ByteArray) -> Unit)?> = AtomicReference(null)
    private val serviceAddLatch: java.util.concurrent.CountDownLatch = java.util.concurrent.CountDownLatch(1)

    private val callback: BluetoothGattServerCallback = object : BluetoothGattServerCallback() {
        override fun onServiceAdded(status: Int, service: BluetoothGattService) {
            if (status == BluetoothGatt.GATT_SUCCESS && service.uuid == SYAUTH_GATT_SERVICE_UUID) {
                Log.i(HOST_SERVICE_LOG_TAG, "gatt-service registered uuid=${service.uuid}")
            } else {
                Log.w(
                    HOST_SERVICE_LOG_TAG,
                    "gatt-service add failed status=$status uuid=${service.uuid}",
                )
            }
            serviceAddLatch.countDown()
        }

        override fun onConnectionStateChange(device: BluetoothDevice, status: Int, newState: Int) {
            when (newState) {
                BluetoothProfile.STATE_CONNECTED -> {
                    connectedDevice.set(device)
                    Log.i(HOST_SERVICE_LOG_TAG, "gatt-server connected addr=${device.address}")
                }
                BluetoothProfile.STATE_DISCONNECTED -> {
                    val current = connectedDevice.get()
                    if (current?.address == device.address) {
                        connectedDevice.set(null)
                    }
                    Log.i(HOST_SERVICE_LOG_TAG, "gatt-server disconnected addr=${device.address}")
                }
            }
        }

        override fun onCharacteristicWriteRequest(
            device: BluetoothDevice,
            requestId: Int,
            characteristic: BluetoothGattCharacteristic,
            preparedWrite: Boolean,
            responseNeeded: Boolean,
            offset: Int,
            value: ByteArray,
        ) {
            if (characteristic.uuid == SYAUTH_CHALLENGE_CHAR_UUID) {
                connectedDevice.set(device)
                val handler = writeHandler.get()
                if (handler != null) {
                    runCatching { handler(device.address ?: "", value) }
                        .onFailure {
                            Log.w(
                                HOST_SERVICE_LOG_TAG,
                                "challenge handler threw addr=${device.address}: ${it.message}",
                            )
                        }
                } else {
                    Log.w(HOST_SERVICE_LOG_TAG, "challenge write but no handler installed; dropping")
                }
            }
            if (responseNeeded) {
                runCatching {
                    serverRef.get()?.sendResponse(
                        device,
                        requestId,
                        BluetoothGatt.GATT_SUCCESS,
                        offset,
                        null,
                    )
                }
            }
        }

        override fun onDescriptorWriteRequest(
            device: BluetoothDevice,
            requestId: Int,
            descriptor: BluetoothGattDescriptor,
            preparedWrite: Boolean,
            responseNeeded: Boolean,
            offset: Int,
            value: ByteArray,
        ) {
            // CCCD writes: accept any value so the desktop's bluer
            // subscribe-write lands cleanly. The desktop's `notify()`
            // call writes the subscribe bitfield (0x0001); we do not
            // need to gate on the exact value.
            if (descriptor.uuid == CCCD_DESCRIPTOR_UUID) {
                connectedDevice.set(device)
                Log.i(HOST_SERVICE_LOG_TAG, "cccd subscribe from ${device.address}")
            }
            if (responseNeeded) {
                runCatching {
                    serverRef.get()?.sendResponse(
                        device,
                        requestId,
                        BluetoothGatt.GATT_SUCCESS,
                        offset,
                        null,
                    )
                }
            }
        }

        override fun onDescriptorReadRequest(
            device: BluetoothDevice,
            requestId: Int,
            offset: Int,
            descriptor: BluetoothGattDescriptor,
        ) {
            runCatching {
                serverRef.get()?.sendResponse(
                    device,
                    requestId,
                    BluetoothGatt.GATT_SUCCESS,
                    offset,
                    CCCD_DEFAULT_VALUE,
                )
            }
        }
    }

    override fun addService(service: BluetoothGattService): Boolean {
        // Stash the response characteristic now so notifyResponse can
        // call notifyCharacteristicChanged against the same instance
        // the server registered.
        val responseChars = service.characteristics.filter { it.uuid == SYAUTH_RESPONSE_CHAR_UUID }
        responseChars.firstOrNull()?.let { responseChar.set(it) }
        val server = serverRef.get() ?: return false
        val queued = server.addService(service)
        if (!queued) return false
        // BluetoothGattServer.addService is async; onServiceAdded fires
        // when the GATT DB is actually populated. Block here so the
        // controller (and the advertise that follows in
        // BluerlessGattServerController.start) only proceeds once the
        // service is queryable — otherwise the desktop's bluer client
        // wins the race, connects, finds an empty service list, and
        // disconnects.
        val ready = serviceAddLatch.await(
            SERVICE_ADD_TIMEOUT_MILLIS,
            java.util.concurrent.TimeUnit.MILLISECONDS,
        )
        if (!ready) {
            Log.w(
                HOST_SERVICE_LOG_TAG,
                "onServiceAdded did not fire within ${SERVICE_ADD_TIMEOUT_MILLIS}ms; advertising anyway",
            )
        }
        return true
    }

    override fun setWriteHandler(handler: ((String, ByteArray) -> Unit)?) {
        writeHandler.set(handler)
    }

    @Suppress("DEPRECATION")
    override fun notifyResponse(bytes: ByteArray): Boolean {
        val server = serverRef.get() ?: return false
        val device = connectedDevice.get() ?: return false
        val char = responseChar.get() ?: return false
        char.value = bytes
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            val status = server.notifyCharacteristicChanged(device, char, false, bytes)
            status == BluetoothStatusCodes.SUCCESS
        } else {
            server.notifyCharacteristicChanged(device, char, false)
        }
    }

    override fun close() {
        runCatching { serverRef.getAndSet(null)?.close() }
        connectedDevice.set(null)
        responseChar.set(null)
        writeHandler.set(null)
    }

    companion object {
        /**
         * Open a real `BluetoothGattServer` against the supplied
         * manager and return a [BluetoothGattServerHandle] whose
         * callback is wired up at construction time. The chicken-and-
         * egg between callback construction (needs server reference to
         * call `sendResponse`) and server construction (needs callback)
         * is resolved with an `AtomicReference<BluetoothGattServer>`
         * the callback consults lazily.
         */
        fun open(context: Context, manager: BluetoothManager): BluetoothGattServerHandle {
            val handle = BluetoothGattServerHandle()
            val server = manager.openGattServer(context, handle.callback)
                ?: throw IllegalStateException("openGattServer returned null")
            handle.serverRef.set(server)
            return handle
        }
    }
}

/**
 * Default CCCD value returned on a read — "no subscriptions". The
 * desktop overwrites this when it calls `Characteristic::notify()`.
 */
private val CCCD_DEFAULT_VALUE: ByteArray = byteArrayOf(0x00, 0x00)

/**
 * Wall-clock budget for `BluetoothGattServer.onServiceAdded` to fire
 * after `addService`. On a healthy adapter this lands in well under
 * 100ms; the 2-second cap is defensive so a misbehaving stack does
 * not hang service start indefinitely.
 */
private const val SERVICE_ADD_TIMEOUT_MILLIS: Long = 2_000L

/**
 * Production adapter wrapping `BluetoothLeAdvertiser`.
 */
private class BluetoothLeAdvertiserHandle(
    private val advertiser: android.bluetooth.le.BluetoothLeAdvertiser,
) : BleAdvertiserHandle {
    override fun startAdvertising(
        settings: android.bluetooth.le.AdvertiseSettings,
        data: android.bluetooth.le.AdvertiseData,
        callback: android.bluetooth.le.AdvertiseCallback,
    ) {
        advertiser.startAdvertising(settings, data, callback)
    }

    override fun stopAdvertising(callback: android.bluetooth.le.AdvertiseCallback) {
        advertiser.stopAdvertising(callback)
    }
}
