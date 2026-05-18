# JOURNEY-S-011: `SyauthCompanionService` → long-running foreground `Service`

> **Spec anchors:** `specs/unlock-proximity/SPEC.md` §3 Approach
> ("**`SyauthCompanionService`** (Android) — becomes a long-running
> foreground service (`foregroundServiceType="connectedDevice"`,
> already in the manifest). Maintains a single `BluetoothGatt`
> client per bonded peer, opened with `autoConnect=true` and
> `TRANSPORT_LE`. Subscribes to the challenge characteristic via
> CCCD write on every fresh service discovery.").
>
> §3 Decisions row "Phone connection lifecycle" — "One persistent
> `BluetoothGatt` per bonded peer, opened with `autoConnect=true`
> and held by `SyauthCompanionService` as a long-running foreground
> service".
>
> §3 Decisions row "Phone fallback when service is killed" —
> "Watchdog: `WorkManager` periodic job (15-min interval) re-launches
> the foreground service if it isn't running and a bond exists". The
> watchdog and `BOOT_COMPLETED` receiver land in Step S-012; S-011
> only re-shapes the service into a plain `Service` with an
> explicit `startForegroundService` entry point from `MainActivity`.
>
> **Roadmap row:** `specs/unlock-proximity/ROADMAP.md` Step S-011.
>
> **Closure condition (verbatim from ROADMAP.md):**
>
> ```
> ./gradlew :app:testDebugUnitTest --tests "*SyauthCompanionServiceTest*"
> ```

## Roadmap Link
- Source roadmap: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-011.
- Feature: swap the parent class of
  `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthCompanionService.kt`
  from `CompanionDeviceService` to plain `android.app.Service`. Add
  `startForeground(NOTIFICATION_ID, notification)` with
  `foregroundServiceType="connectedDevice"`. Create a low-priority
  notification channel `"syauth phone-as-key active"` (channel id
  `syauth-presence`) that the operator can mute after first ack.
  Inject one `PersistentGattClient` per bonded peer at `onCreate`;
  tear them down in `onDestroy`. The manifest declares the service
  with `foregroundServiceType="connectedDevice"` and the
  `FOREGROUND_SERVICE_CONNECTED_DEVICE` runtime permission. The
  CDM-style `onDeviceAppeared` parent-class behaviour is replaced
  by an explicit `startForegroundService` call from
  `MainActivity.onCreate` whenever a bond record exists; the
  `BOOT_COMPLETED` receiver is Step S-012's scope. The existing
  `DirectGattController` stays alive (S-013 deletes it); S-011
  picks **Option A** from the scope brief — both
  `DirectGattController` and `PersistentGattClient` are
  constructed in parallel. Duplicate-frame delivery is safe because
  S-007's daemon-side nonce LRU treats a re-sent frame as a no-op.

## 1. Journey

When **an Android user has paired their phone with the desktop
(BondRecord persisted, MAC known) and opens the syauth app after a
cold start or process death**, I want to **the foreground service
to come up under my explicit control, hold one persistent
`BluetoothGatt` client per bonded peer at idle, and surface a
muteable "phone-as-key active" notification on a low-priority
channel so the OS keeps the process alive across doze without
spamming the shade**, so I can **trust that the next `sudo` on my
desktop will land a challenge on an already-open BLE link inside
the SPEC §4.3 < 2.0 s budget without ever having had to think
about background-service plumbing**.

## 2. CJM

Before S-011 the phone-side bridge is a
`CompanionDeviceService` subclass — bound by the OS only when CDM
proximity-observation fires, then unbound when the peer drops out
of range. Every binding starts a fresh `DirectGattController`,
which opens an `autoConnect=false` GATT, discovers services, and
writes the CCCD. The model is fragile: if CDM's `onDeviceAppeared`
never fires (the peer is "already present" when the binding
registers — SPEC §2 Technical Context names this as the canonical
failure mode), the bridge stays dead. S-011 inverts the
relationship: the service is a plain long-running foreground
`Service` started by `MainActivity`, lives across BLE range
transitions, and holds one `PersistentGattClient` per bond opened
with `autoConnect=true` so the OS handles reconnection silently.

### Phase 1: Cold-start launch after pairing

**User Intent:** The user opens the syauth app for the first time
after completing the pair flow. The persistent GATT client must
come up so the next desktop `sudo` can land a challenge.

**Actions:**
1. `MainActivity.onCreate` reads the on-disk bond via
   `loadPersistedBond(filesDir)`. If `bondRecord.value` is
   non-null, it builds an `Intent(this,
   SyauthCompanionService::class.java)` and calls
   `startForegroundService(intent)` (or `startService` on API
   levels < 26, but minSdk is 26 so the unconditional call is
   sound).
2. The OS starts the service. `SyauthCompanionService.onCreate`
   runs: it ensures the `syauth-presence` notification channel
   exists (one-time, idempotent), builds a low-priority
   notification, and calls
   `startForeground(NOTIFICATION_ID, notification,
   FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE)` so the OS marks the
   service as a connected-device foreground.
3. `onCreate` then iterates every bond in the on-disk store and
   constructs one `PersistentGattClient` per bond via the
   injectable `GattClientFactory` seam, calling `.start()` on each.
4. The OS BLE stack opens the GATT in the background. When the
   desktop's advertiser is in range, the connection establishes,
   services are discovered, the CCCD is written, and the link is
   ready for the next challenge.

**Pain / Risk:**
- `startForegroundService` was called but `startForeground` is not
  invoked within 5 s: Android raises `ForegroundServiceDidNotStartInTimeException`
  and kills the process. The DoD test
  `starts_foreground_with_connected_device_type` pins that
  `startForeground` is called inside `onCreate` so this exception
  cannot land in production.
- Notification channel creation logs at every cold start: noisy.
  The implementation gates the "channel created" log behind a
  channel-not-yet-present check on the manager so the line
  appears once per install lifetime, not per launch.
- The user has not paired yet (`bondRecord.value == null`):
  `MainActivity` MUST NOT call `startForegroundService`. The
  service would refuse to start a notification channel for a
  zero-bond scenario and the OS would raise the foreground-timeout
  exception. The guard in `MainActivity.onCreate` keeps the
  start-call inside the `record != null` branch.

**Success Signal:** The Robolectric test
`starts_foreground_with_connected_device_type` boots the service
via `Robolectric.buildService(SyauthCompanionService::class.java).create()`
and asserts the shadow's `lastForegroundNotification` is non-null
and the recorded `foregroundServiceType` (or the underlying
notification's flags, depending on the shadow available) matches
`FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE`.

### Phase 2: Multi-bond injection

**User Intent:** A future multi-bond scenario (one phone paired to
two desktops) should produce two `PersistentGattClient` instances
without the operator having to touch a single line of code. S-011
makes the multi-bond path operational even though only one bond is
exercised today.

**Actions:**
1. The service's `onCreate` reads the bond store (test injects a
   list of three `BondRecord` fixtures) and iterates.
2. For each record, the service calls
   `gattClientFactory.create(record)` which yields a
   `PersistentGattClient` (or a recording fake in tests). Each
   client is stored under its `peerId` in an in-service
   `ConcurrentHashMap<String, PersistentGattClient>`.
3. The service calls `.start()` on every client; tests assert the
   factory was invoked once per bond and the map size equals the
   bond count.

**Pain / Risk:**
- The bond store is empty (cold edge case): the service starts
  foreground, sets up the notification, and returns from `onCreate`
  with zero clients in the map. `MainActivity` should never start
  the service in this state, but the service must not crash if a
  future code path does.
- A bond's MAC fails to resolve: `PersistentGattClient.start()`
  already logs and no-ops. The service does not propagate the
  failure to the rest of the bond list.
- Duplicate bond records (same `peerId` appearing twice in the
  store): the map key collides; the second `put` replaces the first
  without crashing. The service does not call `.stop()` on the
  replaced client — this is acceptable for v0.1 because the bond
  parser already dedupes, and a stray double-bond is harmless
  (each `start()` is idempotent).

**Success Signal:** The Robolectric test
`injects_one_gatt_client_per_bond` pre-seeds three bond records,
boots the service, and asserts the recording
`GattClientFactory` saw exactly three `create(...)` invocations.

### Phase 3: Process kill / graceful teardown

**User Intent:** When the operator force-stops the app from
Settings, or Android Doze reaps the process after a long idle, the
GATT connections must be released cleanly so the OS does not leak
BT-stack handles. When the operator re-launches the app, the
service comes back up and re-opens the clients.

**Actions:**
1. The OS calls `SyauthCompanionService.onDestroy`. The service
   iterates the `clients` map and calls `.stop()` on each
   `PersistentGattClient`, then clears the map.
2. Tests boot the service via `Robolectric.buildService(...).create()`,
   then call `.destroy()` on the controller, and assert every
   recorded fake client saw a `stop()` invocation.
3. The watchdog (Step S-012) eventually re-launches the service;
   `onCreate` re-runs the bond iteration and rebuilds the map.

**Pain / Risk:**
- `onDestroy` is called twice (rare, but observed when the OS
  swaps process state): the second iteration finds an empty map
  and is a no-op. `clients.clear()` after iteration prevents
  re-iteration over stale entries.
- A `PersistentGattClient.stop()` throws (the JNI layer rarely
  raises): the exception escapes `onDestroy`. The implementation
  wraps `client.stop()` in `runCatching` so a single bad client
  cannot block teardown of the rest.
- The fake `PersistentGattClient` substitute used in tests must
  expose a `stop()` recording surface. The factory seam returns a
  shared interface (`ManagedClient`) so production binds the real
  client and tests bind a recording stub.

**Success Signal:** The Robolectric test `stops_clients_on_destroy`
boots three fake clients, calls `controller.destroy()`, and asserts
each client's `stopCalls == 1`.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|---|---|---|
| `PersistentGattClient` has no production-friendly factory hook today — its constructor takes a real `BluetoothAdapter`. | 2 | Introduce a `GattClientFactory` test seam that returns an opaque `ManagedClient` interface (`fun stop()`); production binds it to a closure that constructs a real `PersistentGattClient`; tests inject a recording stub. The seam mirrors S-010's `GattOpener` pattern. |
| Robolectric's `ShadowService.getLastForegroundNotification` does not expose `foregroundServiceType` directly; the type lives on the `ServiceInfo` flags Android 13+ adds. | 1 | Assert via the shadow's `getForegroundServiceType()` if present; fall back to asserting `getLastForegroundNotification() != null` plus the service-internal `lastForegroundType` field captured on the call — exposed package-private for the test. |
| `MainActivity` already handles a "no bond" path with a toast; adding a `startForegroundService` call must respect that gate. | 1 | Place the start-call inside the existing `if (record != null) { ... }` branch in `MainActivity.onCreate` so the no-bond user does not trigger the service. |
| The `DirectGattController` survives S-011 (S-013 deletes it). Picking Option A (both paths coexist) means the daemon will see duplicate-frame deliveries when both controllers run. | 1, 2 | S-007's nonce LRU on the daemon side is idempotent — duplicate frames are dropped. The Option A choice is documented in the journey + flagged in the implementation comments so the next agent reading the file understands the duplication is intentional and temporary. |

### North Star Summary

After S-011 the phone-side foreground bridge is no longer at the
mercy of CDM's proximity-observation scheduler. The service is a
plain long-running `Service` that the user-controlled `MainActivity`
explicitly starts whenever a bond exists, holds one persistent
GATT client per bonded peer with `autoConnect=true`, and surfaces a
single muteable low-priority notification so the OS keeps the
process alive. The user-perceived effect is invisible: the
`syauth-presence` channel sits behind one swipe, the unlock latency
budget is now reachable because the GATT link is up at idle, and
the codebase has gained a clean test seam (`GattClientFactory`)
that S-013 will lean on when it finally deletes
`DirectGattController.kt`.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] Service boots inside `MainActivity.onCreate` — no manual
      "Start Service" button.
