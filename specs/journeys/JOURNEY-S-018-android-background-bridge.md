# JOURNEY-S-018: Android — `CompanionDeviceService` + foreground BLE bridge

<!-- Authored per .agents/skills/journey/SKILL.md template. -->

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md](../syauth/ROADMAP.md) — item **S-018**.
- Feature: the lifecycle wiring that lets the phone receive challenges
  while the app is backgrounded. Registers the bonded computer with
  `CompanionDeviceManager.associate()` at pairing completion; binds a
  `CompanionDeviceService` whenever the system observes the peer in BLE
  range; opens a `BluetoothGattServer` for the duration of that binding;
  raises an `IMPORTANCE_HIGH` notification on every valid challenge
  frame that, when tapped, drives the user into the S-017 Approve
  screen pre-populated with the challenge bytes.

## 1. Journey

When **Alex's phone is in their pocket, screen off, and the bonded
desktop initiates a `sudo` (or any other PAM-gated action)** I want to
**have my phone wake up, raise an unmistakable approve notification,
and let me tap it to land directly on the Approve screen with the
challenge ready to sign** so I can **complete the unlock with one
biometric gesture without ever opening the syauth app from the
launcher, and without the phone having to keep a battery-draining
foreground service running 24/7**.

## 2. CJM

S-018 is the connective tissue between every other Android-side piece
of syauth. S-014 gave us the UniFFI verify/sign surface; S-015 gave us
the Gradle scaffold; S-016 brought the user through the pairing flow
to a `Bonded` state; S-017 rendered the Approve screen and the
Keystore-backed signer. None of that is reachable on a real device
without the OS-managed background lifecycle this item delivers:
without `CompanionDeviceManager.associate()` + a
`CompanionDeviceService`, Android kills the app within minutes of
backgrounding (SPEC §2.3 — "the naive Android BLE app is killed within
minutes of backgrounding"). With it, the OS itself binds the service
when the bonded peer appears in BLE range and elevates the process
priority above normal background apps. SPEC §3.D8 also pins the
direction of advertising: the **desktop** advertises a rotating
session-bound UUID; the **phone** scans and connects. The
`CompanionDeviceService` is therefore the right place to open the GATT
*server* role on the phone — the desktop pushes challenges to us over
GATT writes, and we push responses back through the same characteristic
the Approve screen writes via `GattResponseSender`.

The four non-negotiables for this item:

1. **No long-lived foreground service we have to keep alive ourselves.**
   The `CompanionDeviceService` is system-bound; the OS owns its
   lifecycle. We never call `startForegroundService()` from our own
   code — `connectedDevice` is the foreground sub-type the manifest
   declares, and the OS promotes the service to foreground only while
   it has bound it.
2. **Every challenge frame is verified before raising a notification.**
   The UniFFI `verify_challenge_frame(bond_key, frame_bytes)` call is
   the only thing standing between an attacker writing garbage to our
   GATT characteristic and the user seeing an Approve prompt. A
   `MobileException.VerifyFailed` / `BadFrame` is dropped silently;
   the desktop will time out cleanly.
3. **Notification taps land in `Approve` with the challenge as an
   intent extra.** No state survives the service kill; if the user
   taps the notification 28 seconds after it was raised, the intent
   extras must contain everything the `ApproveViewModel` needs to
   reconstruct the screen. The challenge bytes ride as base64 in the
   intent; the hostname + peerId ride as plain strings.
4. **Battery optimization off, with a documented deep-link.** Doze
   (API 23+) and the stricter API 30 doze tweaks will kill our binding
   even with CDM unless the user has explicitly excluded syauth from
   battery optimization. The home screen pops the
   `Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` intent on
   first launch.

### Phase 1: Pair-time association

**User Intent:** Bond the phone with a desktop so the desktop appears
in CDM's "associated companion devices" list.

**Actions:**
1. Run S-016 pairing happy path to `Bonded`.
2. The `PairingViewModel` calls `companionAssociator.associate(peer)`
   *before* emitting `Bonded(name)`.
3. The OS pops a single approval dialog: "syauth wants to remember
   this companion device". User taps Allow.
4. The `AssociationInfo` is returned to the ViewModel; the bond is
   persisted via `BondPersister`; the state becomes `Bonded(name)`.

**Pain / Risk:**
- The CDM approval dialog is non-obvious — a second consent in a flow
  that already had a numeric-comparison code and an OOB-emoji code.
  Documented in `docs/android-setup.md` so the user expects it.
- Association rejected (user cancels the OS dialog): the ViewModel
  rolls back (`bondRemover.remove(peerId)`) and emits
  `Failed("companion-device association rejected: $reason")`.
- The user pairs once, association succeeds, then the user revokes
  it via system settings later. The OS will simply stop binding the
  service; from the app's perspective the bonded peer becomes
  unreachable. Documented; covered by Phase 5.
- Phone is out of range during pair-complete: rare in practice (the
  user just bonded over BT) but theoretically the CDM dialog could
  appear after the device drops; documented as a retry path.

**Success Signal:** `CompanionDeviceManager.getMyAssociations()`
returns at least one entry whose device id matches the just-bonded
peer.

### Phase 2: Service binding when peer appears

**User Intent:** Be ready to receive challenges without the user
having to open the app.

**Actions:** None from the user. The OS:
1. Observes the bonded peer in BLE range via its native scanner.
2. Binds `SyauthCompanionService` (the manifest-declared
   `CompanionDeviceService` subclass).
3. Calls `onDeviceAppeared(AssociationInfo)`.
4. Our service calls `gattController.start(association, onChallenge)`.

**Pain / Risk:**
- The OS may bind the service when battery optimization is on but
  kill it within seconds. Mitigated by Phase 4 deep-link.
- Multiple bonded peers in range simultaneously: each fires its own
  `onDeviceAppeared`; the service holds a per-association
  `GattServerController` in a concurrent map so they don't trample.
- The OS may bind the service while the app is in the foreground
  (the user just paired). The same `onDeviceAppeared` codepath runs;
  the GATT server is idempotent (`start` checks an
  `AtomicBoolean.compareAndSet(false, true)` per controller).

**Success Signal:** A trace log line
`syauth.bg.service.appeared peer=$id` appears at the moment of binding.

### Phase 3: Challenge receive → notification

**User Intent:** Be told that the desktop is asking to unlock, even
though the phone is locked or in another app.

**Actions:**
1. The desktop writes a frame to the SYAUTH_CHALLENGE_CHAR_UUID GATT
   characteristic.
2. The GATT server invokes the registered `onChallenge(bytes)`
   callback.
3. The service calls
   `verifyChallengeFrame(bondKey, frameBytes)`; on success it has the
   challenge payload.
4. The service builds an `Intent` for `MainActivity` with action
   `Intent.ACTION_VIEW`, plus extras `EXTRA_CHALLENGE_B64`,
   `EXTRA_HOSTNAME`, `EXTRA_PEER_ID`.
5. The service raises a notification on the
   `syauth.approve.channel` channel with `IMPORTANCE_HIGH`. Tap
   action → the intent; full-screen intent → the same intent (for
   the locked-screen heads-up).

**Pain / Risk:**
- Notification on a locked screen with sensitive content visible:
  defaults to "show name only" via the channel-level configuration;
  the hostname is the only sensitive surface and we deem it acceptable
  (the user just chose this as their daily-driver pair).
- Malformed frame (truncated, wrong version, bad MAC): caught by
  `verifyChallengeFrame`, dropped silently. The desktop will time out.
- Multiple challenges from the same peer in quick succession: the
  notification ID is derived from `peerId.hashCode()`, so the second
  notification replaces the first.
- The user taps an old notification after the underlying challenge
  has been timed-out by the desktop: the Approve screen renders, the
  user taps Approve, the signed frame goes nowhere (the GATT
  controller has unbound); the response sender returns
  `Result.failure(ResponseSendError.ServiceUnbound)` and the
  `ApproveViewModel` emits `Denied(SignError("service unbound"))`.
  Acceptable; the desktop already gave up.

**Success Signal:** The user sees a heads-up notification within
~300 ms of the desktop pushing the challenge.

### Phase 4: User taps → Approve screen runs

**User Intent:** See the Approve screen with the challenge populated;
tap Approve; pass biometric; signed response goes back to the
desktop.

**Actions:**
1. User taps the notification (or the full-screen intent fires on a
   locked device).
2. The OS launches `MainActivity` with the `ACTION_VIEW` intent.
3. `MainActivity.onNewIntent` (or `onCreate` if the activity was not
   alive) reads the extras, navigates the NavHost to the `approve`
   route with `launchSingleTop = true`.
4. The Approve route constructs the production `ApproveViewModel`
   with `KeystoreSigner`, `AndroidBiometricPresenter`,
   `UniffiWireSigner`, `InMemorySigningKeyProvider` (until the
   Keystore-Ed25519 follow-up), and a `GattResponseSender` keyed by
   peerId.
5. The screen runs S-017 end-to-end and ships the response via the
   GATT response characteristic.

**Pain / Risk:**
- Activity already alive on `home` route when the notification fires:
  `launchSingleTop = true` plus `intent.action == ACTION_VIEW` switch
  guarantees a single instance hops to the `approve` route.
- Battery optimization disabled the binding mid-flight: the
  `GattResponseSender` looks up the registry; if absent, returns
  `Result.failure(ResponseSendError.ServiceUnbound)`. The ViewModel
  emits `Denied(SignError("service unbound"))`. Documented.
- Activity recreated by config change after the user taps: the
  intent extras are still on `getIntent()`; the route argument
  serialisation in NavHost survives.

**Success Signal:** S-017 ApproveViewModel runs, `responseSender` is
the GATT-backed implementation, the desktop receives a valid response
within the SPEC §4.2 2 s budget.

### Phase 5: Peer disappears → tear down

**User Intent:** Stop using radio when nobody's listening.

**Actions:**
1. The OS observes the peer leaving BLE range (TX timeout) and calls
   `onDeviceDisappeared(AssociationInfo)`.
2. The service calls `gattController.stop()`.
3. The GATT server unregisters the service; the OS unbinds and the
   service is destroyed.

**Pain / Risk:**
- A late challenge writing to the GATT after `stop()`: the
  `BluetoothGattServer.removeService` call is synchronous; subsequent
  writes from the desktop fail at the radio level. No notification
  is ever raised on a stopped controller because the registered
  callback is gone.
- The OS revokes the association (user removed it in system
  settings): same as disappearance — `onDeviceDisappeared` fires;
  the service goes away cleanly. Re-binding requires re-pairing.

**Success Signal:** A trace log line
`syauth.bg.service.disappeared peer=$id` appears; the GATT server
holds no resources.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Two OS prompts during pairing (BT pair + CDM associate) | 1 | Doc note + tighter pair-screen copy that the user expects two clicks |
| Battery-optimization exclusion not enabled => OS kills binding silently | 2/3 | First-launch deep-link to `ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` |
| Notification on locked screen showing hostname | 3 | Default channel-level "private" visibility; user can opt to "show all" |
| User taps stale notification | 4 | `ApproveViewModel` emits `Denied(SignError("service unbound"))` and the desktop has already moved on |
| OEM skin (Xiaomi/OnePlus) ignores CDM contract | 2 | Documented troubleshooting note in `docs/android-setup.md` |

### North Star Summary

Alex's phone is in their pocket, screen off. They run `sudo whoami` on
the desktop. Within 300 ms, their pocket buzzes with a single
heads-up notification: "Approve unlock for `alex-desktop`?". They
pull it out, see the Approve screen already populated, tap Approve,
the fingerprint sensor lights up, they touch it, and the desktop
shell prompt clears. End-to-end under 2 seconds; nothing started by
the user; no foreground service drained the battery; the OS
managed the entire lifecycle.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] Phone wakes < 1 s after the desktop pushes a challenge (OS binds
  the service on TX detection within a few BLE intervals).
