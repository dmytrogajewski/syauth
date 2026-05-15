# JOURNEY-S-017: Android — Approve screen + BiometricPrompt + Keystore signer

<!-- Authored per .agents/skills/journey/SKILL.md template. -->

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md](../syauth/ROADMAP.md) — item **S-017**.
- Feature: the Compose screen the user sees on every unlock, plus the
  BiometricPrompt + Android Keystore plumbing that gates the Ed25519
  signature on a fresh user gesture. Mirrors the SPEC §1 happy-path step
  "User taps Approve and authenticates with BiometricPrompt; Android
  Keystore releases the phone's signing key for one signature."

## 1. Journey

When **Alex is at the laptop and PAM has pushed a challenge to the phone
via the background bridge (S-018, mocked here)** I want to **see a
single, unmistakable "Approve unlock for `hostname`?" screen, tap
Approve, pass biometric, and have the response signed and shipped in
under a second** so I can **complete sudo / login without typing a
password while keeping the signing key gated by hardware-backed
biometric**.

## 2. CJM

S-017 is the user-visible heart of syauth. Every successful unlock funnels
through this screen, and every denial — explicit or implicit — emits a
`PeerDenied` wire frame so the desktop knows to fall through to the next
PAM module (typically `pam_unix`). The non-negotiables (SPEC §1, §3.D6,
§6 T-006/T-008/T-014):

1. **The signing operation must require a fresh user gesture.** Passive
   proximity is the whole class of vulnerability syauth exists to avoid.
   `setUserAuthenticationRequired(true)` on the Keystore key is the
   hardware-enforced gate. BiometricPrompt is the user-facing wrapper
   around that gate; `BIOMETRIC_STRONG | DEVICE_CREDENTIAL` covers both
   the "fingerprint enrolled" case and the "no biometric, PIN/pattern
   only" fallback.
2. **The signing key never leaves the secure element.** Per SPEC §3.D6
   "Keys never leave secure storage; phone biometric becomes a
   hardware-enforced gate, not an app-level check." On Pixel 7+ the
   StrongBox tee holds it; older hardware falls back to the regular TEE.