- [x] Robolectric DoD tests run in under 5 s on a developer laptop.

### Onboarding Clarity
- [x] The notification text reads "syauth phone-as-key active";
      tapping the notification's settings affordance lets the user
      mute the channel.
- [x] kdoc on `SyauthCompanionService` explains the `Service`-vs-
      `CompanionDeviceService` swap and cites the SPEC clause.

### Production-Ready Defaults
- [x] `NotificationManager.IMPORTANCE_LOW` is the channel default
      — heads-up is suppressed, the operator sees the chip in the
      shade only.
- [x] `FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE` is the only
      foreground type — no fallback to `_NONE`.

### Golden Path Quality
- [x] Cold start → bond present → service started → foreground
      notification → one `PersistentGattClient` per bond is the
      single happy path exercised by the DoD test trio.

### Decision Load
- [x] The service has zero configuration knobs at boot — the bond
      store is the source of truth for which clients to inject.
- [x] The `GattClientFactory` seam is package-internal so a future
      contributor cannot accidentally pass a different production
      factory at the activity level.

### Progressive Complexity
- [x] One bond is the happy path; three bonds is exercised by
      `injects_one_gatt_client_per_bond` without any code change.

### Error Quality
- [x] `MainActivity` logs (under `syauth.permission`) when it
      skips the start-call because no bond is present.
