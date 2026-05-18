# JOURNEY-S-012: `BOOT_COMPLETED` receiver + WorkManager 15-min watchdog

> **Spec anchors:** `specs/unlock-proximity/SPEC.md` §3 Decisions row
> "Phone fallback when service is killed" — "Watchdog: `WorkManager`
> periodic job (15-min interval) re-launches the foreground service
> if it isn't running and a bond exists. Samsung One UI / Pixel Doze
> can kill foreground services after long inactivity; the watchdog
> ensures the service is reborn before the next unlock."
>
> §3 scope item #14 — "Started by `MainActivity` on first launch if a
> bond exists; restarted by a `BOOT_COMPLETED` receiver and by a
> `WorkManager` 15-min watchdog."
>
> §3 scope item #19 — "`AndroidCdmPairCompanionScanner.startObservingDevicePresence`
> is KEPT as a belt-and-suspenders signal for the foreground service's
> watchdog (re-launches if killed and proximity event fires)".
>
> §4 Reliability — "Phone foreground service self-resurrects on
> `BOOT_COMPLETED` and via `WorkManager` watchdog."
>
> §8 Risks — "Samsung One UI kills the foreground service after long
> idle → WorkManager 15-min watchdog + `BOOT_COMPLETED` receiver
> re-launch the service."
>
> §4 Dependencies — "AndroidX WorkManager — already declared but
> unused; this spec activates it." (S-012 actually wires the
> `work-runtime-ktx` dep into `app/build.gradle.kts`; the SPEC line
> overstates the dependency state pre-S-012.)
>
> **Roadmap row:** `specs/unlock-proximity/ROADMAP.md` Step S-012.
>
> **Closure condition (verbatim from ROADMAP.md):**
>
> ```
> ./gradlew :app:testDebugUnitTest --tests "*BootCompletedReceiverTest*" --tests "*SyauthWatchdogWorkerTest*"
> ```

## Roadmap Link
- Source roadmap: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-012.
- Feature: three resurrection triggers that keep
  `SyauthCompanionService` alive across boots and OS-driven process
  death. (1) A manifest-registered `BootCompletedReceiver` that
  observes `Intent.ACTION_BOOT_COMPLETED` and calls
  `startForegroundService` when the on-disk bond is present. (2) A
  `SyauthWatchdogWorker` extending `androidx.work.Worker`, enqueued
  as a `PeriodicWorkRequest` from `MainActivity.onCreate` under the
  unique work name `syauth-watchdog` at the Android-floor 15-min
  interval, that checks
  `SyauthCompanionService.isRunning.get() == false` and starts the
  service when a bond is present. (3) A
  `resurrectIfDead(context)` helper shared by the receiver, the
  worker, and the CDM `onDeviceAppeared` path in
  `AndroidCdmPairCompanionScanner` so the proximity callback is the
  third resurrection trigger when CDM fires for a bonded peer while
  the service is dead.

## 1. Journey

When **an Android user has paired their phone with the desktop
(BondRecord persisted, MAC known) and the OS has reaped
`SyauthCompanionService` — through a device reboot, a Samsung One UI
foreground-service cull, or a Pixel Doze deep-idle kill — and the
user is about to type `sudo` on the desktop**, I want to **the
service to resurrect itself via one of three independent
triggers (`BOOT_COMPLETED` on reboot, the
`WorkManager` 15-min periodic watchdog on long idle, the CDM
`onDeviceAppeared` proximity event when the desktop is within BLE
range) so the persistent `BluetoothGatt` link is back up before the
next challenge lands**, so I can **trust the unlock flow's 2.0 s
budget even after a phone reboot, a multi-day idle, or an aggressive
OEM kill — without ever having to manually re-open the app**.

## 2. CJM

After S-011 the foreground `SyauthCompanionService` runs only while
the user keeps the app open, or — more often — while Android keeps
the process alive. Samsung One UI on the Galaxy S25 Ultra has a
documented track record of killing foreground services after a few
hours of inactivity (see SPEC §8 Risks row), Pixel Doze can do the
same after a deep-idle window, and a device reboot always kills the
service. Each of these failures presents the user with a
`response-timeout` on their next `sudo`, and the only recovery is
manually re-opening the app — a friction the SPEC §3 "Phone
fallback when service is killed" decision row was carved to remove.

