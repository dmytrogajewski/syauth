// Roadmap item S-012 — WorkManager 15-minute watchdog.
//
// One of the three independent triggers SPEC §3 Decisions row "Phone
// fallback when service is killed" relies on. The worker fires every
// 15 minutes (the Android `PeriodicWorkRequest` floor) and calls the
// shared [resurrectIfDead] helper. The helper short-circuits when the
// service is already running or when no bond exists; otherwise it
// issues `startForegroundService` against `SyauthCompanionService`.
//
// Scheduling lives in `MainActivity.onCreate` (under the unique work
// name [WATCHDOG_WORK_NAME] with policy `ExistingPeriodicWorkPolicy.KEEP`
// so re-enqueueing on every cold start is a no-op).
//
// Journey: specs/journeys/JOURNEY-S-012-boot-receiver-watchdog.md
package com.sy.syauth.android.bg

import android.content.Context
import android.util.Log
import androidx.work.Worker
import androidx.work.WorkerParameters
import java.time.Duration

/**
 * Periodic interval at which the watchdog runs. 15 minutes is the
 * documented Android floor for `PeriodicWorkRequest`; lower values
 * are silently coerced up by the framework, so we name the floor
 * explicitly.
 */
public val WATCHDOG_INTERVAL: Duration = Duration.ofMinutes(15)

/** Unique work name the periodic request is enqueued under. */
public const val WATCHDOG_WORK_NAME: String = "syauth-watchdog"

/** Logcat tag for the watchdog worker. */
internal const val WATCHDOG_LOG_TAG: String = "syauth.bg.watchdog"

/**
 * Periodic worker that resurrects [SyauthCompanionService] when the
 * OS reaps it. Idempotent: [resurrectIfDead] no-ops when the service
 * is already running or when no bond exists.
 */
public class SyauthWatchdogWorker(
    context: Context,
    params: WorkerParameters,
) : Worker(context, params) {
    override fun doWork(): Result {
        val started = resurrectIfDead(applicationContext)
        Log.i(WATCHDOG_LOG_TAG, "watchdog tick: started=$started")
        return Result.success()
    }
}