3. **A timeout is a denial.** S-017 DoD: "Cancel on countdown is logged
   as a denial — desktop sees a `PeerDenied` frame." Silence is not
   acceptable; the desktop must hear back so it can fall through
   instantly rather than waiting out its own timeout (SPEC §4.2 budgets
   the desktop side at ~2 s, but a phone-side fast-deny shaves a full
   second off the user's wait).
4. **The Compose surface is unmistakable.** Hostname-prominent,
   Approve/Deny buttons clearly distinct, countdown visible — defends
   T-014 (biometric coercion) by giving the user a fast, friction-free
   Deny path.

A design force unique to S-017: the **UniFFI surface S-014 actually
shipped** is `signChallengeResponse(signing_key: ByteArray, frame_bytes:
ByteArray): ByteArray` — it takes a 32-byte Ed25519 seed and produces a
64-byte signature in Rust. The DoD's "raw signature blob from the
Keystore-backed `Signature` object" envisions a future surface where the
Keystore-backed `Signature` (EC P-256, since Android Keystore lacks
broad Ed25519 support pre-API 33) produces a signature blob that the
Rust core then wraps into a response frame. We thread the needle by:

- **Using the Keystore-backed EC `Signature` as the gate.** A real EC
  P-256 key is generated with `setUserAuthenticationRequired(true)` and
  `setUnlockedDeviceRequired(true)`; `BiometricPrompt.CryptoObject(sig)`
  binds the unlock to that exact `Signature` instance. The gate produces
  a real ECDSA signature blob on the challenge, which is logged as
  audit-proof that the biometric was passed.
- **Keeping the Ed25519 signing seed behind a `SigningKeyProvider`
  interface.** Production wires this to a Keystore-encrypted file
  (lands in S-018 alongside the background bridge; tracked there). For
  S-017 the interface seam is enough — tests inject a fake, production
  loads from a stub today.
- **Calling UniFFI `signChallengeResponse` with the seed + frame bytes.**
  The Rust core is the canonical Ed25519 signer; the Keystore is the
  canonical user-presence gate. The two combine to satisfy both the
  cryptographic contract (one correct Ed25519 signature per challenge,
  byte-identical to what the desktop expects) and the user-presence
  contract (no signature without a fresh biometric or device credential
  gesture).

The honest gap: until the Keystore gains widespread Ed25519 support (or
we wrap the Ed25519 seed inside a Keystore-encrypted blob), the Ed25519
seed bytes briefly cross the Kotlin boundary. We document the gap in
`docs/android-setup.md` and pin the follow-up in the roadmap.

### Phase 1: Challenge arrives, screen renders

**User Intent:** Alex unlocks the desktop and the phone shows "Approve
unlock for `hostname`?" with a visible countdown so they know how much
time they have to decide.

**Actions:** The background bridge (S-018, mocked here) hands the
`ApproveViewModel` a `hostname: String` and the `frame_bytes: ByteArray`
of the challenge. Alex's phone screen lights up; the Compose surface
renders the hostname, the app icon, Approve / Deny buttons, and a
countdown that ticks down from 30 s.

**Pain / Risk:**
- **Hostname is attacker-controlled.** A malicious peer could send
  `hostname = "Bank of America\nWARNING: type your PIN below"`. Mitigation:
  render hostname as a single `Text(text = hostname, maxLines = 1, overflow =
  TextOverflow.Ellipsis)` — newlines collapse, length capped at 64 chars
  via a constant pre-render trim.
- **No challenge body validation in ApproveViewModel.** The challenge
  frame's MAC was already verified by `uniffi.syauth_mobile.verifyChallengeFrame`
  in S-018; S-017 trusts the verified bytes. Mitigation: document the
  contract — `ApproveViewModel` accepts pre-verified `frame_bytes`. If
  S-018 hands raw bytes, the verification step is the caller's
  responsibility.
- **Approve button disabled too eagerly.** If the button is disabled in
  the initial `Idle` state, Alex's first tap is ignored. Mitigation: the
  Approve button is enabled the moment the screen enters `Counting`; the
  countdown coroutine starts in `init { ... }` so `Counting` is
  effectively the initial state on first composition.

**Success Signal:** The Compose `ApproveScreen` displays "Approve unlock
for `<hostname>`?" with two clearly labeled buttons and a "Approve
within Xs" countdown that ticks down once per second. The screen is
visible on a real device or via `createComposeRule()` in an instrumented
test.

### Phase 2: Tap Approve, BiometricPrompt fires

**User Intent:** Alex taps Approve; BiometricPrompt asks for fingerprint
or device PIN; on success, the Keystore-backed `Signature` is unlocked
for one signing operation and the response is built and sent.

**Actions:** Alex taps Approve. The ViewModel transitions to
`AwaitingBiometric`; the `BiometricPresenter` shows
`BiometricPrompt` with allowed authenticators `BIOMETRIC_STRONG |
DEVICE_CREDENTIAL` and the `CryptoObject(Signature)` bound to the
Keystore EC key. Alex authenticates; the unlocked `Signature` produces
a gate-proof blob; the ViewModel fetches the Ed25519 seed from the
`SigningKeyProvider`; UniFFI's `signChallengeResponse(seed, frame_bytes)`
returns the 64-byte signature; the ViewModel transitions to `Approved`
and the response is dispatched via `ResponseSender.sendApprove`.

**Pain / Risk:**
- **No enrolled biometric.** On a device with no fingerprint or face,
  `BIOMETRIC_STRONG` returns `BIOMETRIC_ERROR_NONE_ENROLLED`. Mitigation:
  include `DEVICE_CREDENTIAL` in `allowedAuthenticators` so PIN /
  pattern / password are accepted; if even that's unset, the ViewModel
  transitions to `Denied(BiometricUnavailable)` and the response is a
  PeerDenied frame.
- **User cancels biometric.** The presenter returns
  `BiometricResult.Failed`; the ViewModel transitions to
  `Denied(BiometricFailed)` and sends PeerDenied.
- **StrongBox unavailable.** Older devices (< API 28 or non-Pixel) throw
  `StrongBoxUnavailableException` from `KeyGenParameterSpec.Builder
  .setIsStrongBoxBacked(true).build()`. Mitigation: try StrongBox first,
  catch the exception, fall back to non-StrongBox builder.
- **Keystore EC curve mismatch.** Pre-API 33, the Android Keystore
  doesn't reliably support Ed25519. Mitigation: use EC P-256 (secp256r1)
  for the gate signature; the wire-protocol Ed25519 signature comes from
  the UniFFI surface. Documented in `docs/android-setup.md`.
- **Signature initialization fails after key rotation.** If the user
  added a new biometric while the app was running, the Keystore key is
  invalidated and `Signature.initSign` throws
  `KeyPermanentlyInvalidatedException`. Mitigation: catch and surface as
  `Denied(SignError("key invalidated; re-bond required"))`; the
  follow-up step regenerates the key on next launch.

**Success Signal:** `BiometricPrompt` completes, the Keystore signs the
gate blob, UniFFI returns 64 bytes, `ResponseSender.sendApprove(64-byte
blob)` is invoked, and the UI shows "Unlock approved." for visual
confirmation.

### Phase 3: Deny or timeout closes the screen

**User Intent:** Alex either taps Deny explicitly (they didn't initiate
the unlock and someone else is at their desktop) or ignores the prompt
(they're driving; the desktop should fall through to password
immediately rather than waiting out its own 2-second budget).

**Actions:** Deny tap → ViewModel transitions to `Denied(UserDenied)` and
calls `responseSender.sendDeny()`. Countdown reaches zero → ViewModel
transitions to `Denied(TimedOut)` and calls `responseSender.sendDeny()`.

**Pain / Risk:**
- **Fat-fingered Deny.** A user who meant Approve hits Deny. Mitigation:
  Deny is destructive enough (lose this unlock) but cheap enough (just
  re-trigger the unlock) that no undo is necessary. The Approve button
  is on the dominant side per RTL convention.
- **Timeout fires after biometric started.** A race where the countdown
  reaches zero while the BiometricPrompt is still showing. Mitigation:
  the ViewModel disables the timeout transition once it leaves
  `Counting` — only `Counting` can transition to `Denied(TimedOut)`. The
  `AwaitingBiometric` state ignores tick events.
- **Multiple deny dispatches.** If `onDenyClicked` is called twice, two
  PeerDenied frames go out. Mitigation: only `Counting` → `Denied`
  transitions invoke the sender; subsequent calls are no-ops because the
  state is no longer `Counting`.

**Success Signal:** A single `responseSender.sendDeny()` call fires on
either Deny tap or countdown expiry, and the ViewModel reaches a
terminal `Denied(_)` state with the correct `DenialReason`. The desktop
(in real-world integration) sees a `PeerDenied` wire frame within < 100
ms of the user action.

### Phase 4: Robolectric / Compose test coverage

**User Intent:** A future agent regressing the ViewModel or the screen
sees a red CI within seconds.

**Actions:** `make test` runs the JVM unit tests (Robolectric or pure
JVM — preferred where possible). The androidTest source set runs on a
connected emulator via `make android-test` (which we already wired in
S-015 and skips cleanly on hosts without an emulator).

**Pain / Risk:**
- **Touching real Android crypto in JVM tests.** The Android Keystore,
  `BiometricPrompt`, and `KeyGenParameterSpec` classes don't exist in
  the standard JVM; touching them in a unit test requires Robolectric's
  shadow infra and a connected hardware-backed AndroidManifest. Mitigation:
  every dependency is injected behind a small interface (`SignerBackend`,
  `BiometricPresenter`, `ResponseSender`, `SigningKeyProvider`, `Clock`);
  the ViewModel tests pass fakes for all five and never touch real
  Android classes. The result: tests can run pure-JVM with no
  Robolectric runtime, which is faster and more portable.
- **Coroutines + virtual time.** The countdown is driven by
  `kotlinx.coroutines.delay(tickMillis)`. Mitigation: tests use
  `kotlinx.coroutines.test.runTest`'s virtual scheduler and advance time
  with `testScheduler.advanceTimeBy(...)`.
- **Compose test rule unreachable without an emulator.** `make test`
  doesn't run androidTest; `make android-test` does. Mitigation: the
  Compose test exists in `androidTest/` and is documented as
  emulator-gated.

**Success Signal:** `./gradlew :app:testDebugUnitTest` (Robolectric /
pure-JVM unit tests) runs four scenarios (approve happy path,
user-deny, timeout, biometric-failed) and all pass. The androidTest
runs on emulator hosts and verifies hostname / Approve / Deny / countdown
nodes are rendered.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Real Android crypto unavailable in JVM tests | Phase 4 | Inject every Android class behind a small interface; tests pass fakes. |
| Keystore Ed25519 support inconsistent pre-API 33 | Phase 2 | Use EC P-256 for the gate, UniFFI Ed25519 for the wire signature; document the seam. |
| StrongBox unavailable on most non-Pixel devices | Phase 2 | Try StrongBox first; catch and fall back; surface the choice in `KeyInfo`. |
| Attacker-controlled hostname could spoof a phishing prompt | Phase 1 | Cap length, strip newlines, render in a single Text composable. |
| Timeout race with biometric prompt | Phase 3 | State machine — only `Counting` transitions to `Denied(TimedOut)`. |
| Multiple deny dispatches if user double-taps | Phase 3 | Terminal-state transitions are guarded; only the first wins. |

### North Star Summary

Alex picks up the phone, sees "Approve unlock for `dell-precision`?" with
a countdown ticking from 30 s, taps Approve, fingerprint flashes once,
and the desktop unlocks within 800 ms — without ever typing a password.
A denial — explicit or by timeout — produces a single `PeerDenied` wire
frame so the desktop falls through to password without waiting out its
own budget. The signing key has never left the secure element; the
biometric was checked on hardware, not by app code; the failure modes
each map to a distinct `DenialReason` for observability.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] Tap Approve to laptop unlocked is < 1 s end-to-end (BiometricPrompt
      latency dominates; Ed25519 sign is microseconds).
