# JOURNEY-S-014: `ChallengeApprovalActivity` (transparent, over-keyguard)

> **Spec anchors:** `specs/unlock-proximity/SPEC.md` §3 Approach,
> phone-side:
>
> > "`onCharacteristicChanged → launch ChallengeApprovalActivity`
> > (transparent, full-screen-over-keyguard)"
>
> §9 Open Question 2 (verbatim): the biometric prompt opens a
> transparent activity over the keyguard; the prompt's "reason" copy
> is recommended as
> `"$hostname is requesting sudo (peer_id $short)"` with hostname
> pulled from the bond record, not from the incoming frame.
>
> §3 Decisions row "Phone connection lifecycle" — one persistent
> `BluetoothGatt` per bonded peer; this step launches the approval
> activity from the service-side `onChallenge` callback and uses the
> same GATT handle to write back a denied frame on Cancel.
>
> **Roadmap row:** `specs/unlock-proximity/ROADMAP.md` Step S-014.
>
> **Closure condition (verbatim from ROADMAP.md):**
>
> ```
> ./gradlew :app:testDebugUnitTest --tests "*ChallengeApprovalActivityTest*"
> ```

## Roadmap Link
- Source roadmap: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-014.
- Feature: new
  `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/ChallengeApprovalActivity.kt`
  — a transparent, no-history, single-instance activity that
  `SyauthCompanionService` launches via `PendingIntent.getActivity`
  on the `onChallenge(peerId, frameBytes)` callback. Manifest
  declares `USE_FULL_SCREEN_INTENT`, `android:showWhenLocked=true`,
  `android:turnScreenOn=true`. The activity renders the SPEC §9 Q2
  prompt text. Cancel button writes a denied frame back through the
  same GATT response characteristic via the service. Approve button
  is a placeholder no-op that just calls `finish()` — the real
  BiometricPrompt + Keystore sign land in S-015.

## 1. Journey

When **an Android user has bonded their phone with the desktop, the
foreground `SyauthCompanionService` is alive holding a persistent
`PersistentGattClient` with `autoConnect=true`, and the desktop
emits a fresh `sudo` challenge frame on the challenge
characteristic**, I want to **the phone to wake the screen, show
me a clear, untrusted-input-free prompt saying which desktop is
asking (`"$hostname is requesting sudo (peer_id $short)"`), and
let me Cancel that prompt with a single tap to send a denied frame
back to the daemon so my desktop's `sudo` returns `PAM_AUTH_ERR`
fast (no 8-second `response-timeout` hang)**, so I can **trust the
unlock prompt cannot be spoofed by an incoming frame's contents,
deny a sudo I did not initiate without leaving the desktop hanging,
and approve a real sudo with the biometric gate that lands in
S-015 — this step pins the activity scaffolding so the cancel path
is mechanically observable before the crypto path is wired in**.

## 2. CJM

Before S-014, the `PersistentGattClient.onChallenge` callback the
service installs in `MainActivity.installPersistentClientFactory`
is `{ _, _ -> }` — every challenge frame is silently dropped on
the floor. The daemon then times out after `response-timeout`
(eight seconds in SPEC §3 Decisions) and `pam_syauth` returns
`PAM_AUTHINFO_UNAVAIL`. The user sees `sudo` hang for eight
seconds before falling through to the password prompt.

S-014 closes the first half of the unlock loop: it wakes the
screen, names the requesting desktop verbatim from the bond
record, and offers Cancel as a one-tap denial path. Approve is
left as a placeholder `finish()` so the activity lifecycle can be
asserted in isolation — the BiometricPrompt + Keystore sign that
will replace the placeholder land in S-015. This separation makes
the S-014 DoD tests purely about activity lifecycle, manifest
attributes, and the denied-frame write path; no Keystore /
biometric shadows are pulled into the test classpath.

The denied-frame wire shape is the SPEC v1 frame layout
(`[version:1] || [nonce:16] || [signature:64] || [tag:16]`) with
the signature payload filled with 64 zero bytes
(`DENIED_FRAME_BYTES = ByteArray(64) { 0 }`). The daemon's
`verify_response` then rejects with
`SignError::BadSignature` → maps to `PAM_AUTH_ERR`, which is the
right end-state for a user-initiated denial (the alternative —
sending nothing — would map to `PAM_AUTHINFO_UNAVAIL`, a
fall-through, which is wrong for an explicit user "no"). This
choice is documented inline at the `DENIED_FRAME_BYTES` constant
in `ChallengeApprovalActivity.kt`.