- [x] First pair-to-unlock < 5 minutes including the battery-opt
  exclusion prompt and the CDM grant.

### Onboarding Clarity
- [x] `docs/android-setup.md` documents both the CDM association
  prompt and the battery-opt deep-link.
- [x] CDM association rejection produces an actionable
  `Failed("companion-device association rejected: $reason")` string
  in the pairing screen.

### Production-Ready Defaults
- [x] Notification channel `syauth.approve.channel` is created at
  `IMPORTANCE_HIGH` with a stable name; no per-install configuration
  needed.
- [x] `SyauthCompanionService` does no work until the OS binds it;
  zero idle cost.

### Golden Path Quality
- [x] `onDeviceAppeared` → GATT bind → valid frame → notification →
  tap → Approve → response is exercised by `CdmLifecycleTest`.
- [x] The challenge bytes round-trip through the intent without
  truncation (base64 encoding pinned).

### Decision Load
- [x] Three constants name the only knobs (`SYAUTH_GATT_SERVICE_UUID`,
  `APPROVE_NOTIFICATION_CHANNEL_ID`, the foreground service sub-type
  in the manifest). No runtime configuration.

### Progressive Complexity
- [x] The simple case is "bonded peer comes in range, notification
  fires"; the user does not see CDM, GATT, or service-binding
  vocabulary.