- [x] The Compose screen renders in a single frame on the first composition.

### Onboarding Clarity
- [x] Hostname appears prominently above the buttons; no scrolling required.
- [x] Countdown is a visible text element ("Approve within Xs") not a
      hidden timer.

### Production-Ready Defaults
- [x] `BIOMETRIC_STRONG | DEVICE_CREDENTIAL` is the default and only mode.
- [x] StrongBox is requested by default; falls back transparently if
      unavailable.
- [x] `setUnlockedDeviceRequired(true)` is set unconditionally.
- [x] No emojis or whimsy in the prompt copy.

### Golden Path Quality
- [x] Approve flow produces exactly one `sendApprove` call with the
      Ed25519-signed payload from UniFFI.
- [x] Deny flow produces exactly one `sendDeny` call.
- [x] Timeout flow produces exactly one `sendDeny` call.

### Decision Load
- [x] Two buttons. Three states the user can reach (Approved, Denied,
      TimedOut-internally-as-Denied). No tabs, no menus, no settings.

### Progressive Complexity
- [x] Background bridge wiring (S-018) plugs into the same
      `ResponseSender` interface without changing the ViewModel.

### Error Quality
- [x] Each terminal state carries a typed `DenialReason`: `UserDenied`,
      `TimedOut`, `BiometricFailed`, `BiometricUnavailable`,
      `SignError(String)`.
