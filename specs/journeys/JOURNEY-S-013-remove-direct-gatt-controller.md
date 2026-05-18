# JOURNEY-S-013: Remove `DirectGattController` (tonight's hot-fix path)

> **Spec anchors:** `specs/unlock-proximity/SPEC.md` §3 scope item #19
> — "`AndroidCdmPairCompanionScanner.startObservingDevicePresence` is
> KEPT as a belt-and-suspenders signal for the foreground service's
> watchdog (re-launches if killed and proximity event fires), but the
> primary connection path is the `autoConnect=true` GATT client. The
> "tonight's hot-fix" CDM-only path is removed."
>
> §3.2 D8 — "no fallback hot-fix CDM-only path" (anchored in
> `ROADMAP.md` Traceability matrix row S-013).
>
> §3 row "Phone connection lifecycle" — "One persistent `BluetoothGatt`
> per bonded peer, opened with `autoConnect=true` and held by
> `SyauthCompanionService` as a long-running foreground service."
>
> **Roadmap row:** `specs/unlock-proximity/ROADMAP.md` Step S-013.
>
> **Closure condition (verbatim from ROADMAP.md):**
>
> ```
> git ls-files syauth-android/ | grep DirectGattController   # empty
> ./gradlew :app:assembleDebug
> ./gradlew :app:testDebugUnitTest
> ```

## Roadmap Link
- Source roadmap: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-013.
- Feature: delete the `DirectGattController.kt` file together with the
  `GattControllerFactory` extension point on `SyauthCompanionService`
  and the `installGattControllerFactory` wire-up in `MainActivity`.
  After this step, `SyauthCompanionService` constructs
  `PersistentGattClient` directly (via the `GattClientFactory` seam
  installed by S-011); there is no longer a CDM-style
  `handleDeviceAppeared`/`Disappeared` codepath on the service. The
  `AndroidCdmPairCompanionScanner.startObservingDevicePresence`
  watchdog signal stays — that's the belt-and-suspenders kept by SPEC
  §3 item #19.

## 1. Journey

When **an Android user has paired their phone with the desktop and
the foreground `SyauthCompanionService` is alive holding one
`PersistentGattClient` per bonded peer (the S-011 persistent path),
and the desktop emits a fresh `sudo` challenge frame**, I want to
**only one Android-side code path delivers that frame to
`ApproveNotification.show` — the `PersistentGattClient` notify
callback — and never the legacy `DirectGattController` opened by the
CDM `handleDeviceAppeared` hot-fix**, so I can **eliminate the
duplicate-frame delivery the S-007 nonce LRU has been silently
absorbing on every unlock, shrink the surface area `pam_syauth`
trusts, and keep the SPEC §3.2 D8 "no fallback hot-fix CDM-only
path" invariant load-bearing rather than a comment-anchored
aspiration**.

## 2. CJM

S-010 introduced `PersistentGattClient` as a sibling of
`DirectGattController`. S-011 rewired `SyauthCompanionService` from
`CompanionDeviceService` to a plain foreground `Service`, plumbed the
new `GattClientFactory` / `BondListProvider` seams, and made
`MainActivity.installPersistentClientFactory` the production wire-up
for the persistent path. S-011 deliberately kept Option A: both the
legacy `DirectGattController` factory (still installed by
`installGattControllerFactory`) and the new persistent path coexisted
so the demo could ship while the persistent path bedded in. S-007's
nonce LRU on the daemon side hid the duplicate-frame artifact — every
challenge arrived once via the persistent notify callback and once
via the CDM-triggered direct controller, and `pam_syauth` accepted
the first and dropped the second.

S-013 closes the loop. The persistent path has been observed live
(S-011 closure) and the duplicate path is now technical debt: a
parallel implementation that obscures which code actually drives
unlock, a manifest-grade SPEC deviation surviving on inertia, and an
attack-surface multiplier. This step deletes the file, deletes the
seam, deletes the `MainActivity` installer, and deletes the
instrumented test that asserted the CDM-style binding (which, after
S-011, never fires for a primary unlock anyway).

### Phase 1: Before deletion — the two paths coexist

