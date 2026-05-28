// Roadmap item S-010 — persistent GATT client for the phone-side
// foreground service. After S-013 this is the sole Android-side GATT
// client implementation; the legacy direct-GATT sibling has been
// retired.
//
// Why this exists:
//   * SPEC §3 Decisions row "Phone connection lifecycle" mandates
//     one persistent `BluetoothGatt` per bonded peer, opened with
//     `autoConnect=true` and `TRANSPORT_LE`. That instructs the
//     Android BLE stack to maintain (and silently re-establish)
//     the link across out-of-range / sleep transitions — without
//     app code running, without app-throttling — so the unlock
//     latency budget (SPEC §4.3, < 2.0 s) is reachable.
//   * On `onServicesDiscovered` (which the stack invokes after every
//     fresh connect, including post-reconnect) the client
//     subscribes to the challenge characteristic via
//     `setCharacteristicNotification(challenge, true)` plus a CCCD
//     descriptor write of `CCCD_ENABLE_NOTIFY`.
//   * Every challenge frame the desktop notifies arrives in
//     `onCharacteristicChanged` and is forwarded verbatim to the
//     constructor-supplied `onChallenge(peerId, frameBytes)`
//     callback. Both the pre-API-33 and API-33+ override forms are
//     honored.
//   * `writeResponse(frameBytes)` writes the signed bytes back on
//     the response characteristic so the Approve flow can complete
//     the unlock from the same GATT handle.
//
// The `GattOpener` seam exists because Robolectric 4.11.1's
// `ShadowBluetoothDevice` does not expose a `getAutoConnect()`
// getter on the `connectGatt(...)` call — without the seam, the
// DoD test `auto_connect_true_passed_to_connectGatt` could not
// mechanically pin the autoConnect=true contract.
//
// Constants `CCCD_UUID` and `CCCD_ENABLE_NOTIFY` are declared at
// file scope so the class owns its own GATT-layer constants without
// reaching into a sibling file.
package com.sy.syauth.android.bg

import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCallback
import android.bluetooth.BluetoothGattCharacteristic
import android.bluetooth.BluetoothGattDescriptor
import android.bluetooth.BluetoothProfile
import android.content.Context
import android.os.Handler
import android.os.Looper
import android.util.Log
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

/**
 * Roadmap item S-014 — process-local registry of per-peer
 * [PersistentGattClient] instances `MainActivity.installPersistentClientFactory`
 * populates at construction time. The
 * `ChallengeApprovalActivity.cancelSink` looks the per-peer client up
 * here and calls `writeResponse(deniedFrameBytes)` to send the denied
 * frame back on the response characteristic of the same GATT link
 * the challenge arrived on. Tests reset the registry between cases.
 */
public object PersistentGattClientRegistry {
    private val clients: ConcurrentHashMap<String, PersistentGattClient> =
        ConcurrentHashMap()

    public fun put(peerId: String, client: PersistentGattClient) {
        clients[peerId] = client
    }

    public fun lookup(peerId: String): PersistentGattClient? = clients[peerId]

    public fun remove(peerId: String) {
        clients.remove(peerId)
    }

    /** Reset state for tests. */
    public fun reset() {
        clients.clear()
    }
}

/**
 * Seam that wraps `BluetoothDevice.connectGatt(...)` so the
 * `autoConnect` flag is mechanically observable in tests.
 * Production binds [DefaultGattOpener] which delegates to the
 * 4-arg `BluetoothDevice.connectGatt(context, autoConnect,
 * callback, TRANSPORT_LE)` overload (the minSdk-26 floor).
 */
internal fun interface GattOpener {
    fun open(
        device: BluetoothDevice,
        autoConnect: Boolean,
        callback: BluetoothGattCallback,
    ): BluetoothGatt?
}

/**
 * Production [GattOpener]: routes the call through
 * `BluetoothDevice.connectGatt(context, autoConnect, callback,
 * TRANSPORT_LE)`. Pinned to `TRANSPORT_LE` so the stack never
 * negotiates BR/EDR for our LE-only profile.
 */
internal class DefaultGattOpener(private val context: Context) : GattOpener {
    override fun open(
        device: BluetoothDevice,
        autoConnect: Boolean,
        callback: BluetoothGattCallback,
    ): BluetoothGatt? = device.connectGatt(
        context,
        autoConnect,
        callback,
        BluetoothDevice.TRANSPORT_LE,
    )
}