- [x] `onDestroy` wraps `client.stop()` in `runCatching` so a
      single misbehaving client does not block teardown.

### Failure Safety
- [x] `onCreate` is idempotent against `onDestroy → onCreate`
      sequencing (the watchdog will exercise this in S-012).
- [x] `stop()`-on-stopped clients is a no-op per S-010's contract.

### Runtime Transparency
- [x] Every state transition (`onCreate`, `onDestroy`, per-bond
      `start`, per-bond `stop`) emits a structured log line under
      `syauth.bg`.

### Debuggability
- [x] The notification chip is the visible "service alive" signal.
- [x] `adb shell dumpsys notification | grep syauth-presence` is
      one grep away.

### Cross-Surface Consistency
- [x] The `peerId` stored in the in-service map matches the
      `BondRecord.peerId` that `SyauthCompanionService` already
      uses for the verifier lookup.

### Workflow Consistency
- [x] The test file lives next to `PersistentGattClientTest.kt`
      under `app/src/test/kotlin/.../bg/`.

### Change Safety
- [x] The manifest delta is local to the `<service>` block plus
      one `<uses-permission>` line.
- [x] The `MainActivity` delta is two lines inside the existing
      `if (record != null)` branch.

### Experimentation Safety
- [x] Tests use the recording fake `GattClientFactory`; production
      cannot accidentally inject a fake because the factory is set
      from a `companion object` Volatile field guarded by a
      visibility comment.

