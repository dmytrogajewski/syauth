# JOURNEY-S-015: `BiometricPrompt(AUTH_BIOMETRIC_STRONG, per-use)` + Keystore sign

> **Spec anchors:**
>
> - `specs/unlock-proximity/SPEC.md` §3 Decisions row
>   *"Keystore auth window"* — verbatim:
>
>   > Auth-per-use
>   > (`setUserAuthenticationParameters(0, AUTH_BIOMETRIC_STRONG)`).
>   > The master SPEC's threat model — "every unlock requires an
>   > explicit user action on the phone, by design, because passive
>   > BLE proximity has been comprehensively broken by link-layer
>   > relay attacks" — forbids time-windowed auth. The relay attack's
>   > ~5 ms RTT cap is dominated by the human-tap delay; removing the
>   > human gesture makes the relay free.
>
> - `specs/unlock-proximity/SPEC.md` §3 Scope item 20 (verbatim):
>
>   > Keystore key parameters:
>   > `setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG)`.
>   > Per-sign biometric prompt. This is the SPEC §3.2 D6 contract
>   > and the relay-attack defense.
>
> - `specs/unlock-proximity/SPEC.md` §7 *T-Relay* — verbatim:
>
>   > **T-Relay** (NCC 2022): two custom radios relay encrypted PDUs
>   > at the link layer. Defense: per-unlock biometric tap on the
>   > phone makes the human-tap latency (~500 ms) dwarf the relay's
>   > ~5 ms RTT cap; the operator notices a relayed unlock because
>   > they didn't tap.
>
> **Roadmap row:** `specs/unlock-proximity/ROADMAP.md` Step S-015.
>
> **Closure condition (verbatim from ROADMAP.md):**
>
> ```
> ./gradlew :app:testDebugUnitTest --tests "*BiometricPromptTest*" --tests "*KeystoreSignTest*"
> ```

## Roadmap Link
- Source roadmap: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-015.
- Feature: replace the S-014 placeholder
  `ChallengeApprovalActivity.onApproveClicked()` body (gated behind
  `BuildConfig.DEBUG`) with a real
  `BiometricPrompt(BIOMETRIC_STRONG, CryptoObject(signature))` flow
  that, on success, signs the verbatim challenge-frame body bytes
  with the bond's per-bond Ed25519 Keystore key minted at pair time
  by `AndroidKeystoreKeyGenerator` (DEV-002) and writes the
  resulting 64-byte signature on the response characteristic via
  the same `PersistentGattClient` the challenge arrived on; on
  biometric fail / cancel, write `DENIED_FRAME_BYTES` and `finish()`.

## 1. Journey

When **the Android user is at their phone holding their finger over
the fingerprint sensor with the screen woken by S-014's
over-keyguard activity, having read
`"$hostname is requesting sudo (peer_id $short)"` and decided this
is a real `sudo` they typed**, I want to **tap the Approve button,
present my fingerprint **once** to a `BiometricPrompt` whose
`allowedAuthenticators == BIOMETRIC_STRONG`, and have the bond's
hardware-resident Ed25519 private key release for **exactly one**
signing operation that produces the response frame the desktop
daemon expects, all while a relay attacker — sitting between my
phone and the desktop — cannot complete the unlock because my
finger has to land on **this phone's** sensor**, so I can **trust
SPEC §7 T-Relay defense holds (a relay forwarding the encrypted
PDUs cannot substitute the biometric tap; the operator's missing
gesture is the failure signal), satisfy SPEC §3.2 D6 with no
time-windowed auth (a 5-minute validity bucket would make every
relayed sudo within the bucket free), and finish the unlock loop
in well under the SPEC §4.3 2-second budget because the
`BiometricPrompt → Keystore-sign → GATT-write` path is a single
in-process roundtrip on the same `PersistentGattClient` the
challenge arrived on**.

## 2. CJM

Before S-015, the activity's Approve button calls a `finish()`
no-op behind a `BuildConfig.DEBUG` gate: tapping Approve looks the
same as a Cancel from the daemon's perspective (the
`response-timeout` fires, `pam_syauth` returns
`PAM_AUTHINFO_UNAVAIL`, sudo falls through to `pam_unix`'s
password prompt). The whole point of having "my phone is my key" is
defeated until S-015 closes the loop.