/**
 * Persistent GATT client for one bonded peer.
 *
 * `start()` is idempotent: a second call while a handle is already
 * open is a no-op. `stop()` is idempotent: a second call after
 * close is a no-op. `writeResponse(bytes)` is safe to call when
 * the client is stopped — it returns `false` without throwing.
 *
 * @param context           Used by `BluetoothDevice.connectGatt`.
 * @param adapter           Resolves the bonded peer's MAC.
 * @param peerId            Bond's stable identifier (the desktop
 *                          MAC per `BondRecord.peerId`); forwarded
 *                          verbatim to `onChallenge`.
 * @param deviceMac         The bonded peer's BLE MAC address.
 * @param onChallenge       Invoked with `(peerId, frameBytes)` for
 *                          every challenge frame the desktop
 *                          notifies on
 *                          [SYAUTH_CHALLENGE_CHAR_UUID].
 * @param gattOpener        Test seam; defaults to
 *                          [DefaultGattOpener].
 */
public class PersistentGattClient internal constructor(
    private val context: Context,
    private val adapter: BluetoothAdapter,
    private val peerId: String,
    private val deviceMac: String,
    private val onChallenge: (peerId: String, frameBytes: ByteArray) -> Unit,
    private val gattOpener: GattOpener,
) {

    /**
     * Public constructor used by production code. Binds the default
     * `connectGatt` opener.
     */
    public constructor(
        context: Context,
        adapter: BluetoothAdapter,
        peerId: String,
        deviceMac: String,
        onChallenge: (peerId: String, frameBytes: ByteArray) -> Unit,
    ) : this(
        context = context,
        adapter = adapter,
        peerId = peerId,
        deviceMac = deviceMac,
        onChallenge = onChallenge,
        gattOpener = DefaultGattOpener(context),
    )

    private val gatt: AtomicReference<BluetoothGatt?> = AtomicReference(null)

    /**
     * Set to `true` while the client is intentionally torn down via
     * [stop]. Consulted by the disconnect-watchdog so a watchdog
     * tick scheduled before `stop()` cannot revive the connection
     * after the service decided to shut it down.
     */
    private val stopped: AtomicBoolean = AtomicBoolean(false)

    /**
     * Handler bound to the main looper, owned by the client. The
     * disconnect-watchdog posts itself here on every
     * `STATE_DISCONNECTED` and is removed on every `STATE_CONNECTED`.
     * The main looper is fine for this — the runnable just calls
     * `forceReconnect()` which dispatches to the BLE stack's own
     * threads.
     */
    private val reconnectHandler: Handler = Handler(Looper.getMainLooper())

    /**
     * Watchdog that re-issues a fresh `connectGatt` if we are still
     * disconnected after [RECONNECT_INTERVAL_MS]. Android's
     * `autoConnect=true` background scan has a very low duty cycle —
     * after Doze, screen-off, or a long out-of-range absence it can
     * take many minutes to re-acquire the peer on its own. A fresh
     * `connectGatt` call resets Android's scan timer, so a periodic
     * `forceReconnect()` from a watchdog converts that into a
     * deterministic ~RECONNECT_INTERVAL_MS recovery window.
     *
     * The watchdog re-arms itself through `forceReconnect()` →
     * `start()` (which schedules the next tick), so a single tick
     * keeps the cadence going until the link is up. Successful
     * reconnects clear it via `STATE_CONNECTED` in the callback, and
     * `stop()` clears it directly. The `stopped` guard prevents a
     * tick that fires concurrently with `stop()` from reopening the
     * link.
     */
    private val reconnectRunnable: Runnable = object : Runnable {
        override fun run() {
            if (stopped.get()) return
            Log.i(
                PERSISTENT_GATT_LOG_TAG,
                "watchdog: still disconnected after ${RECONNECT_INTERVAL_MS}ms — forcing reconnect"
            )
            forceReconnect()
        }
    }

    /**
     * Open the persistent GATT connection with `autoConnect=true`.
     * The OS will hold the connection across range transitions
     * without further app calls. Idempotent.
     */
    public fun start() {
        stopped.set(false)
        if (gatt.get() != null) return
        val device = runCatching { adapter.getRemoteDevice(deviceMac) }.getOrNull()
        if (device == null) {
            Log.w(PERSISTENT_GATT_LOG_TAG, "start: getRemoteDevice($deviceMac) null")
            return
        }
        Log.i(PERSISTENT_GATT_LOG_TAG, "start: opening autoConnect=true to $deviceMac")
        val handle = gattOpener.open(device, AUTO_CONNECT_TRUE, gattCallback)
        if (handle == null) {
            Log.w(PERSISTENT_GATT_LOG_TAG, "start: connectGatt returned null")
            return
        }
        gatt.set(handle)
        // BUG-20260528-0130 → BUG-20260528-2334: arm the reconnect
        // watchdog NOW, not only on STATE_DISCONNECTED. A
        // connectGatt(autoConnect=true) that never completes emits NO
        // onConnectionStateChange callback at all (e.g. the desktop was
        // mid-`serve_gatt_application` re-registration, briefly out of
        // range, or Android's low-duty-cycle background scan stalled),
        // so the disconnected-path scheduling never runs and the client
        // would wedge forever in a never-completing scan — the desktop
        // then audits notifier_slot=None / transport-error on every
        // unlock. A fast successful connect cancels this pending tick via
        // STATE_CONNECTED before it fires; a stalled connect is retried.
        reconnectHandler.removeCallbacks(reconnectRunnable)
        reconnectHandler.postDelayed(reconnectRunnable, RECONNECT_INTERVAL_MS)
    }

    /**
     * Tear down the GATT connection. Idempotent — a second call
     * after close is a no-op.
     */
    public fun stop() {
        stopped.set(true)
        reconnectHandler.removeCallbacks(reconnectRunnable)
        val handle = gatt.getAndSet(null) ?: return
        runCatching { handle.disconnect() }
        runCatching { handle.close() }
        Log.i(PERSISTENT_GATT_LOG_TAG, "stopped")
    }

    /**
     * Force a fresh GATT handshake against the bonded peer.
     *
     * Used by [SyauthCompanionService] when CDM presence transitions
     * `absent -> present` — the desktop daemon almost certainly
     * restarted (`syauth-presenced` re-registers its GATT app on
     * boot), and our cached service tree + CCCD subscription are now
     * bound to the dead application registration. BlueZ does not
     * broadcast a Service Changed indication on
     * `serve_gatt_application` re-registration, so the Android stack
     * never auto-invalidates the cache on its own; without an
     * explicit teardown we silently miss every challenge that follows.
     *
     * Sequence: disconnect → close → reopen with the same
     * `autoConnect=true` semantics as [start]. Idempotent; safe to
     * call whether or not the client was previously started.
     */
    public fun forceReconnect() {
        Log.i(PERSISTENT_GATT_LOG_TAG, "forceReconnect: tearing down stale GATT")
        val handle = gatt.getAndSet(null)
        if (handle != null) {
            runCatching { handle.disconnect() }
            runCatching { handle.close() }
        }
        start()
    }

    /**
     * Reflective wrapper around `BluetoothGatt.refresh()`. The method
     * is `@hide` on AOSP but stable across every release since
     * Android 4.x — it clears the per-device GATT service cache the
     * OS keeps under `/data/misc/bluetooth/`. Without this, a fresh
     * `connectGatt` will hand back the cached (stale) services and
     * `discoverServices` is a no-op against the OS-level cache.
     *
     * Returns true on success, false on any reflection failure. We
     * never throw — a failed refresh just means the next
     * `discoverServices` may return cached data, which the
     * absent→present-driven forceReconnect compensates for.
     */
    private fun refreshGattCache(handle: BluetoothGatt): Boolean {
        return runCatching {
            val method = handle.javaClass.getMethod("refresh")
            (method.invoke(handle) as? Boolean) ?: false
        }.getOrElse { err ->
            Log.w(PERSISTENT_GATT_LOG_TAG, "gatt.refresh() reflection failed", err)
            false
        }
    }

    /**
     * Write the signed response frame onto the response
     * characteristic of the open GATT. Returns the stack's verdict
     * for `gatt.writeCharacteristic`. Returns `false` (without
     * throwing) when the client is not started or the response
     * characteristic is not present.
     */
    public fun writeResponse(frameBytes: ByteArray): Boolean {
        val handle = gatt.get() ?: return false
        val c = findCharacteristic(handle, SYAUTH_RESPONSE_CHAR_UUID) ?: return false
        c.value = frameBytes
        return runCatching { handle.writeCharacteristic(c) }.getOrDefault(false)
    }

    private val gattCallback: BluetoothGattCallback = object : BluetoothGattCallback() {
        override fun onConnectionStateChange(g: BluetoothGatt, status: Int, newState: Int) {
            Log.i(PERSISTENT_GATT_LOG_TAG, "conn state status=$status new=$newState")
            when (newState) {
                BluetoothProfile.STATE_CONNECTED -> {
                    // Cancel any pending reconnect watchdog — we are
                    // healthy again. (No-op if none was scheduled.)
                    reconnectHandler.removeCallbacks(reconnectRunnable)
                    // Always invalidate the on-disk GATT service cache before
                    // re-discovering. The desktop daemon may have re-registered
                    // its GATT app while we held the link alive (e.g. an apt
                    // upgrade or a `systemctl restart syauth-presenced`); the
                    // cached handles point at a dead registration, and BlueZ
                    // does not send a Service Changed indication on
                    // `serve_gatt_application` swap. `refresh()` clears the
                    // cache so the upcoming `discoverServices()` actually
                    // talks to the wire.
                    val refreshed = refreshGattCache(g)
                    Log.i(PERSISTENT_GATT_LOG_TAG, "conn state: gatt.refresh()=$refreshed; discovering")
                    g.discoverServices()
                }
                BluetoothProfile.STATE_DISCONNECTED -> {
                    // Arm the watchdog. autoConnect=true alone is too lazy
                    // for field reality (Doze + long out-of-range absences
                    // routinely take minutes to re-acquire). The watchdog
                    // forces a fresh `connectGatt` every RECONNECT_INTERVAL_MS
                    // which resets Android's scan timer and gives a
                    // deterministic recovery window once the peer is back
                    // in range. Cancelled on STATE_CONNECTED above.
                    if (!stopped.get()) {
                        Log.i(
                            PERSISTENT_GATT_LOG_TAG,
                            "conn state: disconnected; scheduling watchdog in ${RECONNECT_INTERVAL_MS}ms"
                        )
                        reconnectHandler.removeCallbacks(reconnectRunnable)
                        reconnectHandler.postDelayed(reconnectRunnable, RECONNECT_INTERVAL_MS)
                    }
                }
            }
        }

        override fun onServicesDiscovered(g: BluetoothGatt, status: Int) {
            Log.i(PERSISTENT_GATT_LOG_TAG, "services discovered status=$status n=${g.services.size}")
            val challenge = findCharacteristic(g, SYAUTH_CHALLENGE_CHAR_UUID)
            if (challenge == null) {
                Log.w(PERSISTENT_GATT_LOG_TAG, "challenge characteristic not present")
                return
            }
            val cccd = challenge.getDescriptor(CCCD_UUID)
            if (cccd == null) {
                Log.w(PERSISTENT_GATT_LOG_TAG, "challenge characteristic has no CCCD")
                return
            }
            if (!g.setCharacteristicNotification(challenge, true)) {
                Log.w(PERSISTENT_GATT_LOG_TAG, "setCharacteristicNotification false")
            }
            cccd.value = CCCD_ENABLE_NOTIFY
            val ok = runCatching { g.writeDescriptor(cccd) }.getOrDefault(false)
            Log.i(PERSISTENT_GATT_LOG_TAG, "subscribed: writeDescriptor=$ok")
        }

        @Deprecated("Pre-API-33 onCharacteristicChanged; we honor both forms.")
        override fun onCharacteristicChanged(
            g: BluetoothGatt,
            characteristic: BluetoothGattCharacteristic,
        ) {
            if (characteristic.uuid != SYAUTH_CHALLENGE_CHAR_UUID) return
            val bytes = characteristic.value ?: return
            Log.i(PERSISTENT_GATT_LOG_TAG, "challenge frame received len=${bytes.size}")
            onChallenge(peerId, bytes)
        }

        override fun onCharacteristicChanged(
            g: BluetoothGatt,
            characteristic: BluetoothGattCharacteristic,
            value: ByteArray,
        ) {
            if (characteristic.uuid != SYAUTH_CHALLENGE_CHAR_UUID) return
            Log.i(PERSISTENT_GATT_LOG_TAG, "challenge frame received (api33) len=${value.size}")
            onChallenge(peerId, value)
        }

        override fun onDescriptorWrite(
            g: BluetoothGatt,
            descriptor: BluetoothGattDescriptor,
            status: Int,
        ) {
            Log.i(PERSISTENT_GATT_LOG_TAG, "descriptor write status=$status uuid=${descriptor.uuid}")
        }
    }

    private fun findCharacteristic(
        g: BluetoothGatt,
        uuid: UUID,
    ): BluetoothGattCharacteristic? {
        for (service in g.services) {
            val ch = service.getCharacteristic(uuid)
            if (ch != null) return ch
        }
        return null
    }

    public companion object {
        /** Logcat tag used by every span the client emits. */
        internal const val PERSISTENT_GATT_LOG_TAG: String = "syauth.bg.persistent"

        /** Client-Characteristic-Configuration descriptor UUID (BT SIG). */
        private val CCCD_UUID: UUID =
            UUID.fromString("00002902-0000-1000-8000-00805f9b34fb")

        /** Bytes to write to a CCCD descriptor to enable notifications. */
        private val CCCD_ENABLE_NOTIFY: ByteArray = byteArrayOf(0x01, 0x00)

        /**
         * Watchdog cadence (milliseconds) for re-issuing a fresh
         * `connectGatt` when the link is in `STATE_DISCONNECTED`.
         * 15 s is short enough that the user's first sudo after
         * returning into range usually finds the link already
         * recovered, and long enough that we do not thrash the BLE
         * stack while the peer is genuinely absent (each fresh
         * `connectGatt` triggers a scan window which has a small but
         * non-zero radio cost).
         */
        internal const val RECONNECT_INTERVAL_MS: Long = 15_000L

        /**
         * The `autoConnect` flag the production code always passes
         * to `BluetoothDevice.connectGatt(...)`. Pinned as a named
         * constant so the contract is one grep away and a future
         * "optimisation" cannot silently flip it to `false`.
         */
        private const val AUTO_CONNECT_TRUE: Boolean = true
    }
}