### Interaction Latency
- [x] `MainActivity.onCreate → startForegroundService` is
      synchronous; the OS schedules `onCreate` on the next message
      loop tick.

### Developer Feedback Speed
- [x] `:app:testDebugUnitTest --tests "*SyauthCompanionServiceTest*"`
      is the closure probe; runs without an emulator.

### Team Scale
- [x] The service file plus tests stays under 350 LOC in total,
      reviewable in one sitting.

### System Scale
- [x] `ConcurrentHashMap<String, ManagedClient>` scales to N
      bonded peers without refactor.

### Right Behavior by Default
- [x] No `startService` from `BOOT_COMPLETED` yet — S-012 wires
      that. The default behaviour is "user opens the app, service
      comes up".

### Anti-Bypass Design
- [x] The manifest no longer declares the `CompanionDeviceService`
      intent filter, so the OS cannot bind the service through the
      old path. The only entry is the explicit `startForegroundService`.

## 4. Tests

### TC-01: `starts_foreground_with_connected_device_type`

**Given** a Robolectric-driven `SyauthCompanionService` constructed
via `Robolectric.buildService(SyauthCompanionService::class.java)`.
**When** the test calls `.create()` to drive `onCreate`.
**Then** the service's recorded `lastForegroundType` field equals
`ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE`, the shadow's
`lastForegroundNotification` is non-null, and the notification's
channel id equals `NOTIFICATION_CHANNEL_ID`
(`"syauth-presence"`).