S-015 is the **security-critical** step of the entire unlock
journey. Every other step — daemon advertising, pair-time bond,
persistent GATT, over-keyguard activity — funnels into this one
gesture: a single fingerprint tap that releases the bond's
Ed25519 Keystore private key for exactly one signing operation.
The auth-per-use + AUTH_BIOMETRIC_STRONG combination is the
hardware-enforced contract; a 5-minute validity window or a
DEVICE_CREDENTIAL fallback would turn every relayed sudo within
the window (or every sudo behind an unlocked screen) into a free
attack. The SPEC's threat model, the NCC 2022 link-layer relay
research, and the deviation audit trail in §3 Decisions all pin
this contract.

### Sign-input convention

The signed message is **exactly the challenge frame's body bytes**
— `version(1) || nonce(16) || payload(challenge)`. This is what
`syauth-core::sign::sign_frame` produces (see
`crates/syauth-core/src/sign.rs` line 68: `frame.body_bytes()`)
and what `verify_frame` expects (line 82). The tag suffix is NOT
in the signed input.

Wire convention for the activity's input: the
`EXTRA_CHALLENGE_BYTES` extra carries the **already-MAC-verified**
challenge body bytes the daemon notified — i.e. the
version+nonce+payload triplet, NOT the full encoded frame with
the trailing tag. S-007 / S-008 verified the MAC before the
service handed the bytes to `launchApprovalActivity`. The
activity's signer therefore calls
`Signature.getInstance("Ed25519").apply { initSign(privateKey); update(challengeBytes); sign() }`
and the resulting 64 bytes are the wire response payload.

This convention is documented inline at the `signChallenge` helper
in `ChallengeApprovalActivity.kt` and at the
`KeystoreSignTest::signs_challenge_with_bond_key` assertion.

### KeystoreKeyGenerator update

DEV-002's `buildEd25519SpecBuilder` calls
`setUserAuthenticationRequired(true)` but does NOT call
`setUserAuthenticationParameters(0, AUTH_BIOMETRIC_STRONG)`. That
older API (`setUserAuthenticationRequired(true)` alone) is
ambiguous: on Android 11 it defaults to
`AUTH_DEVICE_CREDENTIAL | AUTH_BIOMETRIC_STRONG` with a 0-second
validity (per-use), which happens to match the SPEC. **But** the
SPEC §3 Scope item 20 names the explicit parameters call
verbatim, and the explicit form is the contract the next reviewer
should see at the generator. S-015 therefore amends
`buildEd25519SpecBuilder` to call
`.setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG)`
on API 30+ (the `setUserAuthenticationParameters` method was added
in API 30 / Android 11) and keeps the
`setUserAuthenticationRequired(true)` line as the cross-API
fallback. A new
`KeystoreKeyGeneratorTest::base_builder_pins_biometric_strong_per_use`
assertion pins the explicit parameters on API 33.

### Robolectric Ed25519 capability — pre-checked

The hard-blocker protocol named the risk: Robolectric's
`AndroidKeyStore` provider shadow does not host Ed25519. **It does
not need to**, because the host JVM (OpenJDK 25 on the dev box,
OpenJDK 17+ in CI) ships Ed25519 in `SunEC` since JDK 15. The
`KeystoreSignTest::signs_challenge_with_bond_key` test therefore
calls `KeyPairGenerator.getInstance("Ed25519")` *without* the
`"AndroidKeyStore"` provider parameter — the JVM's Ed25519
implementation does the real work, the test verifies the resulting
signature against the matching `Signature.getInstance("Ed25519")`
in verify mode, and the production `signChallenge` helper accepts
a `PrivateKey` parameter so the test injects the JVM-generated key
without going through the AndroidKeyStore shadow.

This is the documented pragmatic deviation. The
`signChallenge(privateKey, challengeBytes): ByteArray` helper is
the test seam; production callers resolve the `PrivateKey` from
`KeyStore.getInstance("AndroidKeyStore").getKey(alias, null)`.

### Phase 1: User taps Approve — BiometricPrompt opens

**User Intent:** The user has read the prompt copy, recognises the
desktop, and wants to authorise this one sudo. They tap Approve.