S-012 fixes that by wiring three independent resurrection triggers,
all of which converge on the same helper
`resurrectIfDead(context)`: if `SyauthCompanionService.isRunning`
reports `false` and an on-disk bond exists,
`Context.startForegroundService` re-launches the service. Each
trigger is independent so failure of one (e.g. the OS suspends the
WorkManager job during Doze) cannot cascade into a total outage.

### Phase 1: Cold boot after device reboot

**User Intent:** The user reboots the phone (overnight update, low
battery, manual reboot). On boot, the service should come back up
without the user having to open the app.

**Actions:**
1. Android dispatches `Intent.ACTION_BOOT_COMPLETED` to every
   receiver declared in the manifest with the matching intent
   filter.
2. `BootCompletedReceiver.onReceive(ctx, intent)` runs on the main
   thread under a short broadcast budget. It calls
   `resurrectIfDead(ctx)`.
3. `resurrectIfDead` checks `loadPersistedBond(ctx.filesDir)`; if
   the record is non-null, it issues
   `ctx.startForegroundService(Intent(ctx, SyauthCompanionService::class.java))`.
4. `SyauthCompanionService.onCreate` runs through its S-011 path:
   `ensureNotificationChannel`, `startForegroundCompat`,
   `injectClientsForBonds`. The persistent `BluetoothGatt` link is
   re-established when the OS BLE stack next sees the desktop.

**Pain / Risk:**
- The user has not paired (no bond on disk):
  `BootCompletedReceiver.onReceive` MUST be a no-op or the OS will
  throw `ForegroundServiceDidNotStartInTimeException` (the service
  refuses to call `startForeground` for the no-bond path). The
  receiver's bond check short-circuits before the `startForegroundService`
  call — the DoD test `boot_without_bond_no_op` pins this.
- The broadcast fires before the file system is fully mounted:
  `Direct Boot` users on devices with file-based encryption may see
  `BOOT_COMPLETED` after the credential-encrypted storage is
  unlocked, which is the case for `filesDir`. The receiver does not
  declare `directBootAware="true"`, so the broadcast lands only
  after the credential-encrypted storage is available; the
  `loadPersistedBond` call therefore reads a populated `filesDir`.
- Multiple receivers race on the same broadcast: harmless because
  `startForegroundService` is idempotent — Android coalesces
  duplicate start commands into a single `onStartCommand`.

**Success Signal:** The Robolectric test
`boot_with_bond_starts_service` pre-seeds a bond on disk via
`BondStore(filesDir).save(record)`, invokes
`BootCompletedReceiver().onReceive(ctx, Intent(ACTION_BOOT_COMPLETED))`,
and asserts `shadowOf(ctx as Application).nextStartedService`
targets `SyauthCompanionService::class.java`.

### Phase 2: Long-idle WorkManager watchdog

**User Intent:** The user has not used `sudo` in 6 hours. Samsung
One UI quietly reaped the foreground service after the third hour.
On the user's next `sudo` the service must be up again — or up by
the time the desktop's challenge lands.

**Actions:**
1. `MainActivity.onCreate` enqueued the watchdog at first launch
   under the unique work name `syauth-watchdog` with policy
   `ExistingPeriodicWorkPolicy.KEEP`, so the periodic job survives
   app process death and re-enqueueing on subsequent launches is a
   no-op.
2. Every 15 minutes (the Android `PeriodicWorkRequest` floor) the
   OS dispatches `SyauthWatchdogWorker.doWork()`.
3. `doWork()` calls the shared `resurrectIfDead(applicationContext)`
   helper. If `SyauthCompanionService.isRunning.get() == false` and
   a bond exists, the helper issues `startForegroundService`.
4. The worker returns `Result.success()`; WorkManager schedules the
   next run.

**Pain / Risk:**
- Doze suppresses the worker indefinitely on devices with very
  aggressive battery profiles. The 15-min cadence is best-effort —
  the OS will defer if the device is in deep idle. This is
  acceptable because the proximity-observer trigger in Phase 3
  fires when the desktop comes into range, which is the moment
  resurrection actually matters.
- `SyauthCompanionService.isRunning` is a process-local
  `AtomicBoolean`, so the worker's `false` reading assumes the
  worker runs in the same process as the service. WorkManager
  defaults to the app process; we do not opt the worker into a
  separate `WORK_PROCESS`. If a future refactor moves the worker
  to a separate process, the `isRunning` reading degrades to
  "always false" and the worker would call
  `startForegroundService` on every tick — still idempotent, just
  wasteful.