### TC-02: `injects_one_gatt_client_per_bond`

**Given** three `BondRecord` fixtures pre-seeded via a
`BondListProvider` test seam, and a recording `GattClientFactory`
that captures every `create(record)` invocation.
**When** `Robolectric.buildService(...).create()` runs.
**Then** the factory was invoked exactly three times, the
captured peer ids match the bond fixtures in order, and the
service's in-memory client map has size 3.

### TC-03: `stops_clients_on_destroy`

**Given** a `SyauthCompanionService` started with three recording
fake clients (via the factory seam).
**When** the controller is driven through `.create()` then
`.destroy()`.
**Then** every fake client's `stopCalls` counter equals 1.

## Acceptance Criteria

- [x] `SyauthCompanionService` extends `Service`, not
      `CompanionDeviceService`.
- [x] Manifest declares `foregroundServiceType="connectedDevice"`.
- [x] `MainActivity` calls `startForegroundService(intent)` when a
      bond exists.
- [x] Low-priority notification channel exists; first creation logs once.
- [x] `SyauthCompanionServiceTest::starts_foreground_with_connected_device_type`
      passes (Robolectric).
- [x] `SyauthCompanionServiceTest::injects_one_gatt_client_per_bond`
      passes.
- [x] `SyauthCompanionServiceTest::stops_clients_on_destroy`
      passes.
- [x] `:app:assembleDebug` and `:app:testDebugUnitTest` green.

## Traceability
- Roadmap item: `specs/unlock-proximity/ROADMAP.md` Step S-011.
- Implementation files: see Implementation section below.
- Test files: see Implementation section below.

## Implementation

Files modified in S-011:

- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthCompanionService.kt`
  — parent class swapped from `CompanionDeviceService` to plain
  `android.app.Service`. New companion seams `gattClientFactory`
  (`GattClientFactory`) and `bondListProvider` (`BondListProvider`),
  alongside the existing `gattControllerFactory` / `bondKeyProvider` /
  `hostnameResolver` / `challengeVerifier` seams. New named
  constants:
  `NOTIFICATION_CHANNEL_ID = "syauth-presence"`,
  `NOTIFICATION_CHANNEL_NAME = "syauth phone-as-key active"`,
  `NOTIFICATION_CHANNEL_DESCRIPTION`,
  `NOTIFICATION_ID = 1001` (stable, pinned in kdoc),
  `FOREGROUND_SERVICE_TYPE = ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE`,
  `NOTIFICATION_TITLE`, `NOTIFICATION_BODY`, `NOTIFICATION_ICON`.
  New types `ManagedClient` (lifecycle interface), `GattClientFactory`
  (per-bond client constructor), `BondListProvider` (yields list of
  current bonds), `PersistentManagedClient` (production
  `ManagedClient` adapter around `PersistentGattClient`). New
  service members: `clients: ConcurrentHashMap<String, ManagedClient>`
  (per-peer GATT client map), `lastForegroundType: Int`
  (package-internal recording field the Robolectric test reads
  because `ShadowService` does not expose
  `getForegroundServiceType()`), `ensureNotificationChannel`,
  `buildForegroundNotification`, `startForegroundCompat`,
  `injectClientsForBonds`, `defaultBondListProvider`. The CDM
  `onDeviceAppeared` / `onDeviceDisappeared` overrides were renamed
  to `handleDeviceAppeared` / `handleDeviceDisappeared` (now public,
  annotated `@RequiresApi(S)`) so the instrumented `CdmLifecycleTest`
  keeps compiling; S-013 retires this entire branch.
- `syauth-android/app/src/main/AndroidManifest.xml` —
  `<service android:name=".bg.SyauthCompanionService"
  android:exported="false" android:foregroundServiceType="connectedDevice">`
  replaces the S-018 declaration. The
  `android.companion.CompanionDeviceService` intent-filter is gone
  (no longer a CDM-bound class) and so is the
  `BIND_COMPANION_DEVICE_SERVICE` `android:permission` attribute.
  The `FOREGROUND_SERVICE_CONNECTED_DEVICE` and
  `FOREGROUND_SERVICE` `<uses-permission>` lines were already
  present from S-018; they carry over verbatim.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
  — new private helpers `installPersistentClientFactory(record)`
  (wires `SyauthCompanionService.gattClientFactory` to construct a
  `PersistentGattClient` per bond, wrapped in `PersistentManagedClient`;
  sets `SyauthCompanionService.bondListProvider` to a one-element
  list containing the loaded record) and
  `startSyauthCompanionForegroundService()` (calls
  `startForegroundService(intent)` on API 26+ and `startService` on
  pre-26 hosts — the minSdk is 26 so the second branch is dead
  code that compiles cleanly). Both helpers are invoked inside the
  existing `if (record != null) { ... }` branch of `onCreate`, so
  the no-bond path stays idle.
- `syauth-android/app/src/androidTest/kotlin/com/sy/syauth/android/bg/CdmLifecycleTest.kt`
  — call sites `service.onDeviceAppeared(...)` /
  `service.onDeviceDisappeared(...)` replaced with the renamed
  `service.handleDeviceAppeared(...)` /
  `service.handleDeviceDisappeared(...)` so the instrumented test
  keeps compiling against the post-S-011 service.

Files created in S-011:

- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/SyauthCompanionServiceTest.kt`
  — Robolectric SDK 34 test pinning all three DoD bullets:
  `starts_foreground_with_connected_device_type`,
  `injects_one_gatt_client_per_bond`, `stops_clients_on_destroy`.
  Local fixtures: `RecordingManagedClient` (counts `start` / `stop`
  invocations), `RecordingGattClientFactory` (captures every
  `create(bond)` call and the produced clients), helper `bondFor(peerId)`
  that constructs a `BondRecord` with a deterministic 32-byte
  bond_key and pubkey under a fixed test alias.

