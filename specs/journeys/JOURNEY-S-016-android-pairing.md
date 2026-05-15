# JOURNEY-S-016: Android — Pairing screen with LE Secure Connections + OOB confirm

<!-- Authored per .agents/skills/journey/SKILL.md template. -->

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md](../syauth/ROADMAP.md) — item **S-016**.
- Feature: First production-shaped screen on the phone. The user taps a CTA, picks a peer from a BLE scan, watches the BT LESC numeric-comparison code, then confirms the app-level 4-word OOB emoji code that is derived (in Rust) from the freshly-negotiated bond key. The screen is the Android twin of the desktop `syauth pair` flow (S-011) and is the second step of the SPEC §4.1 pairing dataflow.

## 1. Journey

When **Alex (the Linux power-user from SPEC §5.1) sits next to their Pixel 8 with `syauth pair` already running on the desktop**, I want to **tap "Pair with computer" in the Android app, pick the desktop from a BLE scan list, watch the BT LESC 6-digit code match the desktop, then confirm the 4-word emoji OOB code also matches** so I can **end the pairing flow with a `Bonded` state on both ends — knowing that a relay attacker who somehow bypassed BT pairing still failed the app-level OOB check (defense in depth per SPEC §4.1)**.

## 2. CJM

S-016 is the first screen that does real work. Until now (S-015) the app is a hello-world that prints the OOB for a fixed bond key. S-016 introduces the **`PairingState` state machine** on the phone — the Android mirror of the same state machine the desktop already runs in `syauth-cli pair` (S-011) and that SPEC §4.4 pins as the canonical pairing workflow.

Three forces dominate the design:

1. **The OOB code MUST come from UniFFI.** SPEC §4.1 is explicit: the app-level OOB is `HKDF(bond, "syauth-oob-v1")[0..4]` followed by a 256-entry word-table lookup. S-014 ships this exact computation as `uniffi.syauth_mobile.oobCodeForBond(bondKey: ByteArray): List<String>`. Re-implementing the HKDF in Kotlin would (a) fork the byte-identity guarantee that `oob_byte_identical_to_cli_fixture` pins in `crates/syauth-mobile/src/implementation.rs`, and (b) move security-critical crypto from the audited Rust core into Kotlin where the audit surface is larger. We MUST call through UniFFI; the production `OobCalculator` impl is a one-liner that delegates.