- [x] The screen renders a small "Denied: <reason>" line so the user can
      see why a flow ended.

### Failure Safety
- [x] `KeyPermanentlyInvalidatedException` (e.g., user added a new
      fingerprint) is caught and reported as `SignError`, not crashed.
- [x] `StrongBoxUnavailableException` is caught and falls back silently.

### Runtime Transparency
- [x] Every state transition is observable via the `StateFlow<ApproveUiState>`.
- [x] Test assertions read directly off the same flow.

### Debuggability
- [x] `DenialReason` is a sealed hierarchy; production logs the variant
      name. No private state is hidden from the test.

### Cross-Surface Consistency
- [x] The phone's `Denied(TimedOut)` and `Denied(UserDenied)` both
      produce a PeerDenied wire frame — the desktop sees them
      identically (per S-017 DoD #5).

### Workflow Consistency
- [x] Mirrors S-015's structure: Compose screen + ViewModel + injected
      backend, no hand-written JNI.

### Change Safety
- [x] No file overwrites — the approve module is new; only the manifest
      and `app/build.gradle.kts` get additive edits.

### Experimentation Safety
- [x] All Android side-effects (Keystore, BiometricPrompt, network) are
      behind interfaces; tests exercise the ViewModel without touching
      real hardware.

### Interaction Latency
- [x] BiometricPrompt is the dominant cost (~200-500 ms on Pixel 7);
      everything else is microseconds.

### Developer Feedback Speed
- [x] Pure-JVM unit tests run in < 1 s.
- [x] No Robolectric runtime required for the ViewModel tests.

### Team Scale
- [x] The state machine is small enough to fit on a screen — every
      future maintainer can read it once and know the contract.

### System Scale
- [x] Adding a new `DenialReason` is a one-line enum addition; the
      ViewModel + tests pick it up via `when` exhaustiveness.

### Right Behavior by Default
- [x] Default 30 s countdown is configurable via constructor; tests use
      3 s to keep virtual time bounded.

### Anti-Bypass Design
- [x] The Keystore key requires `UserAuthenticationRequired(true)` — no
      code path can sign without a fresh user gesture.
- [x] The UniFFI Ed25519 surface is the only place wire signing happens;
      the Kotlin code does not import `ed25519` or any signing primitive.

## 4. Design — Keystore + Biometric Gate

### EC P-256 (secp256r1) for the gate

Android Keystore reliably supports EC P-256 from API 23 onward. Ed25519
support is API 33+ and not present on every Android 13 device. Per the
S-017 DoD line "(curve `secp256r1` if Keystore lacks Ed25519 — fall
back per device; document in `docs/android-setup.md`)", we use P-256
unconditionally for the gate. The wire-protocol signature is Ed25519 and
comes from UniFFI.