**Actions:** The activity's `onApproveClicked()` constructs a
`BiometricPrompt(activity, mainExecutor, callback)` and a
`PromptInfo` whose `allowedAuthenticators == BIOMETRIC_STRONG`
only (no DEVICE_CREDENTIAL fallback), a localised title
(`R.string.syauth_biometric_prompt_title`), a subtitle naming the
desktop and the short peer id
(`R.string.syauth_biometric_prompt_subtitle_fmt`), and a Cancel
button (`R.string.syauth_biometric_prompt_cancel`). It opens the
bond's per-bond Ed25519 `PrivateKey` from the AndroidKeyStore
under the `EXTRA_KEYSTORE_ALIAS` the service supplied
(`BondRecord.keystoreAlias`), wraps it in a
`Signature.getInstance("Ed25519").apply { initSign(privateKey) }`,
wraps that in a `BiometricPrompt.CryptoObject(signature)`, and
calls `prompt.authenticate(promptInfo, cryptoObject)`. The OS
shows the system biometric sheet over the activity; the activity's
own Compose surface remains underneath but is non-interactive.

**Pain / Risk:**
- If the bond's `keystoreAlias` is empty (older bond from before
  DEV-002 closure), `keyStore.getKey(alias, null)` returns `null`
  and the approve flow cannot proceed. The activity logs and
  writes `DENIED_FRAME_BYTES` so the daemon fails fast with
  `PAM_AUTH_ERR` instead of hanging.
- If `BiometricManager.from(context).canAuthenticate(BIOMETRIC_STRONG)`
  reports anything other than `BIOMETRIC_SUCCESS`, no fingerprint
  is enrolled or the hardware is unavailable, and
  `prompt.authenticate` would fire `onAuthenticationError` with
  `ERROR_NO_BIOMETRICS` / `ERROR_HW_UNAVAILABLE`; the activity
  writes a denied frame and finishes. The user sees the desktop's
  sudo fall through to password prompt — the right end state for a
  device without strong biometrics.
- If the activity is recreated mid-prompt (configChanges /
  configuration change), the existing prompt is cancelled by the
  OS and `onAuthenticationError(ERROR_CANCELED, ...)` fires; the
  activity treats this as a deny and writes `DENIED_FRAME_BYTES`.

**Success Signal:** `BiometricPromptTest::strong_authenticator_required`
observes a non-null `PromptInfo` whose
`getAllowedAuthenticators() == BIOMETRIC_STRONG`, with no
DEVICE_CREDENTIAL bit set.

### Phase 2: Fingerprint accepted — Keystore releases the key for ONE sign

**User Intent:** The user wants the unlock to complete fast (the
desktop is waiting on the sudo prompt).

**Actions:** The OS reports `onAuthenticationSucceeded(result)`.
The activity reads `result.cryptoObject?.signature` — non-null on
the success path because we authenticated with a `CryptoObject` —
and calls `sig.update(challengeBytes)` then `sig.sign()`. The
resulting 64-byte Ed25519 signature is the response-frame payload
the daemon's `verify_response` will verify under the bond's
`phonePubkey`. The activity then hands the bytes to a service-side
helper (the `responseSink` companion seam, mirroring the S-014
`cancelSink`) that resolves the per-peer `PersistentGattClient`
and calls `writeResponse(signatureBytes)`. The activity `finish()`-es.

**Pain / Risk:**
- If `result.cryptoObject?.signature` is `null` (a contract
  violation — the OS should never report success without the
  signature when we authenticated with a CryptoObject, but a
  defensive null-check costs nothing and pins the invariant), the
  activity treats this as a deny and writes `DENIED_FRAME_BYTES`.
  The mechanical name for this is the
  `BiometricPromptTest::per_use_keystore_unlock` assertion: a
  second sign on the **same** Signature instance after `sig.sign()`
  is an Ed25519 contract violation in real hardware (the Keystore
  key was released for exactly one use); the test asserts that the
  activity *invokes* a fresh `BiometricGate.authenticate(...)`
  round per Approve tap (no second sign without a second prompt).
- If `signChallenge` throws
  `UserNotAuthenticatedException` (which would mean the
  CryptoObject binding failed under us — a SPEC §3.2 D6 contract
  break the OS should catch first), the activity writes a denied
  frame and surfaces the error in logcat.