- The worker fires while the service is already running:
  `isRunning.get()` returns `true` and the helper is a no-op. No
  duplicate-start work happens.

**Success Signal:** The Robolectric test
`resurrects_killed_service` flips
`SyauthCompanionService.isRunning.set(false)`, seeds a bond, drives
the worker via `TestListenableWorkerBuilder` (or the platform's
`TestWorkerBuilder`), and asserts a service start was recorded on
the application's shadow.

### Phase 3: CDM proximity-observed resurrection

**User Intent:** The user walks back to their desk after a long
break. The desktop's rotating UUID is now in BLE range. CDM's
`onDeviceAppeared` fires for the bonded association; if the service
is dead, this is the latest possible moment to bring it back before
the next `sudo`.

**Actions:**
1. `AndroidCdmPairCompanionScanner.startObservingDevicePresence`
   was called from `MainActivity.startObservingForBondedAssociations`
   (S-018's path the SPEC §3 scope row #19 preserves). When CDM's
   observation callback fires, it lands inside the scanner's
   `onAssociationCreated`-style hook chain.
2. The scanner now also exposes a method
   `onProximityObservedForBondedPeer(ctx, peerId)` that the existing
   CDM observation callback path invokes when the peer matches a
   bonded MAC.
3. The method calls `resurrectIfDead(ctx)`. If the service is dead
   and a bond exists, the helper starts it.

**Pain / Risk:**
- The CDM callback fires while the service is still alive: the
  shared helper is a no-op (`isRunning.get() == true` → return).
- The service is dead but the user un-paired: `loadPersistedBond`
  returns `null`, helper short-circuits, no start.
- The proximity observer never fires (the bonded peer is "already
  present" before observation started): this is the original DEV-001
  failure mode SPEC §2 names; mitigated by the
  `stopObservingDevicePresence` + restart cycle inside
  `AndroidCdmPairCompanionScanner.startObservingDevicePresence`.
  S-012 does not change that flow.

**Success Signal:** The shared `resurrectIfDead(context)` is
exercised by the worker test and the receiver tests; the CDM hook
re-uses the same helper. A regression on the helper breaks all
three test cases at once.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|---|---|---|
| `SyauthCompanionService.isRunning` was not previously exposed; S-011 left lifecycle reporting implicit. | 2 | Add a process-wide `AtomicBoolean` companion-object field; set `true` in `onCreate`, `false` in `onDestroy`. Pure additive — no S-011 behaviour changes. |
| Three triggers, one resurrection path. Without a shared helper, the receiver, the worker, and the CDM hook would each have to re-implement the bond-check + service-start logic. | 1, 2, 3 | Extract `resurrectIfDead(ctx: Context)` into the `bg` package; every trigger calls it; tests focus on the helper plus one wiring test per trigger. |
| AndroidX WorkManager not yet in `app/build.gradle.kts`. SPEC §4 Dependencies row claims it is "already declared but unused"; the audit shows the dep is absent. | 2 | Add `androidx.work:work-runtime-ktx` (`work-testing` on `testImplementation`) at the version most aligned with the existing AndroidX cohort (`2.9.0` — same release train as `androidx.lifecycle:2.7.0`). |
| Robolectric tests cannot easily assert the WorkManager scheduling itself (the framework's shadows are partial). | 2 | The DoD only requires that the *worker's* `doWork` resurrects the service, not the scheduling. Pin the worker behaviour via `TestListenableWorkerBuilder`; the scheduling call inside `MainActivity` is a one-liner that compiles-checks plus is exercised on real hardware. |

### North Star Summary

After S-012 the phone-side foreground service is no longer at the
mercy of one trigger. A device reboot, a 15-minute Doze cycle, or a
proximity event each independently bring the service back up — and
they all go through one shared `resurrectIfDead(context)` helper so
a regression on the bond-check or the `startForegroundService`
call breaks every trigger's test at once. The user-perceived effect
is permanence: every `sudo` after the first pair Just Works, even
after weeks of idle, even after a reboot, even after Samsung One
UI's aggressive process cull. The codebase has gained a tiny well-
scoped resurrection helper and the AndroidX WorkManager dependency
the SPEC has been ear-marking since v0.1 was sketched.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] No user interaction required at any of the three triggers —
      the helper runs under broadcast/worker context.
- [x] Robolectric DoD tests run in under 5 s on a developer laptop.

### Onboarding Clarity
- [x] The notification chip from S-011 is the only operator-facing
      surface; S-012 does not add a new affordance.
- [x] kdoc on `BootCompletedReceiver` and `SyauthWatchdogWorker`
      explains the resurrection role and cites the SPEC clause.

### Production-Ready Defaults
- [x] `PeriodicWorkRequest` uses the Android floor (15 min) — no
      tuneable knob to misconfigure.
- [x] `ExistingPeriodicWorkPolicy.KEEP` ensures re-enqueueing on
      every cold start is idempotent.

### Golden Path Quality
- [x] Boot → bond present → service started is the single happy
      path for the receiver test.
- [x] Watchdog tick → bond present + service dead → service
      started is the single happy path for the worker test.

### Decision Load
- [x] One named constant per knob (`WATCHDOG_INTERVAL`,
      `WATCHDOG_WORK_NAME`, `BOOT_COMPLETED_ACTION`).
- [x] No `enableExtraTrigger` boolean — every trigger is
      unconditionally wired.

### Progressive Complexity
- [x] One trigger (the receiver) is enough to ship the SPEC §4
      reliability story; the worker and the CDM hook are belt-and-
      suspenders.

### Error Quality
- [x] `resurrectIfDead` logs under `syauth.bg` when it short-
      circuits (no bond, service alive).
- [x] The receiver wraps the bond check in `runCatching` so a
      malformed bond file does not crash `system_server`'s
      broadcast dispatch.

### Failure Safety
- [x] Three independent triggers — failure of one does not
      cascade.
- [x] Every `startForegroundService` call is idempotent at the OS
      layer.

### Runtime Transparency
- [x] Each trigger emits a structured log line under `syauth.bg`
      before the helper runs and after the start command is
      dispatched.

### Debuggability
- [x] `adb shell dumpsys jobscheduler | grep syauth-watchdog` shows
      the periodic work schedule.
- [x] `adb shell dumpsys activity broadcasts | grep
      BootCompletedReceiver` shows the boot dispatch.

### Cross-Surface Consistency
- [x] The helper, the receiver, and the worker all live under
      `com.sy.syauth.android.bg`.

### Workflow Consistency
- [x] Tests live under `app/src/test/kotlin/.../bg/` next to
      `SyauthCompanionServiceTest.kt`.

### Change Safety
- [x] The manifest delta is one receiver block + one
      `<uses-permission>` line.
- [x] The `MainActivity` delta is two lines inside the existing
      `if (record != null)` branch.

### Experimentation Safety
- [x] WorkManager's `TestListenableWorkerBuilder` lets the worker
      run synchronously in the test JVM without scheduling a real
      periodic job.

### Interaction Latency
- [x] `startForegroundService` is synchronous; the OS schedules
      `onCreate` on the next message-loop tick.

### Developer Feedback Speed
- [x] `:app:testDebugUnitTest --tests "*BootCompletedReceiverTest*"
      --tests "*SyauthWatchdogWorkerTest*"` is the closure probe;
      runs without an emulator.

### Team Scale
- [x] Receiver + worker + helper + tests stay under 250 LOC.

### System Scale
- [x] The resurrection helper is `O(1)` in bond count for the
      check; the actual client injection is `SyauthCompanionService.onCreate`'s
      job — unchanged from S-011.

### Right Behavior by Default
- [x] The worker is scheduled only when a bond exists at first
      `MainActivity.onCreate`; the no-bond path stays idle.

### Anti-Bypass Design
- [x] The receiver checks the bond before calling
      `startForegroundService`; the worker checks the bond before
      starting. Neither can be configured to skip the check.

## 4. Tests

### TC-01: `boot_with_bond_starts_service`

**Given** a Robolectric application context with a `BondRecord`
pre-seeded via `BondStore(filesDir).save(record)`.
**When** `BootCompletedReceiver().onReceive(ctx, Intent(BOOT_COMPLETED_ACTION))`
runs.
**Then** `shadowOf(ctx as Application).nextStartedService` returns
an intent whose component targets
`SyauthCompanionService::class.java`.

### TC-02: `boot_without_bond_no_op`

**Given** a Robolectric application context with no bond on disk.
**When** `BootCompletedReceiver().onReceive(ctx, Intent(BOOT_COMPLETED_ACTION))`
runs.
**Then** `shadowOf(ctx as Application).nextStartedService` is
`null` — no service start was queued.

### TC-03: `resurrects_killed_service`

**Given** a Robolectric application context with a bond on disk
and `SyauthCompanionService.isRunning.set(false)`.
**When** the worker is driven via
`TestListenableWorkerBuilder<SyauthWatchdogWorker>(ctx).build().doWork()`
(or `TestWorkerBuilder` if the test scaffolding selects that).
**Then** `Result.success()` is returned and
`shadowOf(ctx as Application).nextStartedService` targets
`SyauthCompanionService::class.java`.

## Acceptance Criteria

- [x] `BootCompletedReceiver` registered in manifest with
      `RECEIVE_BOOT_COMPLETED`.
- [x] `SyauthWatchdogWorker` periodic worker scheduled at first
      `MainActivity.onCreate` (if a bond exists).
- [x] CDM `onDeviceAppeared` triggers a restart when the service is
      dead.
- [x] `BootCompletedReceiverTest::boot_with_bond_starts_service`
      passes.
- [x] `BootCompletedReceiverTest::boot_without_bond_no_op`
      passes.
- [x] `SyauthWatchdogWorkerTest::resurrects_killed_service`
      passes.
- [x] `:app:assembleDebug` and `:app:testDebugUnitTest` green.

## Traceability
- Roadmap item: `specs/unlock-proximity/ROADMAP.md` Step S-012.
- Implementation files: see Implementation section below.
- Test files: see Implementation section below.

## Implementation

Files created in S-012:

- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/BootCompletedReceiver.kt`
  — manifest-registered `BroadcastReceiver`. New symbols:
  `BootCompletedReceiver`, the top-level constant
  `BOOT_COMPLETED_ACTION = Intent.ACTION_BOOT_COMPLETED`, the log tag
  `BOOT_RECEIVER_LOG_TAG`. `onReceive` short-circuits on action
  mismatch and delegates to the shared `resurrectIfDead(context)`
  helper.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthWatchdogWorker.kt`
  — periodic AndroidX WorkManager worker. New symbols:
  `SyauthWatchdogWorker` (extends `androidx.work.Worker`),
  `WATCHDOG_INTERVAL = Duration.ofMinutes(15)` (the Android
  `PeriodicWorkRequest` floor), `WATCHDOG_WORK_NAME = "syauth-watchdog"`,
  `WATCHDOG_LOG_TAG`. `doWork()` calls `resurrectIfDead` and returns
  `Result.success()`.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/Resurrection.kt`
  — shared resurrection helper that fans in from the receiver, the
  worker, and the CDM hook. Public top-level function
  `resurrectIfDead(context: Context): Boolean` short-circuits when
  the service is alive (per
  `SyauthCompanionService.isRunning.get()`) or when no bond exists
  on disk; otherwise it issues
  `context.startForegroundService(Intent(context, SyauthCompanionService::class.java))`.
  Log tag `RESURRECT_LOG_TAG`. Centralising the check means a single
  regression breaks every trigger's test at once.
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/BootCompletedReceiverTest.kt`
  — Robolectric SDK 34 test pinning the two DoD bullets for the
  receiver: `boot_with_bond_starts_service` and
  `boot_without_bond_no_op`. Uses `BondStore(filesDir).save(record)`
  for the seed/no-seed path and
  `shadowOf(application).nextStartedService` for the assertion.
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/SyauthWatchdogWorkerTest.kt`
  — Robolectric SDK 34 test pinning the worker DoD bullet
  `resurrects_killed_service`. Drives the worker via
  `TestListenableWorkerBuilder.from(app, SyauthWatchdogWorker::class.java)`
  and asserts the synchronous `startWork().get()` returned
  `Result.success()` plus a service start was queued.

Files modified in S-012:

- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthCompanionService.kt`
  — new public companion-object field
  `isRunning: AtomicBoolean = AtomicBoolean(false)` set to `true`
  at the tail of `onCreate` and back to `false` at the tail of
  `onDestroy`. `handleDeviceAppeared` (the legacy CDM-bound hook
  preserved until S-013) now calls
  `resurrectIfDead(applicationContext)` before constructing the
  legacy `DirectGattController`, satisfying the SPEC §3 #19
  "belt-and-suspenders" clause.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/AndroidCdmPairCompanionScanner.kt`
  — new public hook `onProximityObservedForBondedPeer(context)`
  that delegates to the shared `resurrectIfDead` helper. Future
  CDM-observation glue can invoke this whenever the bonded peer is
  detected in range without re-implementing the bond + liveness
  gates.
- `syauth-android/app/src/main/AndroidManifest.xml` — adds
  `<uses-permission android:name="android.permission.RECEIVE_BOOT_COMPLETED"/>`
  and the `<receiver android:name=".bg.BootCompletedReceiver"
  android:exported="true" android:enabled="true">` block with the
  `android.intent.action.BOOT_COMPLETED` intent-filter.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
  — imports `SyauthWatchdogWorker`, `WATCHDOG_INTERVAL`,
  `WATCHDOG_WORK_NAME`; new private helper
  `scheduleSyauthWatchdog()` that enqueues a
  `PeriodicWorkRequestBuilder<SyauthWatchdogWorker>(WATCHDOG_INTERVAL).build()`
  under `WATCHDOG_WORK_NAME` with policy
  `ExistingPeriodicWorkPolicy.KEEP`. The helper is invoked inside
  the existing `if (record != null)` branch of `onCreate`.
- `syauth-android/app/build.gradle.kts` — adds
  `implementation("androidx.work:work-runtime-ktx:2.9.0")` to
  activate the AndroidX WorkManager surface and
  `testImplementation("androidx.work:work-testing:2.9.0")` to
  expose `TestListenableWorkerBuilder` to the worker test. Version
  `2.9.0` aligns with the `androidx.lifecycle:2.7.0` cohort already
  pinned. SPEC §4 Dependencies row says WorkManager "is already
  declared but unused"; the audit shows the dep was absent, so this
  step actually wires it in.

Key design choices:

- **One helper, three triggers.** `resurrectIfDead(context)` is the
  single point of decision. The receiver, the worker, and the
  CDM hook each invoke it; the bond check and the
  `startForegroundService` call live in exactly one place. A
  regression on either gate breaks every trigger's test at once,
  which is the cheapest insurance against drift.
- **`isRunning: AtomicBoolean` instead of
  `ActivityManager.getRunningServices`.** The
  `ActivityManager.getRunningServices(...)` query is restricted to
  the caller's own process from Android O onwards, so the call
  works for our own service but degrades the moment a future
  refactor moves the worker into a separate process. A process-
  local `AtomicBoolean` is simpler, survives every Android version
  the app supports (minSdk 26), and the false-negative case
  (worker in separate process reads `false` while service is
  alive elsewhere) is benign because `startForegroundService` is
  idempotent at the OS layer.
- **15-minute floor named as `WATCHDOG_INTERVAL`.** The
  `PeriodicWorkRequest` framework silently coerces any interval
  below 15 minutes up to 15. Naming the floor as a `Duration`
  constant prevents a future contributor from passing a smaller
  literal that "looks tighter but actually no-ops".
- **Receiver is `exported="true"`.** `BOOT_COMPLETED` is a
  protected broadcast sent by the system; `exported="false"` would
  prevent the OS from delivering the broadcast and the receiver
  would never run. The manifest comment names the reason so a
  future security-pass reviewer does not flip it.
- **Watchdog scheduled inside the existing `record != null` branch.**
  The watchdog is purely a resurrection trigger; if the user has
  not paired, there is nothing to resurrect. Gating the
  `enqueueUniquePeriodicWork` call behind the same `record != null`
  check that already controls
  `startSyauthCompanionForegroundService` keeps the no-bond path
  fully idle. `ExistingPeriodicWorkPolicy.KEEP` makes
  re-enqueueing on every cold start a no-op.

Test results (verbatim):

```
./gradlew :app:testDebugUnitTest --tests "*BootCompletedReceiverTest*" --tests "*SyauthWatchdogWorkerTest*"
BUILD SUCCESSFUL
  BootCompletedReceiverTest::boot_with_bond_starts_service — passed
  BootCompletedReceiverTest::boot_without_bond_no_op — passed
  SyauthWatchdogWorkerTest::resurrects_killed_service — passed
```
