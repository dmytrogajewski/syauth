// Roadmap item S-016 — production BluetoothBondRemover using reflection.
//
// `android.bluetooth.BluetoothDevice#removeBond(): Boolean` has been
// present in AOSP since API 1 but NEVER exposed in the public SDK. Per
// Android's own docs, the removal of a bond is "internal" — the user is
// supposed to remove pairings from Settings > Bluetooth. We need
// programmatic removal for the Failed-path cleanup in S-016 (DoD #3),
// so reflection is the only option.
//
// Tested against:
//   - Android 14 / API 34 (compileSdk in syauth-android/app/build.gradle.kts)
//   - Android 12 / API 31 (minSdk = 26, with `BLUETOOTH_CONNECT` runtime grant)
//
// Tracking removal: there is no Google-issue-tracker ticket asking for a
// public `removeBond()` as of 2026-05-15. If Android 15 / API 35 promotes
// the method to `@SystemApi` or `@SuppressLint("MissingPermission")`-
// guarded public API, this file becomes a one-line direct call and the
// reflection vanishes. The journey doc tracks the rationale.
//
// Defensive coding choices:
//   - Wrap the reflective call in `runCatching { ... }.getOrDefault(false)`
//     so an `@hide` enforcement at runtime (Android's grey-list / black-list
//     mechanism) returns `false` instead of crashing the screen.
//   - Resolve the peer's `BluetoothDevice` via the system adapter — we
//     receive a string peer-id from the ViewModel (the same id the
//     scan emitted), assumed to be a BT MAC. A peer-id that doesn't
//     match a bonded device returns `false`.
package com.sy.syauth.android.pair.impl

import android.annotation.SuppressLint
import android.bluetooth.BluetoothAdapter
import com.sy.syauth.android.pair.api.BluetoothBondRemover

/**
 * Production [BluetoothBondRemover] using reflection on
 * `BluetoothDevice.removeBond()`.
 *
 * The reflection is unavoidable: Android does not ship a public
 * `removeBond()` in the SDK. The journey doc
 * (`specs/journeys/JOURNEY-S-016-android-pairing.md`) tracks the
 * SDK levels we tested against and the swap path when a public API
 * eventually lands.
 *
 * @param adapter the system [BluetoothAdapter]; injected so a future
 *   test or alternative adapter source can be wired without touching this
 *   class. Production wiring resolves
 *   `BluetoothAdapter.getDefaultAdapter()` once in MainActivity.
 */
class ReflectionBondRemover(
    private val adapter: BluetoothAdapter?,
) : BluetoothBondRemover {

    @SuppressLint("MissingPermission")
    override fun remove(peerId: String): Boolean {
        val a = adapter ?: return false
        val device = runCatching { a.getRemoteDevice(peerId) }.getOrNull() ?: return false
        return runCatching {
            val method = device.javaClass.getMethod(REMOVE_BOND_METHOD_NAME)
            method.invoke(device) as Boolean
        }.getOrDefault(false)
    }

    private companion object {
        /**
         * The hidden API method name. Verbatim per AOSP
         * `frameworks/base/core/java/android/bluetooth/BluetoothDevice.java`.
         */
        const val REMOVE_BOND_METHOD_NAME: String = "removeBond"
    }
}