**User Intent:** The user is unaware of the topology; they just want
`sudo` to unlock. The codebase carries both paths because S-011
chose Option A.

**Actions:** None at the user level. At the system level: the
desktop emits a challenge notification on
`SYAUTH_CHALLENGE_CHAR_UUID`; the persistent `BluetoothGatt` (opened
with `autoConnect=true` at service start) receives it in
`onCharacteristicChanged` and forwards through the
`PersistentGattClient` → `PersistentManagedClient` chain. Meanwhile,
if CDM fires `handleDeviceAppeared` for the same association, the
legacy `gattControllerFactory` opens a *second* `BluetoothGatt`, runs
its own discover/subscribe, and forwards the same frame through the
direct path.

**Pain / Risk:**
- Two GATT clients held open per bonded peer doubles the BLE link
  cost (radio + memory) for no functional gain.
- `pam_syauth` sees the same nonce twice on every unlock; the S-007
  nonce LRU absorbs it silently, which means a future drift in the
  nonce window (e.g., an S-007 cache shrink) would surface as a
  "random" unlock-fail instead of a "two paths are sending duplicate
  frames" symptom.
- The CDM hot-fix path is the SPEC §3.2 D8 deviation kept alive only
  by the comment `// Survives until S-013 retires DirectGattController.kt`.
  As long as it's wired, every reviewer has to know to ignore it.
- Reviewers reading the wire-up see two installers
  (`installGattControllerFactory` + `installPersistentClientFactory`)
  and cannot tell which one is load-bearing without
  cross-referencing the SPEC.

**Success Signal:** `git grep -n "DirectGattController" syauth-android/`
returns non-empty results in `MainActivity.kt`,
`SyauthCompanionService.kt`, and the `bg/DirectGattController.kt` file
itself — the canonical "this is the state to leave behind" probe.

### Phase 2: Deletion — file, seam, wire-up, test all go

**User Intent:** The user still wants `sudo` to unlock. The
deletion must be transparent at the user level.

**Actions:**
1. Delete `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/DirectGattController.kt`.
2. Remove the `GattControllerFactory` interface declaration from
   `SyauthCompanionService.kt` (the `public fun interface` block).
3. Remove the companion-object `gattControllerFactory` field and its
   entry in `resetSeams()`.
4. Remove the `handleDeviceAppeared` and `handleDeviceDisappeared`
   methods (they only existed to drive the legacy controller).
5. Remove the `controllers: ConcurrentHashMap<Int, GattServerController>`
   field and the matching tear-down loop in `onDestroy`.
6. Remove `installGattControllerFactory(record)` and its call in
   `installCompanionSeams` in `MainActivity.kt`.
7. Delete the instrumented `CdmLifecycleTest.kt` — every one of its
   three test methods constructs a `GattControllerFactory` and drives
   `handleDeviceAppeared`/`Disappeared`, both of which no longer
   exist. The persistent path has its own JVM coverage in
   `SyauthCompanionServiceTest.kt`; the CDM lifecycle assertions are
   orphaned by construction.
8. Scrub orphan `DirectGattController` mentions in source-comments
   (`MainActivity.kt`, `SyauthCompanionService.kt`,
   `PersistentGattClient.kt`) so the grep probe is mechanically
   empty.

**Pain / Risk:**
- `GattServerController` is still in use by `BleScanController.kt`
  (the inverted-role implementation introduced for the DEV-003
  phone-side advertising path) — must NOT be removed. The deletion
  scope is the `GattControllerFactory` extension point, not the
  underlying `GattServerController` interface.
- `SYAUTH_CHALLENGE_CHAR_UUID` and `SYAUTH_RESPONSE_CHAR_UUID` are
  declared in `bg/GattServer.kt`, not in `DirectGattController.kt`,
  so the deletion does not orphan them and no `GattConstants.kt`
  move is required.
- The `CdmLifecycleTest.kt` androidTest is the only test surface
  that asserted the CDM-style binding lifecycle. Its deletion is
  load-bearing for the deletion to compile against the androidTest
  source set — leaving it would surface as a compile error on
  `:app:assembleDebugAndroidTest`.