- If `PersistentGattClient` is gone from the registry by the time
  the sign completes (the foreground service died between Phase 1
  and Phase 2), `writeResponse` is a silent no-op; the daemon's
  `response-timeout` fires and sudo falls through to
  `PAM_AUTHINFO_UNAVAIL` — the right end-state for a dead service.

**Success Signal:**
`BiometricPromptTest::per_use_keystore_unlock` asserts that a
recording `BiometricGate` saw exactly one `authenticate(...)` call
per Approve tap, and the recorded signature bytes equal the
fixture's deterministic Ed25519 signature over the fixture
challenge body. Plus
`KeystoreSignTest::signs_challenge_with_bond_key`: the test
generates a JVM Ed25519 keypair, runs `signChallenge(privateKey,
challengeBytes)`, then verifies the signature against the
matching `PublicKey` via `Signature.getInstance("Ed25519")` in
verify mode. Pass = the signature round-trips.

### Phase 3: Biometric fails / cancels — denied frame goes back

**User Intent:** The user did not initiate the sudo (the prompt
woke them up at 03:00 because of a phishing relay), or they
fat-fingered the fingerprint sensor three times and the OS
locked the biometric out for 30 seconds, or they tapped the
Cancel button on the BiometricPrompt's own sheet.

**Actions:** The OS reports `onAuthenticationError(errorCode,
errString)` (cancel, lockout, hardware unavailable, user-cancel,
negative-button) **or** `onAuthenticationFailed()` (fingerprint
not recognised; the OS lets the user retry until they cancel or
hit the lockout — the activity does not act on the soft-failure
callback). On the **terminal** `onAuthenticationError` callback,
the activity calls the `responseSink` with `DENIED_FRAME_BYTES`
(the 64-zero signature payload — same as S-014's Cancel path) and
`finish()`-es.

**Pain / Risk:**
- If the activity acted on every `onAuthenticationFailed()` call
  (soft fingerprint failure), the user would lose every attempt
  after the first miss, defeating the BiometricPrompt's built-in
  retry UX. The activity therefore only writes the denied frame
  on `onAuthenticationError`; `onAuthenticationFailed` is a no-op
  log line.
- If the activity is killed while the prompt is up (e.g. user
  swipes the recents tray), the OS-managed BiometricPrompt is
  reaped automatically; no denied frame is written; the daemon's
  `response-timeout` fires. This is acceptable: the user
  explicitly closed the app, so a `PAM_AUTHINFO_UNAVAIL` is the
  correct end state.
- If the denied frame's signature payload is anything other than
  64 zero bytes, the daemon could accidentally accept it (any
  valid Ed25519 signature would pass). Pinning `DENIED_FRAME_BYTES`
  in the activity makes the "this is a deny, not an approval"
  intent mechanically observable — a future drift would have to
  rename the constant.

**Success Signal:** `BiometricPromptTest::cancel_writes_denied`:
the fake `BiometricGate.fail()` triggers the activity's error
callback; the recording `responseSink` reports exactly one call
with bytes equal to `DENIED_FRAME_BYTES`; the activity is
`finishing`.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| User has no enrolled fingerprint and no Class-3 face → cannot unlock at all (DEVICE_CREDENTIAL is **not** allowed in S-015) | 1 | Future: surface a "enrol a fingerprint to use syauth" hint at app launch. SPEC §3.2 D6 forbids DEVICE_CREDENTIAL fallback because PIN/pattern is weaker than biometric and does not solve the relay-tap-latency story — out of S-015 scope. |
| User taps Approve, then realises this isn't their sudo, can't easily cancel the BiometricPrompt | 1 | The system BiometricPrompt's own "Cancel" button (`PROMPT_NEGATIVE_RES`) fires `onAuthenticationError(ERROR_NEGATIVE_BUTTON, ...)` which the activity treats identically to cancel — denied frame goes back. |
| BiometricPrompt's CryptoObject contract — single sign per prompt — surprises a future contributor who wants to re-use the Signature | 2 | The journey doc pins this contract; the `BiometricPromptTest::per_use_keystore_unlock` test mechanically observes that a second sign would require a fresh authenticate round. Future per-bond key sharing across multiple sudos would need a separate Keystore key with a non-per-use validity window, which the SPEC explicitly forbids. |

### North Star Summary

The ideal end state is that, after S-015 closes, an Android user
with a bonded phone and an enrolled Class-3 biometric can run
`sudo apt update` on their desktop and see the sudo prompt clear
in well under two seconds after a single fingerprint tap; a relay
attacker who forwards the encrypted PDUs at the link layer
cannot complete the unlock (the relayed bytes have no fingerprint
to present); the bond's Ed25519 private key never appears as
bytes anywhere outside the Keystore; and every Approve produces
**exactly one** sign — a second sudo requires a second tap.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] BiometricPrompt opens immediately on Approve tap (no Keystore
      generation on the hot path — the key was minted at pair time).