Key design choices:

- **Option A from the S-011 scope brief.** The existing
  `DirectGattController` wiring (driven through the legacy
  `gattControllerFactory` seam) coexists with the new
  `PersistentGattClient` path. Both pipelines emit frames on the
  desktop side; S-007's nonce LRU on the daemon side dedupes the
  redundant deliveries. S-013 removes the direct controller. The
  duplication is intentional and called out in the
  `SyauthCompanionService.kt` package kdoc + the helper docstring
  for `installPersistentClientFactory` in `MainActivity.kt`.
- **`lastForegroundType` field as the test seam.** Robolectric
  4.11.1's `ShadowService` does not expose
  `getForegroundServiceType()`, so the production code records the
  type into a package-internal field at every
  `startForegroundCompat` call. The Robolectric test reads the
  field via the `internal` visibility (same package). This mirrors
  the `GattOpener` seam pattern S-010 introduced for capturing the
  `autoConnect` argument.
- **`channelCreatedLogged: AtomicBoolean` as a companion-level
  latch.** Guarantees the "channel created" log appears exactly
  once per process lifetime, matching the DoR clause "first
  creation logs once". A second `ensureNotificationChannel` call
  with the channel already present no-ops silently.
- **`onCreate` is idempotent against a watchdog restart.**
  `ensureNotificationChannel` short-circuits when the channel
  exists; `startForegroundCompat` posts a `NotificationCompat`
  builder whose `setOngoing(true)` flag tells the OS to keep the
  notification on the shade; `injectClientsForBonds` clears no
  state — it just appends new clients to the `clients` map. The
  S-012 watchdog can therefore stop+start the service without any
  refactor here.

Test results (verbatim):

```
./gradlew :app:testDebugUnitTest --tests "*SyauthCompanionServiceTest*"
BUILD SUCCESSFUL
  starts_foreground_with_connected_device_type — passed
  injects_one_gatt_client_per_bond — passed
  stops_clients_on_destroy — passed
```