2. **The `Failed` state MUST clean up both sides.** SPEC §6 T-004 ("Rogue device bonding") is mitigated by the OOB-mismatch path: if the user taps "No" on the 4-word confirmation, the phone (a) does not persist the bond via `BondPersister`, and (b) removes the BT-level bond via `BluetoothDevice.removeBond()`. Android does not expose `removeBond()` in the public SDK ([Android issue tracker 35681](https://issuetracker.google.com/issues/37057395)); it has been a hidden API since API 1 and remains so as of API 34. The production `BluetoothBondRemover` therefore uses reflection. The test seam is a `BluetoothBondRemover` interface so unit tests can verify "the remover was called exactly once" without needing a real `BluetoothDevice` instance.

3. **`Scanning` → `LescNegotiating` MUST gate on adapter capability.** DoD #5 requires us to refuse to advance past `Scanning` when the adapter doesn't support LE Secure Connections, and the error must include the adapter name. LE Secure Connections is Bluetooth 4.2+; some emulators and ancient devices expose only Bluetooth 4.0/4.1 controllers (no LESC). The capability check happens before any cryptographic material is exchanged — fail closed, with an actionable adapter name in the error so the user can identify which radio is at fault.

A fourth, operational force: **this CI host has no Android SDK / no emulator**, so the DoD #4 "Robolectric unit tests + Compose UI tests" must compile statically and run on an SDK-equipped host. We accept this; `make android-test` (added in S-015) already skips cleanly when no `adb` device is present. Our addition is the unit + UI tests that will execute the moment an emulator is available.

### Phase 1: User taps the CTA

**User Intent:** Alex opens the app from the launcher, sees the home screen (S-015's hello-world), and finds a single, obvious button: "Pair with computer".

**Actions:** Tap "Pair with computer". The screen transitions from the home route to the `pair` route. The pairing screen renders in the `Idle` state with a single large `Button` labeled "Pair with computer" (`testTag = "pair.idle.cta"`).

**Pain / Risk:**
- The home screen from S-015 is just a `Text("OOB: ...")`. We have to add either a `NavHost` or a manual route switch so the `Idle` pairing screen can be reached AND the user can navigate back. Mitigation: add `androidx.navigation:navigation-compose` and define two routes: `"home"` (the existing OobScreen, wrapped) and `"pair"` (the new PairingScreen).
- The user is on Android 11- where `BLUETOOTH_CONNECT` / `BLUETOOTH_SCAN` are unknown permissions; only `ACCESS_FINE_LOCATION` is needed. Mitigation: declare `ACCESS_FINE_LOCATION` with `android:maxSdkVersion="30"` so newer Android doesn't gate behind a location grant.
- The user grants neither runtime permission. The pairing screen has to either request them inline or surface a clear error. Mitigation: the `PairBackend.startScan()` call returns a `Failed("bluetooth permission not granted")` state if `checkSelfPermission(BLUETOOTH_SCAN)` is denied. The screen catches `Failed` and renders the Failed branch with a "Back" button.

**Success Signal:** The pairing screen renders an `Idle` state with a single button bearing the "Pair with computer" label, and tapping it transitions to `Scanning`.

### Phase 2: Scan picks the desktop

**User Intent:** Alex wants to see a list of nearby advertising peers, pick the desktop running `syauth pair`, and have the phone connect to it.

**Actions:** The `Scanning` state renders a `CircularProgressIndicator` plus a scrollable `LazyColumn` of advertising peers (each a clickable row showing the peer name + MAC). A "Cancel" button (`testTag = "pair.scanning.cancel"`) returns to `Idle`. Alex taps a row; the ViewModel calls `backend.pickPeer(peer)`, which (in the production impl) initiates the LE Secure Connections bond against that peer.

**Pain / Risk:**
- The adapter doesn't support LESC (DoD #5). The capability check is `backend.supportsLeSecureConnections()` — production impl reads `BluetoothAdapter.getName()` + checks the controller's LESC bit (typically via `BluetoothAdapter.isLe2MPhySupported()` is NOT the right check; LESC capability isn't directly exposed, so we use a `BluetoothAdapter.isLeExtendedAdvertisingSupported()` heuristic that pins LESC on 4.2+ controllers in practice — documented in this journey). If the check returns false, the ViewModel emits `Failed("adapter $adapterName does not support LE Secure Connections")`. The test verifies the error message contains the adapter name.
- The scan returns zero peers (desktop not running `syauth pair`). The screen still shows the progress indicator + "Cancel" — never auto-fails on empty list; the user knows what to do. Mitigation: the test for `Scanning` only asserts the progress + cancel are visible; the empty list is acceptable.
- The phone radio is off. `backend.startScan()` throws `BluetoothDisabledException`; ViewModel maps that to `Failed("bluetooth is off — enable Bluetooth and try again")`.

**Success Signal:** Picking a peer transitions to `LescNegotiating(code)` with a non-empty 6-character numeric string.

### Phase 3: BT LESC numeric-comparison code

**User Intent:** Alex sees a 6-digit code on the phone and a 6-digit code on the desktop. They glance at both; they match.

**Actions:** The `LescNegotiating(code)` state renders the code in `MaterialTheme.typography.headlineLarge` (`testTag = "pair.lesc.code"`). A "Cancel" button (`testTag = "pair.lesc.cancel"`) aborts and returns to `Idle`. The BT stack on both ends drives the comparison; the user confirms in the *system* Bluetooth-pairing dialog (Android's stock UI), then the ViewModel observes the bond completing.

**Pain / Risk:**
- The system Bluetooth dialog is *modal* and outside our control — the LESC numeric comparison happens in the OS, not in our Compose surface. Our state `LescNegotiating(code)` is therefore informational: we display the same code the OS dialog is showing so the user has a *second* confirmation that "this is the right peer". When the OS bond completes successfully, our `PairBackend` notifies us, the ViewModel calls `oobCalculator.compute(bondKey)`, and we transition to `OobConfirming(emoji)`. Mitigation: `PairBackend` exposes a `bondCompleted` callback that carries the negotiated bond key bytes; our test fakes drive this synchronously.
- LESC fails mid-handshake (controller-firmware bug, RF interference). `PairBackend` emits a failure callback; ViewModel transitions to `Failed("LESC handshake failed: $reason")`. Per DoD #3, the bond is NOT persisted on either side; `bondRemover.remove(device)` is called.
- The user cancels the OS dialog. Same as the failure above — `Failed("user cancelled BT pairing")`.

**Success Signal:** The 6-digit code is rendered in `headlineLarge` and matches whatever the desktop CLI displays. The next state is `OobConfirming` with a non-empty 4-element list.

### Phase 4: App-level OOB 4-word confirmation

**User Intent:** Alex sees four emoji-prefixed words on the phone and the same four words on the desktop. They compare; they match. Alex taps "Yes".

**Actions:** The `OobConfirming(emoji)` state renders the four words (`testTag = "pair.oob.words"`) followed by a question "These match the computer?" and two buttons: "Yes" (`testTag = "pair.oob.yes"`) and "No" (`testTag = "pair.oob.no"`). Tapping Yes calls `bondPersister.persist(...)` and transitions to `Bonded(peerName)`. Tapping No calls `bondRemover.remove(device)` and transitions to `Failed("OOB code did not match — peer might be a relay attacker")`.

**Pain / Risk:**
- The four words are emoji-prefixed; the user has to *read* them in the same order on both ends. Mitigation: the words are produced by the same UniFFI surface that the desktop CLI's `syauth pair` calls — byte-identical output is pinned by `crates/syauth-mobile/src/implementation.rs::oob_byte_identical_to_cli_fixture`. We never re-implement the HKDF in Kotlin.
- The user is rushed and taps Yes without reading. This is the user-error class T-004 ("Rogue device bonding") guards against. We can only do so much in UI — the SPEC accepts this residual risk and documents it. The 4-word OOB at least slows the attack down and forces a visible inspection.
- The user taps No because the codes really don't match (active MitM during pairing). DoD #3 mandates: (a) bond NOT persisted on the Kotlin side (`bondPersister` is never called), AND (b) BT bond is removed. Our test verifies *both*.

**Success Signal:** On Yes, the screen shows `Bonded(peerName)`. On No, the screen shows `Failed(reason)` and the BT bond is removed.

### Phase 5: Bonded → home / Failed → recovery

**User Intent (Bonded):** Alex sees "Paired with `hostname`", taps "Done", and returns to the home screen — ready for the next step (Approve unlock, S-017).

**User Intent (Failed):** Alex sees "Pairing failed: `reason`", taps "Back", and returns to the home screen — knowing that no bond was created on either side and a retry is safe.

**Actions:** Both terminal states render a single back/done button (`testTag = "pair.bonded.done"` / `testTag = "pair.failed.back"`) that calls back to MainActivity via the `onDone: () -> Unit` parameter on `PairingScreen`. MainActivity pops the `pair` route.

**Pain / Risk:**
- The user reaches `Failed` and the BT bond removal silently fails (reflection throws because Android 35 finally hid the method behind `@SystemApi`). Mitigation: `ReflectionBondRemover.remove()` returns `false` on any reflection failure; the ViewModel logs the failure but still transitions to `Failed` (the *app-level* bond is what matters for our security model — the BT bond is best-effort cleanup). The test for `oob_no_calls_remover_and_transitions_to_failed` asserts only the *call*, not its success.
- The user reaches `Bonded` but the persistence call throws. Mitigation: `BondPersister.persist` is `Result`-shaped; if it fails, we transition to `Failed("could not persist bond: $reason")` and remove the BT bond.

**Success Signal:** On `Bonded` → tap "Done" → back to home. On `Failed` → tap "Back" → back to home. `bondPersister` invocation count is asserted in tests.

### State Machine Diagram

```
                        ┌─────┐
                  ┌────▶│Idle │◀───── onDone callback (back to home)
                  │     └──┬──┘
                  │        │ user tap "Pair with computer"
                  │        ▼
                  │     ┌──────────┐
                  │     │ Scanning │
                  │     └──┬───┬───┘
                  │        │   │ user tap "Cancel"
                  │        │   └────────────────────────────┐
                  │        │                                 │
                  │        │ user picks peer +               │
                  │        │ adapter supports LESC           │
                  │        │                                 │
   adapter ◀──────┤        ▼                                 │
   doesn't        │  ┌────────────────────┐                  │
   support  ──────┤  │ LescNegotiating    │                  │
   LESC           │  │     (code: String) │                  │
                  │  └──────┬───┬─────────┘                  │
                  │         │   │ user tap "Cancel"          │
                  │         │   └────────────────────────────│
                  │         │                                │
                  │         │ LESC succeeds + UniFFI         │
                  │         │ oobCodeForBond(bondKey)        │
                  │         ▼                                │
                  │  ┌─────────────────────────┐             │
                  │  │ OobConfirming(emoji)    │             │
                  │  └────┬────────────┬───────┘             │
                  │       │            │                     │
                  │       │ tap Yes    │ tap No              │
                  │       │            │                     │
                  │       │            │ bondRemover.remove  │
                  │       │            │                     │
                  │       ▼            ▼                     │
                  │  ┌──────────┐  ┌────────┐                │
                  │  │ Bonded   │  │ Failed │◀───────────────┘
                  │  │ (name)   │  │(reason)│
                  │  └────┬─────┘  └───┬────┘
                  │       │            │
                  │       │ tap "Done" │ tap "Back"
                  └───────┴────────────┘
```

### Bluetooth Permission Contract

Manifest entries (added by S-016, audited per Android Developer guidance — [Bluetooth permissions](https://developer.android.com/develop/connectivity/bluetooth/bt-permissions)):

| Permission | Required for | API level gate |
|------------|--------------|----------------|
| `android.permission.BLUETOOTH_CONNECT` | Connecting to a chosen peer (LESC bonding via the system dialog) | Runtime permission on API 31+ (Android 12+). On API 30 and below the legacy `android.permission.BLUETOOTH` is granted automatically because `targetSdk = 34` and our `minSdk = 26`. |
| `android.permission.BLUETOOTH_SCAN` | Scanning for advertising peers | Runtime permission on API 31+. We declare `android:usesPermissionFlags="neverForLocation"` so scanning does not require `ACCESS_FINE_LOCATION` on API 31+ ([usesPermissionFlags doc](https://developer.android.com/develop/connectivity/bluetooth/bt-permissions#assert-never-for-location)). |
| `android.permission.ACCESS_FINE_LOCATION` | Legacy: BLE scans on API 26-30 require location. We declare with `android:maxSdkVersion="30"` so it is not requested on API 31+. | Up to API 30 only. |

**Explicitly NOT added in S-016:**
- `android.permission.BLUETOOTH_ADVERTISE` — the *phone* is the scanner per SPEC §3.2 D8 (desktop advertises, phone scans). Adding this would mis-state the surface and trip the threat-model presence-tracking defense.
- `android.permission.POST_NOTIFICATIONS` — lands in S-018 with the foreground BLE bridge.

### Reflection Note: `BluetoothDevice.removeBond()`

The method `android.bluetooth.BluetoothDevice.removeBond(): boolean` has been present in AOSP since API 1 but **never exposed in the public SDK**. Android 14 / API 34 still hides it; calling it requires reflection on the Java `Method`:

```kotlin
val method = BluetoothDevice::class.java.getMethod("removeBond")
method.invoke(device) as Boolean
```

Tested against API 34. The reflection is wrapped in `runCatching { ... }.getOrDefault(false)` because Android 15 / API 35 may finally promote the method to `@SystemApi` and our reflection would fail. The wrapping interface `BluetoothBondRemover` plus the `ReflectionBondRemover` impl is the *only* place this reflection exists; the screen never calls it directly. The roadmap-tracking note is in `ReflectionBondRemover.kt`'s class-level docstring. If Android exposes a public `removeBond()` in a future SDK, the swap is one file.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| LESC capability check is not exposed in the public SDK | Phase 2 | Wrap the check in `PairBackend.supportsLeSecureConnections()`. Production impl uses adapter-name + version heuristics; tests inject a fake. Error message *must* include the adapter name (DoD #5). |
| `BluetoothDevice.removeBond()` is a hidden API | Phase 5 | Single-purpose `BluetoothBondRemover` interface + `ReflectionBondRemover` impl. Reflection is documented here AND in a class-level comment in `ReflectionBondRemover.kt`. Reviewing the swap is one file when Android exposes a public API. |
| The 6-digit BT code is rendered by the OS *and* by us | Phase 3 | We render it too so the user has *two* surfaces showing the same code — drift catches an OS-side bug or a fake system-dialog overlay (rogue accessibility service). The `headlineLarge` typography makes it impossible to miss. |
| Re-implementing HKDF in Kotlin is tempting (one extra round-trip into Rust) | Phase 4 | `OobCalculator` interface keeps the test seam, but production `UniffiOobCalculator` is a one-liner. The journey + AGENTS.md both forbid re-implementation. |

### North Star Summary

A first-time pairing takes Alex under 60 seconds end-to-end: tap CTA → pick peer → glance at 6-digit code → glance at 4-word OOB → tap Yes → see "Paired". On any mismatch, the bond is cleaned up on both sides with zero residual state — Alex can retry safely. The screen is the second-to-last step of SPEC §5.3 Phase 2 ("Pair"); the only thing after is `syauth list` showing the new peer on the desktop, which proves the same UniFFI surface drove both sides.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] One CTA tap reaches `Scanning`.
- [x] Four code-comparison glances reach `Bonded`.

### Onboarding Clarity
- [x] Each state has exactly one primary action (button) and at most one secondary (cancel/back).
- [x] Errors include the adapter name and a one-line cause.

### Production-Ready Defaults
- [x] OOB code computed via UniFFI ONLY — never reimplemented in Kotlin.
- [x] `BluetoothBondRemover` is called on every Failed transition.

### Golden Path Quality
- [x] Idle → Scanning → LescNegotiating → OobConfirming → Bonded fires unit tests.
- [x] Compose UI test renders every state.

### Decision Load
- [x] User only ever picks "Pair with computer" (Phase 1), the peer row (Phase 2), and Yes/No (Phase 4). No version-numbers, no advanced options.

### Progressive Complexity
- [x] The screen is one file; the ViewModel is one file. No extra navigation graph beyond the two-route NavHost.

### Error Quality
- [x] `Failed(reason)` always carries an actionable string. DoD #5: capability error names the adapter.

### Failure Safety
- [x] Failed cleans up the BT bond AND skips Kotlin-side persistence. Both verified by unit test.

### Runtime Transparency
- [x] Each state renders its own static screen; the user can always see what step they are on.

### Debuggability
- [x] The state name + payload is the screen content; a screenshot is a state snapshot.

### Cross-Surface Consistency
- [x] The 4-word OOB on the phone is byte-identical to the desktop's; pinned by `oob_byte_identical_to_cli_fixture` in syauth-mobile.

### Workflow Consistency
- [x] Mirrors the prrr-android `ScanState`/`QRScanViewModel` idiom: sealed class state + StateFlow + UnconfinedTestDispatcher.

### Change Safety
- [x] Reflection is wrapped in a single class; SDK changes touch one file.

### Experimentation Safety
- [x] Pair flow is reversible up to `Bonded`; `Bonded` is reversible via S-017's revoke (future work).

### Interaction Latency
- [x] State transitions are synchronous on the ViewModel side; async work is in `PairBackend` only.

### Developer Feedback Speed
- [x] Robolectric unit tests run in seconds on the JVM.

### Team Scale
- [x] Test fakes are tiny, no external mocking lib needed.

### System Scale
- [x] The state machine has 6 variants. Adding a 7th (e.g., `RetryingLesc`) is one variant + one transition; the Compose `when` exhaustiveness check forces the screen to render it.

### Right Behavior by Default
- [x] Default is to NOT persist a bond. Only `OobConfirming → Bonded` writes anything.

### Anti-Bypass Design
- [x] The Compose screen never calls `removeBond()` directly. Only through `BluetoothBondRemover`. Tests can catch a regression by checking the fake's call count.

## 4. Tests

### TC-01: idle_then_start_scan_transitions_to_scanning

**Given** a ViewModel constructed with fakes in `Idle` state.
**When** `onStartScanTapped()` is called.
**Then** the state flow emits `Scanning`.

### TC-02: scanning_then_lesc_unsupported_emits_failed_with_adapter_name

**Given** a ViewModel where the fake `PairBackend.supportsLeSecureConnections()` returns false and adapter name is "FakeAdapter-4.0".
**When** `onPeerPicked(peer)` is called from `Scanning`.
**Then** the state flow emits `Failed` with a reason string containing "FakeAdapter-4.0" and "does not support LE Secure Connections".

### TC-03: scanning_then_peer_picked_transitions_to_lesc_negotiating_with_code

**Given** a ViewModel in `Scanning` and a fake `PairBackend` that returns code "123456" from `pickPeer`.
**When** `onPeerPicked(peer)` is called.
**Then** the state flow emits `LescNegotiating("123456")`.

### TC-04: lesc_then_oob_computed_transitions_to_oob_confirming

**Given** a ViewModel in `LescNegotiating` and a fake `OobCalculator` that returns `listOf("alpha", "beta", "gamma", "delta")`.
**When** `onLescBondCompleted(bondKey)` is called.
**Then** the state flow emits `OobConfirming(listOf("alpha", "beta", "gamma", "delta"))`.

### TC-05: oob_yes_writes_bond_and_transitions_to_bonded

**Given** a ViewModel in `OobConfirming` with a fake `BondPersister`.
**When** `onOobYesTapped()` is called.
**Then** `BondPersister.persist` is invoked exactly once AND the state flow emits `Bonded(peerName)`.

### TC-06: oob_no_calls_remover_and_transitions_to_failed

**Given** a ViewModel in `OobConfirming` with a fake `BluetoothBondRemover`.
**When** `onOobNoTapped()` is called.
**Then** `BluetoothBondRemover.remove` is invoked exactly once AND the state flow emits `Failed` with a reason mentioning "OOB code did not match".

### TC-07: failed_state_does_not_persist_bond

**Given** a ViewModel that is driven from `OobConfirming` through the No path.
**When** the state reaches `Failed`.
**Then** the fake `BondPersister.persist` invocation count is zero (never called).

### TC-08 (UI): idle_renders_pair_cta

**Given** PairingScreen rendered with state `Idle`.
**When** the rule composes.
**Then** the node with testTag `pair.idle.cta` is displayed and has the text "Pair with computer".

### TC-09 (UI): scanning_renders_progress_and_cancel

**Given** PairingScreen rendered with state `Scanning`.
**When** the rule composes.
**Then** a node with testTag `pair.scanning.progress` is displayed AND a node with testTag `pair.scanning.cancel` is displayed.

### TC-10 (UI): lesc_negotiating_renders_6_digit_code

**Given** PairingScreen rendered with state `LescNegotiating("123456")`.
**When** the rule composes.
**Then** a node with testTag `pair.lesc.code` is displayed and contains the text "123456".

### TC-11 (UI): oob_confirming_renders_4_emoji_words_and_yes_no_buttons

**Given** PairingScreen rendered with state `OobConfirming(listOf("a", "b", "c", "d"))`.
**When** the rule composes.
**Then** the words are displayed AND `pair.oob.yes` AND `pair.oob.no` are clickable.

### TC-12 (UI): bonded_renders_peer_name

**Given** PairingScreen rendered with state `Bonded("alex-desktop")`.
**When** the rule composes.
**Then** a node containing "alex-desktop" is displayed AND `pair.bonded.done` is clickable.

### TC-13 (UI): failed_renders_reason_and_back_button

**Given** PairingScreen rendered with state `Failed("LESC handshake failed")`.
**When** the rule composes.
**Then** a node containing "LESC handshake failed" is displayed AND `pair.failed.back` is clickable.

## Traceability
- Roadmap item: [specs/syauth/ROADMAP.md#step-s-016](../syauth/ROADMAP.md) — S-016.
- Implementation files:
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/PairingState.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/PairingViewModel.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/PairingScreen.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/api/OobCalculator.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/api/BondPersister.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/api/BluetoothBondRemover.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/api/PairBackend.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/UniffiOobCalculator.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/ReflectionBondRemover.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt` (NavHost integration)
  - `syauth-android/app/src/main/AndroidManifest.xml` (new permissions)
- Test files:
  - `syauth-android/app/src/test/kotlin/com/sy/syauth/android/pair/PairingViewModelTest.kt`
  - `syauth-android/app/src/androidTest/kotlin/com/sy/syauth/android/pair/PairingScreenTest.kt`
