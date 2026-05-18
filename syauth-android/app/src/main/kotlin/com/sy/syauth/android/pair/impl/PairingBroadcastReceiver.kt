// DEV-001 (re-march): Android pairing-variant gate.
//
// Android delivers OS-level pairing requests via the
// `BluetoothDevice.ACTION_PAIRING_REQUEST` broadcast. The
// `EXTRA_PAIRING_VARIANT` int names the variant; we accept ONLY
// `PAIRING_VARIANT_PASSKEY_CONFIRMATION` (value 2 per AOSP
// `BluetoothDevice.java`), per SPEC §3.2 D5. Every other variant
// (`PAIRING_VARIANT_CONSENT` = 3 / Just Works,
// `PAIRING_VARIANT_PIN` = 0 / legacy PIN,
// `PAIRING_VARIANT_PASSKEY` = 1 / passkey-entry,
// `PAIRING_VARIANT_OOB_CONSENT` = 6 / OOB) is rejected by calling
// `BluetoothDevice.setPairingConfirmation(false)` and aborting the
// broadcast.
//
// The receiver is registered programmatically by the production
// wiring in [com.sy.syauth.android.pair.impl.RealPairBackend] at
// construction time; it auto-unregisters when the backend's
// `cleanup()` method is called from the ViewModel's `onCleared()`.
package com.sy.syauth.android.pair.impl

import android.bluetooth.BluetoothDevice
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log

/**
 * Constant the receiver compares against `EXTRA_PAIRING_VARIANT`.
 * Numeric value `2` per AOSP `BluetoothDevice.java` (the public SDK
 * hides the symbol — we pin it explicitly here; the value is stable
 * across API levels per the AOSP reference). The DEV-001 re-march
 * fixed an earlier drift to value 4, which was actually
 * `PAIRING_VARIANT_DISPLAY_PASSKEY` and would have silently rejected
 * every LESC numeric-comparison broadcast. Tests pin the value so a
 * future SDK reshuffle is caught loudly.
 */
public const val PAIRING_VARIANT_PASSKEY_CONFIRMATION: Int = 2

/** Logcat tag the receiver uses for every span. */
public const val PAIRING_RECEIVER_LOG_TAG: String = "syauth.pair.receiver"

/** Extra-name string used by Android for the `EXTRA_PAIRING_VARIANT` int. */
public const val EXTRA_PAIRING_VARIANT_NAME: String = "android.bluetooth.device.extra.PAIRING_VARIANT"

/** Extra-name string used by Android for the `EXTRA_PAIRING_KEY` int (the 6-digit code). */
public const val EXTRA_PAIRING_KEY_NAME: String = "android.bluetooth.device.extra.PAIRING_KEY"

/** Decision the receiver derives from the broadcast. Exposed for unit tests. */
public sealed class PairingVariantDecision {
    /** Variant accepted — LESC numeric comparison. Carries the 6-digit code. */
    public data class AcceptPasskeyConfirmation(val passkey: Int) : PairingVariantDecision()
    /** Variant rejected — Just Works / legacy PIN / passkey-entry / OOB / unknown. */
    public data class Reject(val variant: Int) : PairingVariantDecision()
}

/**
 * Decide whether `variant` is acceptable for the syauth pair flow.
 * Mirrors the Rust-side `syauth_cli::pair::decide_pairing` decision
 * so a future change must flip both sides together.
 */
public fun decideAndroidPairingVariant(variant: Int, passkey: Int): PairingVariantDecision =
    if (variant == PAIRING_VARIANT_PASSKEY_CONFIRMATION) {
        PairingVariantDecision.AcceptPasskeyConfirmation(passkey = passkey)
    } else {
        PairingVariantDecision.Reject(variant = variant)
    }

/**
 * Broadcast receiver that gates `ACTION_PAIRING_REQUEST` on the
 * variant. Production registers an instance after
 * `BluetoothDevice.createBond()` and forwards
 * [PairingVariantDecision.AcceptPasskeyConfirmation] events to the
 * pairing UI via the provided callback. A
 * [PairingVariantDecision.Reject] outcome calls
 * `setPairingConfirmation(false)` and aborts the broadcast.
 */
public class PairingBroadcastReceiver(
    private val onAccept: (Int) -> Unit,
    private val onReject: (Int) -> Unit,
) : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != BluetoothDevice.ACTION_PAIRING_REQUEST) {
            return
        }
        val variant = intent.getIntExtra(EXTRA_PAIRING_VARIANT_NAME, INVALID_VARIANT)
        val passkey = intent.getIntExtra(EXTRA_PAIRING_KEY_NAME, INVALID_PASSKEY)
        val device: BluetoothDevice? = intent.getParcelableExtra(BluetoothDevice.EXTRA_DEVICE)
        when (val decision = decideAndroidPairingVariant(variant, passkey)) {
            is PairingVariantDecision.AcceptPasskeyConfirmation -> {
                // We do NOT call setPairingConfirmation(true) here:
                // that API requires `BLUETOOTH_PRIVILEGED` (signature
                // permission) and silently fails for third-party
                // apps. We also do NOT abort the broadcast — the
                // system's pairing dialog/notification needs to fire
                // so the user can compare the 6-digit code (SPEC
                // §3.2 D5) and tap "Pair" themselves. The OS then
                // confirms the LESC bond via its privileged code
                // path and BlueZ derives the LTK. Our `onAccept`
                // stash forwards the 6-digit code to the app UI so
                // both sides can display it for the comparison.
                Log.i(PAIRING_RECEIVER_LOG_TAG, "accepted LESC passkey-confirmation variant")
                onAccept(decision.passkey)
            }
            is PairingVariantDecision.Reject -> {
                Log.w(PAIRING_RECEIVER_LOG_TAG, "rejected pairing variant ${decision.variant}")
                runCatching { device?.setPairingConfirmation(false) }
                abortBroadcastIfPossible()
                onReject(decision.variant)
            }
        }
    }

    private fun abortBroadcastIfPossible() {
        runCatching { abortBroadcast() }
    }

    private companion object {
        const val INVALID_VARIANT: Int = -1
        const val INVALID_PASSKEY: Int = -1
    }
}