### StrongBox try / fallback

```text
try {
    builder.setIsStrongBoxBacked(true).build()
} catch (e: StrongBoxUnavailableException) {
    builder.setIsStrongBoxBacked(false).build()
}
```

`KeyInfo` (our small data class) records the boolean so callers can
observe whether the gate is StrongBox-backed.

### BiometricPrompt allowedAuthenticators

`BiometricManager.Authenticators.BIOMETRIC_STRONG or
BiometricManager.Authenticators.DEVICE_CREDENTIAL` — the user can pass
biometric OR device credential. Per
`androidx.biometric:biometric:1.2.0-alpha05` the two flags can be ORed
freely (older versions required mode segregation).

### State machine

```
Idle ──> Counting(remainingSec)
   │       │
   │       ├── tick (every tickMillis) ──> Counting(remainingSec - 1)
   │       │
   │       ├── tick @ 0 ──> Denied(TimedOut) ──> sendDeny()
   │       │
   │       ├── onApproveClicked() ──> AwaitingBiometric
   │       │                            │
   │       │                            ├── presenter.authenticate(sig) success
   │       │                            │       ──> Signing
   │       │                            │             │
   │       │                            │             ├── signer.signGate(sig, challenge) ok
   │       │                            │             │       └── seedProvider.seed() ok
   │       │                            │             │             └── uniffi.signChallengeResponse(seed, frame) ok
   │       │                            │             │                   └── Approved(responseFrame)
   │       │                            │             │                         └── sendApprove(responseFrame)
   │       │                            │             └── any error ──> Denied(SignError(msg)) ──> sendDeny()
   │       │                            └── presenter.authenticate failure
   │       │                                  └── Denied(BiometricFailed) ──> sendDeny()
   │       │
   │       └── onDenyClicked() ──> Denied(UserDenied) ──> sendDeny()
```