**Success Signal:** `git grep -n "DirectGattController" syauth-android/`
returns an empty result. `git grep -n "GattControllerFactory" syauth-android/`
also returns empty. `:app:assembleDebug` and
`:app:testDebugUnitTest` are both green.

### Phase 3: After — only the persistent path emits challenges

**User Intent:** The user `sudo`s on the desktop. The phone signs
the challenge and the unlock completes.

**Actions:** Desktop emits a notify on `SYAUTH_CHALLENGE_CHAR_UUID`;
the single persistent `BluetoothGatt` per bonded peer receives it in
`onCharacteristicChanged`; the `PersistentManagedClient` (held by
`SyauthCompanionService`) routes the bytes to the approve flow; the
approve flow signs and writes the response on
`SYAUTH_RESPONSE_CHAR_UUID` through the same handle.

**Pain / Risk:**
- A future contributor re-introducing a CDM-style binding would
  have to add a brand-new factory + interface; the path no longer
  has a "default-off" knob to flip.
- The duplicate-frame artifact disappears, which makes the S-007
  nonce LRU's role narrower (it now only defends against actual
  network replay, not against our own duplicate emission). That
  reduction is the win, not a risk.
- `make scope-discipline` enforcement is now stricter — the
  `// Survives until S-013` comment that anchored the legacy seam
  is gone, and `git grep -F "S-013 retires"` returns empty so no
  ROADMAP-anchored future-tense excuse remains in the codebase.

**Success Signal:** Every challenge frame at `pam_syauth` carries a
fresh nonce; the S-007 nonce LRU stops absorbing duplicates;
`git grep -nE "GattControllerFactory|handleDeviceAppeared|handleDeviceDisappeared" syauth-android/`
returns empty.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Two installers in `MainActivity.installCompanionSeams` (`installGattControllerFactory` + `installPersistentClientFactory`) make it ambiguous which one drives unlock | 1 | After deletion, `installCompanionSeams` calls only `installPersistentClientFactory`; reviewers see a single load-bearing wire-up. |
| `CdmLifecycleTest` androidTest asserts a binding lifecycle that no longer fires for a primary unlock after S-011 | 1 | Delete the test along with the seam it covered; the persistent path's JVM-side coverage (`SyauthCompanionServiceTest`) is the new source of truth. |
| `SyauthCompanionService.controllers` map holds entries only the legacy CDM path populates; the field still gets cleared in `onDestroy` for no functional reason | 1 | Removing it shrinks the service's mutable state and clarifies that the only resource the service owns at runtime is the per-bond `ManagedClient` map. |

### North Star Summary

After S-013 closes, the syauth Android app has exactly one Android
→ desktop unlock path: the foreground `SyauthCompanionService` holds
one persistent `BluetoothGatt` per bonded peer, opened with
`autoConnect=true`, and every challenge frame arrives through the
`PersistentGattClient` notify callback. No CDM-style hot-fix path
exists. The `GattControllerFactory` extension point and its legacy
`DirectGattController` implementation are gone, the
`installGattControllerFactory` wire-up in `MainActivity` is gone,
the orphaned `CdmLifecycleTest` androidTest is gone, and `git grep`
for `DirectGattController` returns empty. SPEC §3.2 D8 "no fallback
hot-fix CDM-only path" goes from comment-anchored to
grep-anchored.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] Unlock latency is unchanged at the user level: the persistent
      path was already running, so SPEC §4.3 < 2.0 s budget is
      preserved.
- [x] No onboarding step changes — the user does not interact with
      either path directly.

### Onboarding Clarity
- [x] No new operator surface introduced; deletion-only step.
- [x] Error messages on the foreground service (`appeared peer=N but
      no factory installed`) disappear because the call site goes
      away.

### Production-Ready Defaults
- [x] After deletion, the only Android-side default is the persistent
      `autoConnect=true` client. There is no toggle.
- [x] The `MainActivity` wire-up is a single installer call,
      `installPersistentClientFactory`.

### Golden Path Quality
- [x] The unlock flow continues to work end-to-end via the
      persistent path; duplicate frames stop arriving at
      `pam_syauth`.