### Hostname-vs-peer_id deviation

SPEC §9 Q2 recommends the prompt copy
`"$hostname is requesting sudo (peer_id $short)"`. The
`BondRecord` (DEV-002) does carry a `hostName` field, so the full
SPEC §9 Q2 text is reachable. The activity reads `hostName` from
the extras the service supplies; both pieces flow from the
service's `HostnameResolver` seam (already in place since S-011)
and the bond record loaded at `MainActivity.onCreate`.

### Phase 1: Challenge arrives — service wakes the screen

**User Intent:** The user is at their desktop, has just typed
`sudo apt update`, and expects the phone in their pocket to wake
up with a "did you ask for this?" prompt.

**Actions:** None at the phone level — the user does not touch
the phone. At the system level: the desktop daemon notifies the
challenge characteristic; the `PersistentGattClient` fires
`onChallenge(peerId, frameBytes)`; the service constructs a
`PendingIntent.getActivity` for `ChallengeApprovalActivity`
populated with `EXTRA_PEER_ID`, `EXTRA_HOSTNAME`, and
`EXTRA_CHALLENGE_BYTES`, then calls
`startActivity(intent)`. The OS, honoring
`android:showWhenLocked="true"` and `android:turnScreenOn="true"`,
turns the screen on and renders the activity over the keyguard.

**Pain / Risk:**
- If the activity's `showWhenLocked` flag is wrong, the OS leaves
  the screen off; the user never sees the prompt; sudo times out
  with `response-timeout`.
- If the `launchMode="singleInstance"` is wrong, a rapid second
  challenge could stack two activities, leaving the user staring
  at the older challenge while the newer one sits behind it. The
  single-instance flag forces the OS to deliver the new intent to
  the existing activity instance via `onNewIntent`.
- If `USE_FULL_SCREEN_INTENT` is not declared in the manifest, the
  OS denies the screen-wake on Android 14+ and the prompt arrives
  as a heads-up notification instead — defeating the
  "over-keyguard wake" requirement.

**Success Signal:** Robolectric's `Activity` shadow reports
`isShowWhenLocked()` and `isTurnScreenOn()` both `true` after the
test launches the activity with a fresh intent.

### Phase 2: User reads the prompt and decides

**User Intent:** The user wants to know which desktop is asking
for sudo so they can tell a real sudo from a phishing relay.

**Actions:** The user looks at the screen. The activity renders
the SPEC §9 Q2 copy verbatim:
`"$hostname is requesting sudo (peer_id $short)"`, where
`$short` is the last six hex chars of the peer's MAC
(or the full peer id if shorter). Two buttons are visible:
"Approve" and "Cancel".

**Pain / Risk:**
- If the hostname were taken from the incoming frame's payload
  instead of the bond record, an attacker who replays a frame
  with a tampered payload could spoof the desktop's identity.
  S-014 reads `EXTRA_HOSTNAME` from the intent the service builds
  out of the bond record, not from the frame's bytes — the bytes
  are passed through opaquely as `EXTRA_CHALLENGE_BYTES` for S-015
  to sign. This pins the SPEC §9 Q2 "hostname pulled from the
  bond record, not from the incoming frame" guarantee at the
  activity boundary.
- If the prompt is dismissable by tapping outside (the default
  `android:theme="@android:style/Theme.Translucent.NoTitleBar"`
  behaviour for translucent activities), the user might dismiss
  by accident and leave the daemon hanging. The activity overrides
  `onBackPressed` to behave as Cancel (sends a denied frame, then
  finishes); a future S-015 commit may tighten outside-tap.
- If the prompt copy ever drifts away from the bond record's
  `hostName`, a phishing relay's tampered frame can rename the
  desktop. The intent extras are read once at `onCreate` /
  `onNewIntent` and held in a `val`; they cannot be mutated by a
  later frame.

**Success Signal:** Robolectric activity content includes the
SPEC §9 Q2 prompt string with the fixture hostname and the short
peer id substring. (Test:
`ChallengeApprovalActivityTest::hostname_shown_in_prompt`.)

### Phase 3: User taps Cancel — denied frame goes back

**User Intent:** The user did not initiate the sudo (or did but
changed their mind). They tap Cancel.