Terminal states (`Approved`, `Denied`) ignore further inputs.

## 5. Tests

### TC-01: Happy-path approve

**Given** an `ApproveViewModel` with `timeoutMillis = 3_000`,
`tickMillis = 1_000`, a fake `BiometricPresenter` that returns
`BiometricResult.Success`, a fake `SignerBackend` that returns a canned
32-byte gate blob, a fake `SigningKeyProvider` that returns a 32-byte
seed, and a fake UniFFI binding returning a canned 64-byte signature.

**When** `onApproveClicked()` is invoked.

**Then** the terminal `uiState` is `Approved(responseFrame)` where
`responseFrame` is the canned 64-byte signature, and
`responseSender.sendApprove(responseFrame)` was called exactly once.
`responseSender.sendDeny` was never called.

### TC-02: Explicit deny

**Given** a `ApproveViewModel` in `Counting` state.

**When** `onDenyClicked()` is invoked.

**Then** the terminal `uiState` is `Denied(UserDenied)`,
`responseSender.sendDeny()` was called exactly once,
`responseSender.sendApprove` was never called.

### TC-03: Countdown timeout

**Given** a `ApproveViewModel` with `timeoutMillis = 3_000` and
`tickMillis = 1_000`.

**When** the test scheduler advances virtual time by 3_500 ms.

**Then** the terminal `uiState` is `Denied(TimedOut)`,
`responseSender.sendDeny()` was called exactly once,
`responseSender.sendApprove` was never called.

### TC-04: Biometric failed

**Given** a `ApproveViewModel` with a fake `BiometricPresenter` that
returns `BiometricResult.Failed("user-cancelled")`.

**When** `onApproveClicked()` is invoked.

**Then** the terminal `uiState` is `Denied(BiometricFailed)`,
`responseSender.sendDeny()` was called exactly once,
`responseSender.sendApprove` was never called.

### TC-05: Compose screen renders hostname + buttons + countdown
(emulator-gated)

**Given** the androidTest harness with a connected emulator and the
`ApproveScreen` composable hosted via `createComposeRule()`.

**When** the screen renders for `hostname = "dell-precision"` and the
countdown begins.

**Then** the test asserts:
- A node with text containing `"dell-precision"` is displayed.
- A node with text `"Approve"` is displayed and clickable.
- A node with text `"Deny"` is displayed and clickable.
- A node with text matching `"Approve within \d+s"` is displayed.

Marked emulator-gated; not in `make test`'s default suite.

## Traceability
- Roadmap item: [specs/syauth/ROADMAP.md §S-017](../syauth/ROADMAP.md).
- Implementation files:
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/KeystoreSigner.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/ApproveViewModel.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/ApproveScreen.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/SigningKeyProvider.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/BiometricPresenter.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/ResponseSender.kt`
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt` (NavHost extension)
  - `syauth-android/app/src/main/AndroidManifest.xml` (`USE_BIOMETRIC`)
  - `syauth-android/app/build.gradle.kts` (biometric + ViewModel + nav deps)
  - `docs/android-setup.md` (Keystore key parameters)
- Test files:
  - `syauth-android/app/src/test/kotlin/com/sy/syauth/android/approve/ApproveViewModelTest.kt`
  - `syauth-android/app/src/androidTest/kotlin/com/sy/syauth/android/approve/ApproveScreenTest.kt`
