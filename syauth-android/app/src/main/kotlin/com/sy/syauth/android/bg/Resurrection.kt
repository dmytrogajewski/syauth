// Roadmap item S-012 — shared service-resurrection helper.
//
// Three independent triggers fan in here:
//   1. `BootCompletedReceiver.onReceive`         — on phone boot.
//   2. `SyauthWatchdogWorker.doWork`             — every 15 minutes.
//   3. CDM `onProximityObservedForBondedPeer`    — when the bonded
//      desktop appears in BLE range.
//
// All three call [resurrectIfDead]; the helper short-circuits when
// either the on-disk bond is absent or the service is already running,
// otherwise it issues `Context.startForegroundService` against
// `SyauthCompanionService`. Keeping the logic in one place means a
// regression on the bond-check or the start-dispatch breaks every
// trigger's test simultaneously.
//
// Journey: specs/journeys/JOURNEY-S-012-boot-receiver-watchdog.md
package com.sy.syauth.android.bg

import android.content.Context
import android.content.Intent
import android.os.Build
import android.util.Log
import com.sy.syauth.android.bond.loadPersistedBond

/** Logcat tag for the resurrection helper. */
internal const val RESURRECT_LOG_TAG: String = "syauth.bg.resurrect"

/**
 * Resurrect [SyauthCompanionService] if it is not currently running
 * AND a bond exists on disk. The helper is idempotent: calling it on
 * a live service or on an unpaired device is a no-op.
 *
 * Returns `true` when a `startForegroundService` call was actually
 * dispatched; `false` otherwise. The return value is mostly useful
 * for tests — production callers (the receiver, the worker, the CDM
 * hook) ignore it.
 */
public fun resurrectIfDead(context: Context): Boolean {
    if (SyauthCompanionService.isRunning.get()) {
        Log.d(RESURRECT_LOG_TAG, "service already running; skip")
        return false
    }
    val bond = runCatching { loadPersistedBond(context.filesDir) }.getOrNull()
    if (bond == null) {
        Log.d(RESURRECT_LOG_TAG, "no bond on disk; skip")
        return false
    }
    val intent = Intent(context, SyauthCompanionService::class.java)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
        context.startForegroundService(intent)
    } else {
        context.startService(intent)
    }
    Log.i(RESURRECT_LOG_TAG, "resurrected service for peer=${bond.peerId}")
    return true
}