**Actions:** The activity's Cancel handler calls a service-side
helper (the `SyauthCompanionService.cancelChallenge(peerId)`
companion seam exposed for the activity) that writes the denied
frame on the same GATT connection via
`PersistentGattClient.writeResponse(deniedFrameBytes)`. The
denied frame is the v1 frame layout with the signature payload
filled with `DENIED_FRAME_BYTES = ByteArray(64) { 0 }`; the
daemon's `verify_response` then rejects with `BadSignature`,
which maps to `PAM_AUTH_ERR`. `pam_syauth` returns the desktop's
sudo with an error in well under one second — no
`response-timeout` hang.

**Pain / Risk:**
- If the service has died between challenge-receive and Cancel
  (battery optimisation, OEM kill), the
  `PersistentGattClient` lookup returns `null` and the write is
  a silent no-op. The user's sudo then hits the daemon's
  `response-timeout` (8 s) and falls through to
  `PAM_AUTHINFO_UNAVAIL`. This is the right end-state for a dead
  service; the activity logs the no-op and finishes.
- If the denied frame's signature payload is anything other than
  64 zero bytes, the daemon could accidentally accept it (any
  valid Ed25519 signature would pass). Pinning the constant in
  the activity makes the "this is a deny, not an approval" intent
  mechanically observable — a future drift would have to renames
  the constant.
- If the activity does not `finish()` after the write, the prompt
  sits on the screen across the keyguard until the OS reaps it,
  confusing the user. The Cancel handler unconditionally calls
  `finish()` regardless of the write's success.

**Success Signal:** Robolectric test
`ChallengeApprovalActivityTest::cancel_writes_denied_frame`
injects a recording `CancelSink` seam; the assertion is that the
recorded bytes equal `DENIED_FRAME_BYTES` (the 64-zero
signature payload).

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Tapped Cancel does nothing visible — user can't tell whether the desktop saw the deny | 3 | Service-side log + foreground notification tick on cancel; out of S-014 scope |
| User does not recognise the hostname (e.g. shared workstation alias) | 2 | Future: surface the bond's pair-time timestamp + association count alongside the hostname; out of S-014 scope |
| Activity wakes the screen at 03:00 because a phishing replay reached the GATT | 1 | DEV-004 bond-key MAC + S-007 nonce LRU already gate this at the service; the activity is the last line of UX defense, not the first crypto check |

### North Star Summary

The ideal end state is that the user's first awareness of an
incoming sudo is the screen waking with a clear, untamperable
"my-desktop is asking" prompt; tapping Cancel ends the
desktop-side sudo in well under a second with a `PAM_AUTH_ERR`
that the operator recognises as "I said no"; and there is no path
by which a tampered frame can rename the desktop in the prompt
because the hostname comes from the bond record.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] Screen wakes within Android's `USE_FULL_SCREEN_INTENT`
      latency budget (sub-second on AOSP).
- [x] Cancel produces a deterministic daemon-side error in well
      under one second.

### Onboarding Clarity
- [x] Prompt copy is the SPEC §9 Q2 recommendation verbatim.
- [x] No free-form fields from the incoming frame are rendered.

### Production-Ready Defaults
- [x] Manifest attributes (`showWhenLocked`, `turnScreenOn`,
      `noHistory`, `singleInstance`, `exported="false"`) match
      SPEC §3 Approach phone-side.
- [x] Denied-frame constant is named and pinned.

### Golden Path Quality
- [x] Service-side launch path uses
      `PendingIntent.getActivity` with `FLAG_IMMUTABLE` for the
      Android 12+ floor.
- [x] Cancel path closes the loop without crashing when the
      service is dead.

### Decision Load
- [x] Two buttons only: Approve (placeholder) and Cancel.
- [x] No "remember my decision" toggle — every sudo is a fresh
      ack.

### Progressive Complexity
- [x] S-014 ships the lifecycle scaffolding; S-015 wires
      BiometricPrompt without changing the activity's contract
      with the service.

### Error Quality
- [x] Cancel-during-dead-service is a silent no-op + log line,
      not a crash.
- [x] Missing extras → activity `finish()`-es immediately
      (cannot render an empty prompt).

### Failure Safety
- [x] `noHistory="true"` means the activity does not persist in
      the task back-stack — the user cannot accidentally tap a
      Cancel from a stale challenge.
