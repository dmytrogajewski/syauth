// DEV-001 (CDM pivot): production [PairCompanionScanner] backed by
// `android.companion.CompanionDeviceManager`.
//
// Why CDM instead of `BluetoothLeScanner.startScan`: Samsung One UI
// on the Galaxy S25 Ultra (Android 15) requires `BLUETOOTH_PRIVILEGED`
// to actually start a scan from a third-party app, even when
// `BLUETOOTH_SCAN(neverForLocation)` is granted. `BLUETOOTH_PRIVILEGED`
// is `signature|privileged` — out of reach for any non-system app.
// CDM runs the BLE scan under system privileges and presents the
// user with a system-rendered device picker; on user pick CDM
// returns the `BluetoothDevice` and authorises the app to talk to
// the picked device without further BLE-scan permission.
//
// Architecture:
//
//   1. The Activity registers an `ActivityResultLauncher<IntentSenderRequest>`
//      at `onCreate` time (see `MainActivity.cdmPickerLauncher`). The
//      launcher's callback decodes the returned `BluetoothDevice`
//      and invokes the callbacks we stashed via [pendingOnPicked] /
//      [pendingOnFailed].
//   2. [associate] builds an [AssociationRequest] with one
//      [BluetoothLeDeviceFilter] per slot UUID (current + previous
//      minute, for skew absorption) and calls
//      `CompanionDeviceManager.associate(request, executor, callback)`.
//   3. The CDM callback fires `onAssociationPending(IntentSender)`;
//      we wrap it in an [IntentSenderRequest] and dispatch to the
//      launcher. The OS shows its picker; on pick, the launcher's
//      callback resolves the pending [onPicked] callback.
//
// API surface: API 33+ exposes the `Executor`-shaped
// `associate(AssociationRequest, Executor, Callback)` overload. The
// app's `minSdk` is 26, but the production callers (the Compose
// pair flow on the connected R5CY214FQHM running API 35) take this
// path; the [associate] function returns a typed failure on
// pre-API-33 hosts.
package com.sy.syauth.android.pair.impl

import android.app.Activity
import android.bluetooth.BluetoothDevice
import android.bluetooth.le.ScanFilter
import android.companion.AssociationInfo
import android.companion.AssociationRequest
import android.companion.BluetoothLeDeviceFilter
import android.companion.CompanionDeviceManager
import android.content.Context
import android.content.Intent
import android.content.IntentSender
import android.os.Build
import android.os.ParcelUuid
import android.util.Log
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.IntentSenderRequest
import androidx.annotation.RequiresApi
import com.sy.syauth.android.bg.resurrectIfDead
import java.util.UUID
import java.util.concurrent.Executor
import java.util.concurrent.atomic.AtomicReference

/** Logcat tag for the CDM-driven companion scanner. */
internal const val CDM_PAIR_SCANNER_LOG_TAG: String = "syauth.pair.cdm"

/** Reason surfaced when the host platform predates the CDM Executor overload. */
internal const val CDM_API_TOO_OLD_REASON: String =
    "CompanionDeviceManager.associate(Executor) requires API 33+"

/** Reason surfaced when the platform service lookup returns null. */
internal const val CDM_SERVICE_MISSING_REASON: String =
    "CompanionDeviceManager system service unavailable"

/** Reason surfaced when the user dismisses the OS picker. */
internal const val CDM_PICKER_CANCELLED_REASON: String =
    "user cancelled the companion-device picker"

/** Reason surfaced when the picker returns without a BluetoothDevice extra. */
internal const val CDM_PICKER_NO_DEVICE_REASON: String =
    "companion-device picker returned no BluetoothDevice"

/**
 * Production [PairCompanionScanner] wrapping
 * `CompanionDeviceManager.associate(AssociationRequest, Executor, Callback)`.
 *
 * Construction takes an [ActivityResultLauncher] keyed on
 * [IntentSenderRequest] — register one at the Activity's
 * `onCreate(...)` and pass it here. The launcher's callback must
 * forward into [onPickerResult] so this scanner can resolve the
 * [pendingOnPicked] / [pendingOnFailed] continuations.
 */