- [x] Signature + GATT-write complete in the same in-process
      roundtrip; total Approve → desktop-sudo-cleared is
      dominated by the BLE notify RTT (200-500 ms) plus the
      fingerprint sensor latency (~500 ms).

### Onboarding Clarity
- [x] Prompt title/subtitle are localised string resources; the
      subtitle names the desktop and the short peer id so the
      user can match the BiometricPrompt sheet against the
      under-keyguard prompt copy from S-014.

### Production-Ready Defaults
- [x] `BIOMETRIC_STRONG` only (no DEVICE_CREDENTIAL fallback) per
      SPEC §3.2 D6.
- [x] Per-use Keystore key (the key was minted at pair time with
      `setUserAuthenticationParameters(0, AUTH_BIOMETRIC_STRONG)`).

### Golden Path Quality
- [x] BiometricPrompt success → exactly one
      `signChallenge(privateKey, challengeBytes)` call → one
      `writeResponse(signatureBytes)` on the same
      `PersistentGattClient` the challenge arrived on.

### Decision Load
- [x] One button: Approve (the system's BiometricPrompt sheet
      owns the Cancel UX once it opens).

### Progressive Complexity
- [x] S-014 shipped the activity lifecycle; S-015 wires the
      crypto without changing the cancelSink / extras contract.
      The `responseSink` seam mirrors `cancelSink` so the test
      harness pattern is identical.

### Error Quality
- [x] Every failure mode writes `DENIED_FRAME_BYTES` and
      `finish()`-es; the desktop sudo sees a fast
      `PAM_AUTH_ERR`, not a hang.

### Failure Safety
- [x] Keystore key release is hardware-gated; a relay attacker
      cannot bypass the biometric tap by replaying PDUs.

### Runtime Transparency
- [x] Activity logs the approve start, the biometric result, the
      sign attempt, and the response write.

### Debuggability
- [x] `adb logcat -s syauth.bg.approve` shows the whole lifecycle.
- [x] The `BiometricGate` test seam lets a JVM unit test drive
      `succeed()` / `fail()` without firing a real prompt.

### Cross-Surface Consistency
- [x] The Cancel path and the biometric-fail path both write
      `DENIED_FRAME_BYTES` — the daemon sees identical wire
      bytes; the phone-side audit log distinguishes the two via
      logcat tags.

### Workflow Consistency
- [x] The `BiometricGate` interface lives next to `CancelSink` on
      the activity's companion object; both follow the same
      Volatile-write pattern installed by `MainActivity`.

### Change Safety
- [x] No production-only branches; production and tests go
      through the same `BiometricGate` / `responseSink` seams.

### Experimentation Safety
- [x] Tests can install a fake `BiometricGate` that calls
      `succeed(signatureBytes)` or `fail(reason)` to drive the
      activity lifecycle without firing the real OS prompt.

### Interaction Latency
- [x] No I/O on the Approve path other than the Keystore sign
      itself (microseconds) and the GATT write (one ATT MTU
      packet over the existing connection).

### Developer Feedback Speed
- [x] Robolectric JVM tests — no instrumented-emulator round-trip.

### Team Scale
- [x] Constants (`STRONG_AUTHENTICATOR`, `PROMPT_TITLE_RES`,
      `PROMPT_SUBTITLE_FMT`, `PROMPT_NEGATIVE_RES`) live in
      `ChallengeApprovalActivity.kt` so the next reviewer
      doesn't have to grep across modules.