- [x] `:app:testDebugUnitTest` covers the persistent path
      end-to-end via `SyauthCompanionServiceTest`.

### Decision Load
- [x] One installer in `installCompanionSeams` instead of two
      reduces reviewer decision load.
- [x] `SyauthCompanionService.companion` exposes one factory seam
      (`gattClientFactory`) instead of two.

### Progressive Complexity
- [x] No new opt-in features added; the codebase shrinks.
- [x] The remaining `GattServerController` interface (used by
      `BleScanController`) is unchanged.

### Error Quality
- [x] No new error paths introduced.
- [x] The deletion does not silence any error the user previously
      saw — it removes a *successful* duplicate path.

### Failure Safety
- [x] `make scope-discipline` enforces no orphan future-tense
      anchors after the deletion.
- [x] `:app:assembleDebug` is the compile-level mechanical probe.

### Runtime Transparency
- [x] After deletion, every `Log.i(SYAUTH_BG_LOG_TAG, ...)` line in
      a single unlock attempt corresponds to exactly one frame.
- [x] No hidden duplicate-frame state.

### Debuggability
- [x] Field operators reading `adb logcat -s syauth.bg` see one
      `challenge frame received` per unlock instead of two.
- [x] The `syauth.bg.direct` log tag disappears with the file.

### Cross-Surface Consistency
- [x] The persistent path is the only documented path in SPEC §3
      "Phone connection lifecycle"; the codebase now matches.
- [x] Terminology (`PersistentGattClient`, `ManagedClient`,
      `GattClientFactory`) is the only vocabulary that remains.

### Workflow Consistency
- [x] `MainActivity.installCompanionSeams` continues to follow the
      "install seam → start service" sequence; the inner call set
      shrinks by one.
- [x] No artifact-layout changes.

### Change Safety
- [x] The closure-condition grep is the mechanical preview.
- [x] No silent user customizations are touched.

### Experimentation Safety
- [x] The deletion is fully reversible by `git revert` until merged.
- [x] No `cfg(demo)` or build-flag remnants left behind.

### Interaction Latency
- [x] Unchanged — same persistent connection.
- [x] No new I/O on the deletion path.

### Developer Feedback Speed
- [x] `:app:assembleDebug` exit code is the immediate compile-level
      gate.
- [x] `:app:testDebugUnitTest` exit code is the immediate
      behavior-level gate.

### Team Scale
- [x] One fewer wire-up to teach new contributors.
- [x] The SPEC-deviation comment anchor disappears, so reviewers no
      longer have to remember the exception.

### System Scale
- [x] One fewer `BluetoothGatt` handle per bonded peer at idle.
- [x] One fewer log-tag domain to forward to crashlytics-style
      tooling.

### Right Behavior by Default
- [x] After deletion, the default Android-side topology is the SPEC
      §3 canonical one.
- [x] No flag exists to toggle the legacy path back on.

### Anti-Bypass Design
- [x] `make scope-discipline` enforces no orphan `// Survives until
      S-013` anchors.
- [x] The `git grep "DirectGattController"` empty assertion is the
      hard quality gate.

## 4. Tests

### TC-01: `:app:assembleDebug` compiles after deletion

**Given** the deletion of `DirectGattController.kt`, the
`GattControllerFactory` interface, the `gattControllerFactory`
companion field, the `handleDeviceAppeared` / `handleDeviceDisappeared`
methods, the `controllers` map, and `installGattControllerFactory`.
**When** Gradle runs `:app:assembleDebug`.
**Then** the build succeeds with no compile error referring to any
of those removed symbols and no orphan import.

### TC-02: `:app:testDebugUnitTest` stays green

**Given** the deletion above plus the deletion of `CdmLifecycleTest.kt`
(orphaned androidTest).
**When** Gradle runs `:app:testDebugUnitTest`.
**Then** every JVM unit test passes — `SyauthCompanionServiceTest`
in particular, which covers the persistent path
(`starts_foreground_with_connected_device_type`,
`injects_one_gatt_client_per_bond`, `stops_clients_on_destroy`).

### TC-03: `git grep` for the deleted symbols is empty