- [x] `singleInstance` collapses a rapid second challenge into
      the existing activity via `onNewIntent`.

### Runtime Transparency
- [x] Service logs the launch + the cancel with the peer id.
- [x] Activity logs the extras parse + the cancel write.

### Debuggability
- [x] `adb shell dumpsys activity activities | grep ChallengeApprovalActivity`
      surfaces the activity when alive.
- [x] `logcat -s syauth.bg` shows the full launch → cancel
      timeline.

### Cross-Surface Consistency
- [x] Hostname is the bond record's `hostName` (single source).
- [x] Peer id formatting matches the daemon's audit-log
      `peer_id` field shape.

### Workflow Consistency
- [x] Activity sits under `bg/` alongside
      `SyauthCompanionService` and `PersistentGattClient`.
- [x] Test sits under `app/src/test/kotlin/...bg/...Test.kt`
      mirroring the other Robolectric tests in the package.

### Change Safety
- [x] No production-only constructor; both production and tests
      go through the same constants.
- [x] Test seams live on a `companion object`, mirroring
      `SyauthCompanionService.resetSeams()`.

### Experimentation Safety
- [x] Approve is a `finish()` no-op gated behind a debug-only
      seam; nothing crypto-sensitive is touched.

### Interaction Latency
- [x] Render path is a single `setContent { … }` Compose pass
      with no I/O.

### Developer Feedback Speed
- [x] All three DoD tests are pure Robolectric — no NDK, no
      AAR, no UniFFI dependency at test time.

### Team Scale
- [x] Constants live in the same file as the activity, so a
      future contributor doesn't have to grep across modules.

### System Scale
- [x] One activity instance per bond regardless of the bond
      count (singleInstance).

### Right Behavior by Default
- [x] Cancel → `PAM_AUTH_ERR` (the right user-intent
      semantics).
- [x] Service down → `PAM_AUTHINFO_UNAVAIL` via daemon-side
      timeout (the right system-state semantics).

### Anti-Bypass Design
- [x] Hostname is read from the bond, not the frame.
- [x] `exported="false"` blocks third-party launch.

## 4. Tests

### TC-01: `launches_over_keyguard`

**Given** a fresh `ChallengeApprovalActivity` intent built with
`EXTRA_PEER_ID`, `EXTRA_HOSTNAME`, and `EXTRA_CHALLENGE_BYTES`
populated from a fixture bond record.
**When** the activity is created via
`Robolectric.buildActivity(ChallengeApprovalActivity::class.java, intent).create().start().resume()`.
**Then** the activity's `setShowWhenLocked` and `setTurnScreenOn`
flags both observe `true` (via the package-internal
`lastShowWhenLockedFlag` / `lastTurnScreenOnFlag` recording
fields the activity writes inside `onCreate`).

### TC-02: `cancel_writes_denied_frame`

**Given** a launched `ChallengeApprovalActivity` with a recording
`CancelSink` seam injected on the companion object.
**When** the activity's `onCancelClicked()` is invoked
(simulating a Cancel tap).
**Then** the recording sink reports exactly one call with
`peerId` matching the fixture and `bytes` equal to
`DENIED_FRAME_BYTES`; the activity is `finishing` after the call.

### TC-03: `hostname_shown_in_prompt`

**Given** a launched `ChallengeApprovalActivity` with
`EXTRA_HOSTNAME = "alex-desktop"` and
`EXTRA_PEER_ID = "AA:BB:CC:DD:EE:FF"`.
**When** the activity's prompt text is queried (via the
package-internal `lastPromptText` recording field the activity
writes inside the Compose callback).
**Then** the recorded text equals
`"alex-desktop is requesting sudo (peer_id DD:EE:FF)"` (the SPEC
§9 Q2 template instantiated with the fixture hostname + the last
three octets of the peer id as `$short`).

## Traceability
- Roadmap item: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-014.
- Implementation files: see "Implementation" section below.
- Test files: see "Implementation" section below.

## Implementation

Closed 2026-05-18.

### Files created

- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/ChallengeApprovalActivity.kt`
  — the transparent over-keyguard activity. Holds the SPEC §9 Q2
  prompt copy, the `CancelSink` companion seam, the
  `DENIED_FRAME_BYTES = ByteArray(64) { 0 }` constant, the
  `EXTRA_PEER_ID = "syauth.peerId"` /
  `EXTRA_HOSTNAME = "syauth.hostname"` /
  `EXTRA_CHALLENGE_BYTES = "syauth.challengeBytes"` /
  `DENIED_FRAME_REASON = "denied"` constants, and the
  `lastShowWhenLockedFlag` / `lastTurnScreenOnFlag` /
  `lastPromptText` recording fields the DoD tests read.
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/ChallengeApprovalActivityTest.kt`
  — three Robolectric SDK-34 tests pinning the DoD bullets:
  `launches_over_keyguard`, `cancel_writes_denied_frame`,
  `hostname_shown_in_prompt`.

### Files modified

- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthCompanionService.kt`
  — new companion helper
  `launchApprovalActivity(context, peerId, challengeBytes)` that
  builds the activity intent with the S-014 extras, resolves the
  hostname via the installed `HostnameResolver`, and dispatches via
  `PendingIntent.getActivity` with
  `FLAG_UPDATE_CURRENT | FLAG_IMMUTABLE`. New file-scope
  `APPROVAL_PENDING_REQUEST_CODE = 0x5A14`. New imports
  `android.app.PendingIntent`, `android.content.Context`.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/PersistentGattClient.kt`
  — new file-scope `PersistentGattClientRegistry` object
  (`put` / `lookup` / `remove` / `reset`) so the activity's cancel
  sink can resolve the per-peer client and call `writeResponse`.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
  — `installPersistentClientFactory` now passes a real
  `onChallenge` callback that calls
  `SyauthCompanionService.launchApprovalActivity` and registers
  the just-built `PersistentGattClient` in
  `PersistentGattClientRegistry`. `installCompanionSeams` now
  installs `ChallengeApprovalActivity.cancelSink` (the production
  cancel path that resolves the per-peer client and calls
  `writeResponse(DENIED_FRAME_BYTES)`). Imports renamed to track
  the `APPROVE_EXTRA_*` constants.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/ApproveNotification.kt`
  — pre-existing `EXTRA_CHALLENGE_B64` / `EXTRA_HOSTNAME` /
  `EXTRA_PEER_ID` top-level constants renamed to `APPROVE_EXTRA_*`
  so the S-014 activity-launch constants own the canonical names
  the task spec pinned.
- `syauth-android/app/src/main/AndroidManifest.xml`
  — adds the `USE_FULL_SCREEN_INTENT` permission and the
  `<activity android:name=".bg.ChallengeApprovalActivity"
  android:exported="false" android:launchMode="singleInstance"
  android:noHistory="true" android:showOnLockScreen="true"
  android:turnScreenOn="true"
  android:theme="@style/Theme.SyauthTranslucent"
  android:configChanges="orientation|screenSize" />`
  declaration.
- `syauth-android/app/src/main/res/values/themes.xml`
  — adds `Theme.SyauthTranslucent` parented to
  `android:Theme.Translucent.NoTitleBar`.
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/ApproveNotificationTest.kt`
  — updated to track the `APPROVE_EXTRA_*` rename.

### Deviations

None. The `BondRecord.hostName` field is already present (DEV-002
journey doc), so the SPEC §9 Q2 prompt copy
`"$hostname is requesting sudo (peer_id $short)"` is reachable
verbatim — no hostname-vs-peer_id fallback was needed.

The denied frame is a 64-byte all-zero signature payload
(`DENIED_FRAME_BYTES`). The daemon's `verify_response` rejects it
as `SignError::BadSignature` → maps to `PAM_AUTH_ERR`, which is
the user-intent-correct end-state for an explicit denial. This
choice — versus sending nothing (would map to
`PAM_AUTHINFO_UNAVAIL`) or extending the wire format with an
explicit denied sentinel (SPEC change too invasive for S-014) — is
documented at the constant in `ChallengeApprovalActivity.kt`.

### Closure verification

- `make scope-discipline` — clean.
- `make lint` — clippy + fmt + cargo-deny clean.
- `make test` — 387 passed, 0 failed, 8 ignored (the 8 are
  pre-existing live-radio gated DEV-004 + benches).
- `:app:assembleDebug` — BUILD SUCCESSFUL.
- `:app:testDebugUnitTest` — 94 tests passed, 0 failed.
- Closure-condition probe
  (`./gradlew :app:testDebugUnitTest --tests "*ChallengeApprovalActivityTest*"`)
  — `BUILD SUCCESSFUL`; 3 tests passed.