### Error Quality
- [x] Every error path emits a typed reason: `ServiceUnbound`,
  `VerifyFailed`, `AssociationRejected`. None contains key material.

### Failure Safety
- [x] Association failure rolls back the BT bond via
  `bondRemover.remove(peerId)`.
- [x] Notification tap on a stale (service-unbound) state degrades
  gracefully into a `Denied(SignError)`.

### Runtime Transparency
- [x] `tracing` spans (Kotlin: `Log.i` on the `syauth.bg` tag) emit
  on every appear/disappear/challenge.

### Debuggability
- [x] `getMyAssociations()` is inspectable via `adb shell dumpsys
  companiondevice`.
- [x] Notification ID derivation is deterministic (peerId hash) so a
  developer can trace one specific peer's notification through `adb
  shell dumpsys notification`.

### Cross-Surface Consistency
- [x] The peer id used by CDM is the same opaque `PeerHandle.id` the
  pairing flow used in S-016.

### Workflow Consistency
- [x] The seam pattern (`CompanionAssociator`, `GattServerController`,
  `ResponseSender`) mirrors the S-016 / S-017 injectable-interface
  style.

### Change Safety
- [x] All new code lives under `bg/` and `pair/{api,impl}/`; no
  existing file's external contract is changed.

### Experimentation Safety
- [x] The fake `GattServerController` in tests is the only way the
  unit suite reaches the GATT API; the production class is a thin
  adapter that the instrumented test can stub.

### Interaction Latency
- [x] No `runBlocking` on the main thread; the service dispatches
  GATT work on `Dispatchers.IO`.

### Developer Feedback Speed
- [x] JVM unit tests (`ApproveNotificationTest`,
  `GattServerControllerTest`, `PairingViewModelCdmAssociationTest`)
  run on `make test` without an emulator.

### Team Scale
- [x] All new constants are named; no magic literals leak.
- [x] Documentation lives in version control alongside the code.

### System Scale
- [x] The architecture scales to N bonded peers without changing the
  service contract: per-peer `GattServerController` and per-peer
  registry entry in `GattResponseSender`.

### Right Behavior by Default
- [x] Notifications default to `IMPORTANCE_HIGH` so the user actually
  sees them; no user configuration required.
- [x] Frame validation is mandatory; no path bypasses
  `verifyChallengeFrame`.

### Anti-Bypass Design
- [x] `verifyChallengeFrame` is called *before* the notification is
  raised; an attacker writing garbage to the GATT cannot produce a
  prompt.
- [x] The notification's tap intent is internal-only (explicit
  `Intent(this, MainActivity::class.java)` + `setPackage`); other
  apps cannot synthesise a fake approve notification.

## 4. Tests

### TC-01: cdm-lifecycle-appeared-starts-gatt

**Given** a `SyauthCompanionService` with a fake `GattServerController`.
**When** `onDeviceAppeared(association)` is invoked with a fabricated
`AssociationInfo`.
**Then** `fakeController.startCalled == true` and the captured
`AssociationInfo` carries the expected device id.

### TC-02: cdm-lifecycle-disappeared-stops-gatt

**Given** the service from TC-01 with `start` already invoked.
**When** `onDeviceDisappeared(association)` is invoked.
**Then** `fakeController.stopCalled == true`.

### TC-03: notification-channel-has-high-importance

**Given** a fresh `Context` (Robolectric).
**When** `showApproveNotification(context, challenge, hostname,
peerId)` is called.
**Then** the `NotificationChannel` named
`APPROVE_NOTIFICATION_CHANNEL_ID` exists with `IMPORTANCE_HIGH` and
the displayed name `APPROVE_NOTIFICATION_CHANNEL_NAME`.

### TC-04: notification-encodes-challenge-bytes

**Given** a 64-byte challenge buffer.
**When** `showApproveNotification(...)` runs.
**Then** the resulting `Notification.contentIntent` extras contain
`EXTRA_CHALLENGE_B64` (base64-decoded matches the original buffer)
plus `EXTRA_HOSTNAME` and `EXTRA_PEER_ID`.

### TC-05: gatt-controller-start-registers-service

**Given** a `BluerlessGattServerController` with an injected fake
`GattServerHandle`.
**When** `start(association, onChallenge)` is called.
**Then** the fake observes one `addService` invocation whose primary
service UUID equals `SYAUTH_GATT_SERVICE_UUID` and characteristics
match `SYAUTH_CHALLENGE_CHAR_UUID` and `SYAUTH_RESPONSE_CHAR_UUID`.

### TC-06: gatt-controller-stop-unregisters-service

**Given** the controller from TC-05 after `start`.
**When** `stop()` is called.
**Then** the fake observes one `close` invocation; the controller's
internal state allows a fresh `start` without throwing.

### TC-07: pairing-view-model-associates-on-bonded-happy-path

**Given** the S-016 happy path with a fake `CompanionAssociator`.
**When** the user taps Yes on the OOB question.
**Then** `associator.callCount == 1` and `state == Bonded(name)`.

### TC-08: pairing-view-model-rolls-back-on-association-failure

**Given** a fake `CompanionAssociator` returning
`Result.failure(AssociationError("rejected"))`.
**When** the user taps Yes on the OOB question.
**Then** state is `Failed` with reason containing
`"companion-device association rejected"`, `bondPersister.persisted`
is empty, `bondRemover.removed == listOf(peerId)`.

### TC-09: pairing-view-model-no-path-does-not-associate

**Given** a fake `CompanionAssociator`.
**When** the user taps No on the OOB question.
**Then** `associator.callCount == 0`.

## Traceability
- Roadmap item: `specs/syauth/ROADMAP.md` § S-018.
- Implementation files:
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthCompanionService.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/GattServer.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/ApproveNotification.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/BatteryOptimizationDeepLink.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/GattResponseSender.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/api/CompanionAssociator.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/RealCompanionAssociator.kt`
  - `syauth-android/app/src/main/AndroidManifest.xml`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
- Test files:
  - `syauth-android/app/src/androidTest/kotlin/com/sy/syauth/android/bg/CdmLifecycleTest.kt`
  - `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/ApproveNotificationTest.kt`
  - `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/GattServerControllerTest.kt`
  - `syauth-android/app/src/test/kotlin/com/sy/syauth/android/pair/PairingViewModelCdmAssociationTest.kt`