@RequiresApi(Build.VERSION_CODES.O)
public class AndroidCdmPairCompanionScanner(
    private val activity: Activity,
    private val launcher: ActivityResultLauncher<IntentSenderRequest>,
    private val executor: Executor,
) : PairCompanionScanner {

    /** Stashed callback that fires on a successful CDM pick. */
    private val pendingOnPicked: AtomicReference<
        ((deviceAddress: String, deviceName: String?) -> Unit)?,
    > = AtomicReference(null)

    /** Stashed callback that fires on any failure path. */
    private val pendingOnFailed: AtomicReference<((String) -> Unit)?> =
        AtomicReference(null)

    override fun associate(
        serviceUuids: List<UUID>,
        onPicked: (deviceAddress: String, deviceName: String?) -> Unit,
        onFailed: (reason: String) -> Unit,
    ) {
        pendingOnPicked.set(onPicked)
        pendingOnFailed.set(onFailed)
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
            failPending(CDM_API_TOO_OLD_REASON)
            return
        }
        val manager = activity.getSystemService(CompanionDeviceManager::class.java)
            ?: run {
                failPending(CDM_SERVICE_MISSING_REASON)
                return
            }
        val request = buildAssociationRequest(serviceUuids)
        try {
            manager.associate(request, executor, cdmCallback())
        } catch (e: SecurityException) {
            failPending(e.message ?: e::class.java.simpleName)
        }
    }

    private fun buildAssociationRequest(serviceUuids: List<UUID>): AssociationRequest {
        val builder = AssociationRequest.Builder().setSingleDevice(false)
        for (uuid in serviceUuids) {
            val scan = ScanFilter.Builder()
                .setServiceUuid(ParcelUuid(uuid))
                .build()
            val filter = BluetoothLeDeviceFilter.Builder()
                .setScanFilter(scan)
                .build()
            builder.addDeviceFilter(filter)
        }
        return builder.build()
    }

    private fun cdmCallback(): CompanionDeviceManager.Callback =
        object : CompanionDeviceManager.Callback() {
            override fun onAssociationPending(intentSender: IntentSender) {
                launchPicker(intentSender)
            }

            @Deprecated("Legacy pre-API-33 callback; framework deprecated in API 34.")
            override fun onDeviceFound(intentSender: IntentSender) {
                // Legacy callback path (pre-API-33). Same handling:
                // launch the IntentSender; the user pick lands on the
                // launcher's callback which then resolves
                // [pendingOnPicked].
                launchPicker(intentSender)
            }

            override fun onAssociationCreated(associationInfo: AssociationInfo) {
                // Reached on the modern API path after the user
                // accepted the picker. The BluetoothDevice is
                // delivered via the launcher's Intent extras; this
                // callback's role is informational on API 33+. We
                // log, start the proximity observer the unlock path
                // needs, and let the launcher path resolve the picked
                // peer.
                Log.i(
                    CDM_PAIR_SCANNER_LOG_TAG,
                    "CDM association created id=${associationInfo.id}",
                )
                startObservingDevicePresence(associationInfo.id)
            }

            override fun onFailure(error: CharSequence?) {
                failPending(error?.toString() ?: CDM_PICKER_CANCELLED_REASON)
            }
        }

    private fun launchPicker(intentSender: IntentSender) {
        val request = IntentSenderRequest.Builder(intentSender).build()
        runCatching { launcher.launch(request) }.onFailure { t ->
            failPending(t.message ?: t::class.java.simpleName)
        }
    }

    /**
     * Start the OS-managed proximity observer for [associationId].
     * Required for `SyauthCompanionService` to be bound when the
     * desktop's rotating unlock UUID appears in BLE range — without
     * this call, the CDM association's `mNotifyOnDeviceNearby` stays
     * `false` and the OS never binds the service. The grant is silent
     * (no runtime dialog), gated only by the manifest's
     * `REQUEST_OBSERVE_COMPANION_DEVICE_PRESENCE` permission.
     *
     * Idempotent: calling on an already-observed association is a
     * no-op per the framework contract. Pre-API-31 hosts skip the
     * call (the symbol was added in API 31 / Android 12).
     *
     * Reflection: the int-overload `startObservingDevicePresence(int)`
     * is `@SystemApi` on `compileSdk = 34` even though the runtime on
     * API 33+ accepts it. We invoke it via reflection so we keep
     * `compileSdk = 34` while still using the modern association-id
     * shape on Android 13/14/15. The deprecated MAC-string overload
     * (`startObservingDevicePresence(String)`) is the documented
     * fallback when the int overload throws `NoSuchMethodException`.
     */
    public fun startObservingDevicePresence(associationId: Int) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) {
            Log.i(
                CDM_PAIR_SCANNER_LOG_TAG,
                "skip startObservingDevicePresence: api=${Build.VERSION.SDK_INT} < ${Build.VERSION_CODES.S}",
            )
            return
        }
        val manager = activity.getSystemService(CompanionDeviceManager::class.java)
        if (manager == null) {
            Log.w(CDM_PAIR_SCANNER_LOG_TAG, "startObservingDevicePresence: $CDM_SERVICE_MISSING_REASON")
            return
        }
        // CDM only fires `onDeviceAppeared` on a state transition
        // (offline → online). If the device was already present
        // before observation started, the binding sits in
        // "already-appeared" state and the service's
        // `onDeviceAppeared` never fires. Force the transition by
        // stopping observation first, then starting it again — the
        // OS treats the restart as a fresh observation and re-emits
        // the appear event when the device is next seen.
        runCatching {
            val stopInt = manager.javaClass.getMethod("stopObservingDevicePresence", Int::class.javaPrimitiveType)
            stopInt.invoke(manager, associationId)
        }.onSuccess {
            Log.i(CDM_PAIR_SCANNER_LOG_TAG, "pre-cycle: stopped observing id=$associationId (int)")
        }.onFailure {
            // Fallback: try the MAC overload before starting.
            val mac = associationMacAddress(manager, associationId)
            if (mac != null) {
                @Suppress("DEPRECATION")
                runCatching { manager.stopObservingDevicePresence(mac) }.onSuccess {
                    Log.i(CDM_PAIR_SCANNER_LOG_TAG, "pre-cycle: stopped observing id=$associationId (mac=$mac)")
                }
            }
        }
        // Try the int-overload first via reflection. On API 33+ this
        // is the production path; on older runtimes the method is
        // missing and the catch falls back to the MAC-string overload.
        val viaInt = runCatching {
            val method = manager.javaClass.getMethod("startObservingDevicePresence", Int::class.javaPrimitiveType)
            method.invoke(manager, associationId)
        }
        if (viaInt.isSuccess) {
            Log.i(CDM_PAIR_SCANNER_LOG_TAG, "started observing device presence (int) id=$associationId")
            return
        }
        // Fallback: deprecated MAC-string overload (API 31+).
        val mac = associationMacAddress(manager, associationId)
        if (mac == null) {
            Log.w(
                CDM_PAIR_SCANNER_LOG_TAG,
                "startObservingDevicePresence id=$associationId: no mac (int overload failure: ${viaInt.exceptionOrNull()?.message})",
            )
            return
        }
        @Suppress("DEPRECATION")
        runCatching { manager.startObservingDevicePresence(mac) }
            .onSuccess {
                Log.i(CDM_PAIR_SCANNER_LOG_TAG, "started observing device presence (mac) id=$associationId addr=$mac")
            }
            .onFailure { t ->
                Log.w(CDM_PAIR_SCANNER_LOG_TAG, "startObservingDevicePresence id=$associationId addr=$mac failed", t)
            }
    }

    /**
     * Resolve the MAC string for a CDM association id, or null if no
     * matching association exists. Used by the MAC-string fallback in
     * [startObservingDevicePresence].
     */
    @RequiresApi(Build.VERSION_CODES.S)
    private fun associationMacAddress(manager: CompanionDeviceManager, associationId: Int): String? =
        manager.myAssociations
            .firstOrNull { it.id == associationId }
            ?.deviceMacAddress
            ?.toString()
            ?.uppercase()

    /**
     * S-012 belt-and-suspenders resurrection hook. SPEC §3 scope item
     * #19 keeps `CompanionDeviceManager.startObservingDevicePresence`
     * alive as a proximity signal for the foreground service's
     * watchdog. When the OS observes the bonded desktop in BLE range
     * the caller should invoke this method; if
     * `SyauthCompanionService.isRunning` reports `false` and a bond
     * exists on disk, the shared [resurrectIfDead] helper issues
     * `startForegroundService` so the next challenge lands on a live
     * service. Idempotent on a live service and on an unpaired
     * device.
     */
    public fun onProximityObservedForBondedPeer(context: Context) {
        Log.i(CDM_PAIR_SCANNER_LOG_TAG, "proximity observed; checking service liveness")
        resurrectIfDead(context)
    }

    /**
     * Resolve the pending [pendingOnPicked] / [pendingOnFailed] with the
     * result of the OS picker. The Activity calls this from the
     * launcher's callback. Returns true if a pending continuation was
     * present (so the Activity can log a stray-callback diagnostic
     * otherwise).
     */
    public fun onPickerResult(resultCode: Int, data: Intent?): Boolean {
        val onPicked = pendingOnPicked.getAndSet(null) ?: return false
        val onFailed = pendingOnFailed.getAndSet(null)
        if (resultCode != Activity.RESULT_OK || data == null) {
            (onFailed ?: { _ -> })(CDM_PICKER_CANCELLED_REASON)
            return true
        }
        val picked = extractPickedPeer(data)
        if (picked == null) {
            (onFailed ?: { _ -> })(CDM_PICKER_NO_DEVICE_REASON)
            return true
        }
        onPicked(picked.address, picked.name)
        return true
    }

    private data class PickedPeer(val address: String, val name: String?)

    /**
     * Resolve the picked peer from the launcher's result Intent. On
     * Samsung One UI / Android 15, CDM's `setResultAndFinish` returns
     * an `AssociationInfo` via [CompanionDeviceManager.EXTRA_ASSOCIATION]
     * rather than a `BluetoothDevice` via the legacy
     * [CompanionDeviceManager.EXTRA_DEVICE] key — the latter returns
     * null on this codepath. We try both extras (legacy first for
     * other OEMs), and fall back to looking up the device by MAC
     * through [android.bluetooth.BluetoothAdapter] when only the
     * association is present.
     */
    @Suppress("DEPRECATION")
    private fun extractPickedPeer(data: Intent): PickedPeer? {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            val legacyDevice = data.getParcelableExtra(
                CompanionDeviceManager.EXTRA_DEVICE,
                BluetoothDevice::class.java,
            )
            if (legacyDevice != null) {
                return PickedPeer(legacyDevice.address, runCatching { legacyDevice.name }.getOrNull())
            }
            val association = data.getParcelableExtra(
                CompanionDeviceManager.EXTRA_ASSOCIATION,
                AssociationInfo::class.java,
            )
            if (association != null) {
                // MacAddress.toString() is lowercase per its contract;
                // BluetoothAdapter.getRemoteDevice requires uppercase
                // hex per its docs (and throws IllegalArgumentException
                // otherwise). Uppercase before handing the MAC back.
                val mac = association.deviceMacAddress?.toString()?.uppercase()
                val display = association.displayName?.toString()
                if (mac != null) {
                    return PickedPeer(mac, display)
                }
            }
            return null
        }
        val legacyDevice: BluetoothDevice? = data.getParcelableExtra(CompanionDeviceManager.EXTRA_DEVICE)
        if (legacyDevice != null) {
            return PickedPeer(legacyDevice.address, runCatching { legacyDevice.name }.getOrNull())
        }
        return null
    }

    private fun failPending(reason: String) {
        pendingOnPicked.set(null)
        val onFailed = pendingOnFailed.getAndSet(null) ?: return
        onFailed(reason)
    }
}

/**
 * Direct executor that runs every task inline on the caller's thread.
 * The CDM callbacks are short and only stash references; running
 * them on the calling thread keeps the lifecycle simple and avoids
 * spinning up an executor pool for one callback per pair attempt.
 */
internal class InlineExecutor : Executor {
    override fun execute(command: Runnable) {
        command.run()
    }
}