**Given** the deletion has happened.
**When** the reviewer runs `git grep -n "DirectGattController" syauth-android/`
and `git grep -n "GattControllerFactory" syauth-android/`.
**Then** both invocations return zero lines — the SPEC §3.2 D8
"no fallback hot-fix CDM-only path" invariant is grep-anchored, not
comment-anchored.

### TC-04: `make scope-discipline` is clean after the deletion

**Given** the deletion has happened and the orphan
`// Survives until S-013` anchor in `SyauthCompanionService.kt` is
gone.
**When** the operator runs `make scope-discipline`.
**Then** the target reports `Scope-discipline grep clean.` and exits
zero.

### TC-05: One installer remains in `MainActivity.installCompanionSeams`

**Given** the deletion has happened.
**When** the reviewer reads `MainActivity.installCompanionSeams`.
**Then** the body calls `installPersistentClientFactory` exactly once
and contains no other GATT-factory installer.

## Traceability
- Roadmap item: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-013.
- Implementation files:
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/DirectGattController.kt` (deleted).
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthCompanionService.kt` (modified).
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt` (modified).
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/PersistentGattClient.kt` (comment scrub).
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/BleScanController.kt` (comment scrub).
- Test files:
  - `syauth-android/app/src/androidTest/kotlin/com/sy/syauth/android/bg/CdmLifecycleTest.kt` (deleted — orphaned by the seam removal; the persistent path's JVM coverage in `SyauthCompanionServiceTest.kt` is the new authority).

## Implementation

### Files created
- `specs/journeys/JOURNEY-S-013-remove-direct-gatt-controller.md` (this document).

### Files modified
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthCompanionService.kt`
  — removed the `GattControllerFactory` `public fun interface`, the
  `controllers: ConcurrentHashMap<Int, GattServerController>` field
  and its `onDestroy` tear-down loop, the
  `handleDeviceAppeared` / `handleDeviceDisappeared` public methods,
  the private `handleChallenge` helper, the companion-object
  `gattControllerFactory` field, its entry in `resetSeams()`, the
  unused `android.companion.AssociationInfo` import, and the legacy
  reference in the file-header comment.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
  — removed the `installGattControllerFactory(record)` helper and
  its call from `installCompanionSeams`.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/PersistentGattClient.kt`
  — file-header comment scrubbed; no behavioural change.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/BleScanController.kt`
  — KDoc on `SyauthBleScannerController` scrubbed of the
  `gattControllerFactory` mention; no behavioural change.
- `specs/unlock-proximity/ROADMAP.md` — DoD bullets ticked,
  Traceability paragraph appended to Step S-013.

### Files deleted
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/DirectGattController.kt`
  — the 154-line CDM-only hot-fix path called out in the roadmap.
- `syauth-android/app/src/androidTest/kotlin/com/sy/syauth/android/bg/CdmLifecycleTest.kt`
  — the only test surface that asserted the removed
  `handleDeviceAppeared` / `handleDeviceDisappeared` /
  `gattControllerFactory` symbols. Orphaned by construction once
  those symbols go.

### Verification

Closure-condition probes from the roadmap:

```
$ git ls-files syauth-android/ | grep DirectGattController
(empty)

$ JAVA_HOME=/usr/lib/jvm/java-21 ./gradlew :app:assembleDebug
BUILD SUCCESSFUL in 2s

$ JAVA_HOME=/usr/lib/jvm/java-21 ./gradlew :app:testDebugUnitTest
BUILD SUCCESSFUL in 6s
```

Additional probes (this journey doc's TC-03 / TC-04):

```
$ git grep -n "DirectGattController" syauth-android/
(empty, exit 1)

$ git grep -n "GattControllerFactory" syauth-android/
(empty, exit 1)

$ make scope-discipline
Scope-discipline grep clean.
```

JVM unit-test totals (`:app:testDebugUnitTest`):

```
tests=91 skipped=0 failures=0 errors=0
```

Rust workspace totals (`make test`):

```
passed=387 failed=0 ignored=8
```

(The 8 ignored tests are radio-gated DEV-004 integration tests; pre-existing, unrelated to S-013.)