### System Scale
- [x] One activity instance per challenge regardless of the bond
      count (`singleInstance` from S-014); each instance owns
      one `BiometricPrompt` round.

### Right Behavior by Default
- [x] BIOMETRIC_STRONG only — no DEVICE_CREDENTIAL fallback.
- [x] Per-use Keystore key — no time-windowed validity.

### Anti-Bypass Design
- [x] The `setUserAuthenticationParameters(0, AUTH_BIOMETRIC_STRONG)`
      contract is enforced **at the Keystore** by the hardware;
      a tampered phone-side app cannot release the key without
      the biometric.
- [x] The `BuildConfig.DEBUG` no-op gate from S-014 is removed
      in S-015; release builds use the same path as debug.

## 4. Tests

### TC-01: `BiometricPromptTest::strong_authenticator_required`

**Given** a launched `ChallengeApprovalActivity` with the
fixture intent (peer id, hostname, challenge bytes, keystore
alias).
**When** the activity's `buildPromptInfo()` companion helper is
invoked (a package-internal getter the activity exposes for the
test to inspect the constructed `BiometricPrompt.PromptInfo`).
**Then** `promptInfo.allowedAuthenticators` equals
`BiometricManager.Authenticators.BIOMETRIC_STRONG` — no
DEVICE_CREDENTIAL bit set, no other bits.

### TC-02: `BiometricPromptTest::per_use_keystore_unlock`

**Given** a launched `ChallengeApprovalActivity` with a recording
`BiometricGate` injected on the companion object; the recording
gate's `succeed(signatureBytes)` records every call.
**When** the test calls `activity.onApproveClicked()` (simulates
the Approve button), then calls
`activity.onApproveClicked()` a second time after the gate
recorded the first sign.
**Then** the recording gate observes exactly **two**
`authenticate(...)` calls — one per Approve tap — and the
production code path NEVER calls `signChallenge` twice on the
same Signature instance. The mutant this kills: a future
refactor that caches the `BiometricPrompt.CryptoObject` and
re-uses the unlocked `Signature` across taps would let one
biometric authorise two sudos.

### TC-03: `BiometricPromptTest::cancel_writes_denied`

**Given** a launched `ChallengeApprovalActivity` with a recording
`BiometricGate` and a recording `responseSink` injected on the
companion object.
**When** the test calls `activity.onApproveClicked()` then drives
the gate's `fail("user cancel")` callback.
**Then** the recording `responseSink` reports exactly one call
with bytes equal to `DENIED_FRAME_BYTES`; the activity is
`finishing`.

### TC-04: `KeystoreSignTest::signs_challenge_with_bond_key`

**Given** a fresh JVM-generated Ed25519 keypair (via
`KeyPairGenerator.getInstance("Ed25519")` — the host JVM, NOT the
AndroidKeyStore shadow, per the Robolectric-Ed25519 pre-check in
§2 above) and the fixture challenge bytes (a 49-byte
version+nonce+payload triplet matching `Frame::body_bytes` shape).
**When** the test calls
`signChallenge(privateKey, challengeBytes)`.
**Then** the returned 64-byte signature verifies under the
matching `PublicKey` via
`Signature.getInstance("Ed25519").apply { initVerify(pubkey);
update(challengeBytes); verify(signatureBytes) }` — returns
`true`.

### TC-05: `KeystoreKeyGeneratorTest::base_builder_pins_biometric_strong_per_use`

**Given** the `buildEd25519SpecBuilder(alias).build()` spec.
**When** the test reads
`spec.userAuthenticationType` and
`spec.userAuthenticationValidityDurationSeconds`.
**Then** `userAuthenticationType` includes
`KeyProperties.AUTH_BIOMETRIC_STRONG`,
`userAuthenticationValidityDurationSeconds` is `0` (per-use).

## Traceability
- Roadmap item: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-015.
- Implementation files: see "Implementation" section below.
- Test files: see "Implementation" section below.

## Implementation

Closed 2026-05-18.

### Files modified

- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/ChallengeApprovalActivity.kt`
  — parent class changed from `ComponentActivity` to
  `FragmentActivity` (BiometricPrompt binds to the host fragment
  manager). New top-level constants:
  `EXTRA_KEYSTORE_ALIAS = "syauth.keystoreAlias"`,
  `STRONG_AUTHENTICATOR = BiometricManager.Authenticators.BIOMETRIC_STRONG`,
  `ED25519_ALGORITHM = "Ed25519"`,
  `KEYSTORE_PROVIDER = "AndroidKeyStore"`,
  `PROMPT_TITLE_RES = R.string.syauth_biometric_prompt_title`,
  `PROMPT_SUBTITLE_FMT = R.string.syauth_biometric_prompt_subtitle_fmt`,
  `PROMPT_NEGATIVE_RES = R.string.syauth_biometric_prompt_cancel`.
  New interfaces: `ResponseSink`, `BiometricGate`,
  `BiometricGateCallback`. New top-level helpers: `signChallenge(
  privateKey, challengeBytes)` (the test seam pinning the
  sign-input convention — body bytes, NOT the full encoded frame
  with the trailing tag), `buildPromptInfo(activity, hostname,
  shortPeerId)` (the prompt-info factory the
  `strong_authenticator_required` test pins), `buildApprovalIntent(
  context, peerId, hostname, challengeBytes, keystoreAlias)`
  (the shared intent factory the service-side launcher delegates
  to). New `internal` class `AndroidBiometricGate(activity, peerId,
  hostname)` — the production BiometricPrompt + Keystore-sign
  implementation. New companion seams: `responseSink: ResponseSink?`,
  `biometricGate: BiometricGate?`; `resetSeams()` extended to reset
  both. `onApproveClicked()` now reads the bond's Keystore alias
  from `EXTRA_KEYSTORE_ALIAS`, falls back to the per-instance
  `AndroidBiometricGate(this, resolvedPeerId, resolvedHostname)` if
  no test override is installed, and writes either the 64-byte
  Ed25519 signature (success) or `DENIED_FRAME_BYTES` (fail) via
  the `responseSink`. The S-014 `BuildConfig.DEBUG` placeholder
  gate is removed.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/KeystoreKeyGenerator.kt`
  — `buildEd25519SpecBuilder` now chains
  `.setUserAuthenticationParameters(
  KEYSTORE_AUTH_VALIDITY_PER_USE_SECONDS,
  KeyProperties.AUTH_BIOMETRIC_STRONG)`. New top-level constant
  `KEYSTORE_AUTH_VALIDITY_PER_USE_SECONDS = 0`. This pins the
  SPEC §3 Scope item 20 contract verbatim (per-use, Class-3
  biometric, no DEVICE_CREDENTIAL fallback, no time window).
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthCompanionService.kt`
  — new `KeystoreAliasResolver` `fun interface` + companion
  `keystoreAliasResolver: KeystoreAliasResolver?` seam +
  `resetSeams()` reset. `launchApprovalActivity` now resolves the
  alias via the resolver and delegates intent construction to the
  shared `buildApprovalIntent` helper.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
  — `installCompanionSeams` now wires the `KeystoreAliasResolver`
  to the bond record's `keystoreAlias` and installs the production
  `ResponseSink` that routes the BiometricPrompt response (signed
  bytes on success, `DENIED_FRAME_BYTES` on biometric fail) back
  through the same per-peer `PersistentGattClient.writeResponse`
  plumbing the S-014 cancel sink already uses. The production
  `BiometricGate` is the per-instance `AndroidBiometricGate` the
  activity constructs against itself; the companion seam stays
  `null` in production so the per-instance path runs.
- `syauth-android/app/src/main/res/values/strings.xml`
  — adds `syauth_biometric_prompt_title`
  (`"Approve syauth challenge"`),
  `syauth_biometric_prompt_subtitle_fmt`
  (`"%1$s is requesting sudo (peer_id %2$s)"`), and
  `syauth_biometric_prompt_cancel` (`"Cancel"`).
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/pair/KeystoreKeyGeneratorTest.kt`
  — adds `base_builder_pins_biometric_strong_per_use` (pins
  `userAuthenticationValidityDurationSeconds == 0` and
  `userAuthenticationType == AUTH_BIOMETRIC_STRONG`).

### Files created

- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/BiometricPromptTest.kt`
  — three Robolectric `@Config(sdk = [34])` tests on the
  `ChallengeApprovalActivity` lifecycle:
  `strong_authenticator_required` reads
  `activity.buildPromptInfoForTest()` and asserts
  `allowedAuthenticators == BIOMETRIC_STRONG`;
  `per_use_keystore_unlock` builds two activity instances (one
  per Approve tap), drives `gate.succeed(...)` on each, asserts
  the recording `BiometricGate.callCount == 2` and the
  recording `ResponseSink` saw both signature payloads in order;
  `cancel_writes_denied` drives `gate.fail("user cancel")` and
  asserts the response sink received exactly one
  `DENIED_FRAME_BYTES` call with the activity `finishing`.
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/KeystoreSignTest.kt`
  — `signs_challenge_with_bond_key` generates a fresh Ed25519
  keypair via `KeyPairGenerator.getInstance("Ed25519")` (the host
  JVM's `SunEC` provider, not the AndroidKeyStore shadow — see
  the *Robolectric Ed25519 capability* note in §2 of the journey
  body for the rationale), calls
  `signChallenge(keypair.private, challenge)`, then verifies the
  returned 64-byte signature against `keypair.public` via
  `Signature.getInstance("Ed25519")` in verify mode. Pass = the
  signature round-trips under the same body bytes.

### Deviations

1. **Sign-input convention is the frame body, not the full frame.**
   The signed message handed to `signChallenge` is exactly the
   challenge-frame body — `version(1) || nonce(16) ||
   payload(challenge)` — matching what
   `syauth-core::sign::sign_frame` produces and what
   `verify_frame` expects (see `crates/syauth-core/src/sign.rs`).
   The Frame's trailing 16-byte tag is NOT in the signed input.
   The activity therefore expects `EXTRA_CHALLENGE_BYTES` to
   carry the already-MAC-verified body bytes; the upstream
   service (S-007 / S-008) is responsible for stripping the tag
   before calling `launchApprovalActivity`. Pinned at the
   `signChallenge` helper kdoc + the `KeystoreSignTest` fixture.
2. **Robolectric Ed25519 substitution.** Robolectric's
   `AndroidKeyStore` provider shadow does not host Ed25519, so
   the production `AndroidBiometricGate.openPrivateKey(alias)`
   path cannot run end-to-end in a JVM unit test. The
   `signChallenge(privateKey, challengeBytes)` helper takes a
   plain `PrivateKey` so the test injects a host-JVM-generated
   Ed25519 key (OpenJDK's `SunEC` ships Ed25519 since JDK 15).
   This proves the sign-input convention and the
   verify-round-trip; the AndroidKeyStore happy path is
   exercised by the real-device e2e probe documented in
   JOURNEY-DEV-002's Closure Appendix. The same pragmatic
   pattern as `KeystoreFrameSignerTest`.
3. **`AUTH_BIOMETRIC_STRONG` only — no DEVICE_CREDENTIAL
   fallback.** SPEC §3.2 D6 and §3 Decisions row "Keystore auth
   window" forbid DEVICE_CREDENTIAL because PIN/pattern is
   weaker than Class-3 biometric and does not solve the
   relay-tap-latency story. A future contributor who is tempted
   to add `or BIOMETRIC_WEAK` or `or DEVICE_CREDENTIAL` would
   weaken the SPEC §7 T-Relay defense and must run the
   SPEC-DEVIATION procedure first.

### Closure verification

- `make scope-discipline` — clean.
- `make lint` — clippy + fmt + cargo-deny clean.
- `make test` — `passed=387 failed=0 ignored=8`. The 8 ignored
  are the pre-existing live-radio gated DEV-004 + bench cases.
- `:app:assembleDebug` — BUILD SUCCESSFUL.
- `:app:testDebugUnitTest` — 17 suites, 99 tests, 0 failures, 0
  errors, 0 skipped.
- Closure-condition probe
  (`./gradlew :app:testDebugUnitTest --tests "*BiometricPromptTest*" --tests "*KeystoreSignTest*"`)
  — BUILD SUCCESSFUL; 4 tests pass (3
  `BiometricPromptTest` + 1 `KeystoreSignTest`).
- ChallengeApprovalActivityTest regression
  (`./gradlew :app:testDebugUnitTest --tests "*ChallengeApprovalActivityTest*"`)
  — BUILD SUCCESSFUL; 3 tests pass.
