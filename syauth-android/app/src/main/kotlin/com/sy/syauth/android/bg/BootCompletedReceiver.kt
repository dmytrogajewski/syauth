// Roadmap item S-012 — `BOOT_COMPLETED` resurrection trigger.
//
// One of the three independent triggers SPEC §3 Decisions row "Phone
// fallback when service is killed" relies on. On phone boot, the OS
// dispatches `Intent.ACTION_BOOT_COMPLETED` to every manifest-declared
// receiver with the matching intent filter; this receiver checks the
// on-disk bond and (if present) issues `startForegroundService` so the
// long-running foreground `SyauthCompanionService` is back up before
// the user's next `sudo`.
//
// Bond check + start dispatch are delegated to [resurrectIfDead] so the
// receiver, the [SyauthWatchdogWorker], and the CDM proximity hook all
// share one implementation — a regression on the helper breaks every
// resurrection test at once.
//
// Journey: specs/journeys/JOURNEY-S-012-boot-receiver-watchdog.md
package com.sy.syauth.android.bg

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log

/** Action this receiver matches. Constant so the manifest + tests cannot drift. */
public val BOOT_COMPLETED_ACTION: String = Intent.ACTION_BOOT_COMPLETED

/** Logcat tag for the boot-resurrection path. */
internal const val BOOT_RECEIVER_LOG_TAG: String = "syauth.bg.boot"

/**
 * Manifest-registered receiver. On `ACTION_BOOT_COMPLETED`, the
 * receiver calls [resurrectIfDead] which starts
 * `SyauthCompanionService` when a bond exists. The receiver does
 * nothing else — bond presence is the sole gate.
 */
public class BootCompletedReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != BOOT_COMPLETED_ACTION) {
            return
        }
        Log.i(BOOT_RECEIVER_LOG_TAG, "boot received; checking bond + service")
        resurrectIfDead(context)
    }
}
