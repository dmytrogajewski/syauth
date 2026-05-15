# JOURNEY-S-019: Full e2e on real radios

<!-- Authored per .agents/skills/journey/SKILL.md template. -->

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md](../syauth/ROADMAP.md) — item **S-019**.
- Feature: a hermetic CI job (`make e2e-real`) that boots an Android
  emulator, installs the syauth APK, scripts a pair, then drives the
  nine SPEC §4.3 wire cases through the production `BlueZBtPeer` on the
  host and the `SyauthCompanionService` on the emulator. Records p50 /
  p95 / p99 histograms for the budgeted cases to
  `docs/perf-baselines.md`. Gated on `SYAUTH_E2E_REAL=1` so `make test`
  on a developer box stays green without an emulator or BLE radio.

## 1. Journey

When **a CI host (or a developer with an AVD and a BLE radio) wants to
prove that the desktop PAM module and the Android companion actually
talk to each other over a real Bluetooth link** I want to **run a single
`make e2e-real` target that boots an emulator, provisions a bond,
exercises every SPEC §4.3 scenario, and records the latency budget**
so I can **trust the green status next to S-019 instead of trusting a
mock and a hand-wave**.

## 2. CJM

Until this item, every PAM e2e test in the repo runs against an
in-process `MockBtPeer` (S-009). The mock is good — it pins the
nine-case matrix in `crates/syauth-pam/tests/pam_e2e.rs` and guards
every reason-token in `AuthOutcome`. But it cannot catch a regression
in `bluer`'s adapter probe, in `BlueZBtPeer::connect`'s typed-error
mapping, in the GATT MTU split when the real controller chooses 23
instead of 247, in the OS-level CDM bind on Android 14, or in the
real-clock latency budget. S-010 already gated a `bluer_smoke` test
behind `SYAUTH_E2E=1` to keep the dependency *linked*; S-018 shipped
the Android service that exposes the GATT server. S-019 is the
connective tissue: it stitches the real desktop transport to the real
Android companion through an emulator and reports back honestly.

The four non-negotiables for this item:

1. **No silent skips on hosts that cannot run the test.** When
   `SYAUTH_E2E_REAL` is unset, every test prints a single explanatory
   `eprintln!` line and returns 0. When it IS set but a prerequisite
   is missing (no AVD, no `adb`, no APK), the runner exits non-zero
   with an actionable error — silent green on a misconfigured CI host
   is a bug.
2. **Latency budgets are asserted, not just measured.** The golden
   case must finish under 2.0 s p95 across `E2E_RUN_COUNT` runs
   (default 100); the offline case under 1.2 s p99. Recording these
   to `docs/perf-baselines.md` is *append-only*; the assertion runs
   on every CI run regardless of recording.
3. **Pairing is scripted, not interactive.** The `syauth pair`
   subcommand grows a hidden `--scripted-oob <hex>` flag that takes
   the OOB code directly from a string. The
   `scripts/e2e-emulator-up.sh` script reads the code from the
   emulator's logcat, feeds it to `syauth pair`, then exports the
   bond id so the Rust test can pick the right peer. No human is in
   the loop.
4. **The flake budget is zero, with a documented escape valve.** If
   a case flakes on CI, the operator files a bug via `/bug` and
   either fixes the race or quarantines the case with
   `#[ignore = "QUARANTINED: <bug-id>"]` before merge. Quarantines
   are a stop-the-line signal, not a maintenance pattern.

### Phase 1: Emulator boot + bond provisioning

**User Intent:** Have a running emulator that the desktop side can
treat as a paired Android peer, without any human input.

**Actions:**
1. Operator (or CI) runs `make e2e-real` with `SYAUTH_E2E_REAL=1`.
2. The Make target invokes `scripts/e2e-emulator-up.sh`.
3. The script verifies `adb`, `emulator`, and `cargo` are on `PATH`.
4. The script verifies the APK at
   `syauth-android/app/build/outputs/apk/debug/app-debug.apk` exists
   (else fails loudly with the gradle command to fix it).
5. The script starts the AVD named `syauth_e2e` headless, waits for
   `adb wait-for-device`, then installs the APK with `adb install
   -r`.
6. The script reads the OOB code printed by the app to logcat under
   the documented tag `syauth-pair-oob`, then drives
   `cargo run -p syauth-cli -- pair --adapter hci0 --yes
   --scripted-oob <hex>`.
7. On success, the script writes `SYAUTH_E2E_PEER_BOND_ID=<id>` to
   `.env.e2e` at the repo root and exits 0.

**Pain / Risk:**
- `emulator` not on PATH (host has Java but no Android SDK): the
  script names this exact failure with the fix-it command
  (`Install Android Studio's command-line tools and add
  $ANDROID_HOME/emulator to PATH`).
- AVD `syauth_e2e` does not exist: the script names the
  `avdmanager create avd -n syauth_e2e -k 'system-images;android-34;...'`
  command verbatim.
- Logcat OOB code never appears (the app crashed): the script tails
  logcat for the failure stack and dumps the last 200 lines to
  stderr.
- BlueZ on the host has no virtual radio: the script uses `btvirt`
  if present, else fails with a hint about loading `vhci`.
- Repeated runs leave a stale bond: the script invokes
  `syauth revoke` for any prior bond with the same emulator MAC
  before the new pair.

**Success Signal:** `.env.e2e` exists with `SYAUTH_E2E_PEER_BOND_ID`
set; `adb devices` lists the emulator with state `device`.

### Phase 2: Run the nine SPEC §4.3 cases

**User Intent:** Drive each scenario to its expected `AuthOutcome` /
`PamReturn` against the real BLE transport.

**Actions:**
1. The Make target runs `cargo test --package syauth --test
   e2e_real -- --nocapture --test-threads=1`.
2. Each `#[tokio::test]` reads `SYAUTH_E2E_REAL` at start. If unset,
   it prints `e2e-real skipped: set SYAUTH_E2E_REAL=1` and returns.
3. Otherwise the test loads the bond id from
   `.env.e2e`, opens a `BlueZBtPeer` against `hci0`, drives one PAM
   roundtrip via the in-process `auth::authenticate(&cfg)` (NOT
   `sudo`), and asserts the case-specific behaviour.

**Pain / Risk:**
- Emulator drops the BLE link between scenarios: each test reopens
  the `BlueZBtPeer`; the harness does NOT cache a session across
  cases.
- Adapter is powered off by another process during the run: typed
  `TransportError::AdapterMissing` maps to `AuthInfoUnavail`; the
  test still asserts an outcome but tags the run as
  environment-degraded in the histogram.
- p95 outlier from a hot-cache effect on the first run: the
  histogram discards the first run as warmup, sorts the remaining
  N-1 samples, and reports p50 / p95 / p99 against that.
- A scenario hangs forever: each `connect`/`recv` call carries the
  production timeout (`DEFAULT_AUTH_TIMEOUT` = 1.2 s); the test
  wraps each case in a top-level
  `tokio::time::timeout(CASE_HARD_DEADLINE, …)` of 10 s so a stuck
  test fails fast.
- Real radio returns a corrupted frame: the SPEC §4.3 oversized /
  incomplete-reassembly reasons fire. The test asserts the reason
  matches and re-runs the case up to `MAX_E2E_RETRIES` (default 0
  — flake budget is zero); if retries are enabled and a re-run
  passes, the test still fails with a "flake observed" message.

**Success Signal:** Each test prints a single line
`e2e-real <case>: <outcome> elapsed=<ms>` and asserts the documented
outcome.

### Phase 3: Histogram recording

**User Intent:** Track perf regressions over time in
`docs/perf-baselines.md` without polluting the file on every CI run.

**Actions:**
1. The test computes p50, p95, p99 across the N samples.
2. If `SYAUTH_E2E_REAL_WRITE_BASELINES=1` is set, the test calls
   `append_baseline(case, hist)` which appends one Markdown table
   row to the case's section.
3. Without the write flag, the test merely asserts p95 (golden) or
   p99 (offline) is under the documented budget.

**Pain / Risk:**
- Two CI hosts append concurrently: not addressed in v0.1 — the
  recording is gated on a manual `SYAUTH_E2E_REAL_WRITE_BASELINES`
  variable that is set on only the "blessed" baseline-recording
  host. Documented in the file header.
- Recording row has no run id: every appended row includes a stable
  run id (env `E2E_RUN_ID`, falling back to a hex of the current
  unix timestamp).
- Baseline file is hand-edited and the placeholder row is removed:
  the helper tolerates the missing placeholder and just appends; it
  never re-writes the header section.

**Success Signal:** The case section in `docs/perf-baselines.md`
gains exactly one new row whose `p95_ms` value is below the
documented budget.

### Phase 4: Teardown

**User Intent:** Leave the host in the same state as before the run.

**Actions:**
1. Make target invokes `scripts/e2e-emulator-down.sh`.
2. The script kills the emulator, removes `.env.e2e`, and reports
   exit status.

**Pain / Risk:**
- Emulator already gone (crashed mid-run): the script tolerates a
  missing PID and exits 0.
- Stale lockfile under `~/.android/avd/syauth_e2e.avd/`: the
  script removes the `*.lock` files explicitly so the next run can
  reuse the AVD.
- The `make` invocation was interrupted before this phase: the
  teardown script is idempotent and safe to re-run by hand.

**Success Signal:** `adb devices` shows no emulator;
`.env.e2e` is absent.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| AVD provisioning is fully out-of-band today | 1 | Future S-021 packaging step can ship a `scripts/e2e-emulator-create.sh` that runs `avdmanager` for the operator. |
| Histogram file grows linearly with CI runs | 3 | Document a quarterly archive rotation under `docs/perf-baselines/` directory; not in v0.1 because the row count is tiny. |
| Quarantine policy is documented, not enforced | 2 | A future skill (`/flake`) could parse `#[ignore = "QUARANTINED: ..."]` markers and gate merge; out of v0.1 scope. |

### North Star Summary

A first-time contributor runs `SYAUTH_E2E_REAL=1 make e2e-real` on a
machine with the prerequisites installed, watches the nine cases
pass, and sees a fresh row in `docs/perf-baselines.md` that proves
the round-trip stayed inside its budget. On a stock developer box
without the prerequisites, `make test` stays green without any
configuration; the e2e-real harness is invisible until the operator
opts in.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] Operator runs `SYAUTH_E2E_REAL=1 make e2e-real` and gets a
      pass/fail verdict in under 5 minutes on a warm host.
- [x] On a host without prerequisites the harness skips silently
      (default `make test`) — zero onboarding friction for the
      common case.

### Onboarding Clarity
- [x] Missing prerequisites name the fix command verbatim
      (`./gradlew :app:assembleDebug`, `avdmanager create ...`).
- [x] The Makefile help section names the new target.

### Production-Ready Defaults
- [x] `SYAUTH_E2E_REAL` is OFF by default; `make test` is unchanged.
- [x] `E2E_RUN_COUNT` defaults to 100; one knob, sane.

### Golden Path Quality
- [x] The golden case asserts `AuthOutcome::Success` AND a p95
      latency under 2.0 s. Both checks must pass.
- [x] The offline case asserts `AuthInfoUnavail` AND a p99 latency
      under 1.2 s.

### Decision Load
- [x] The operator never picks a peer; the scripted pair handles it.
- [x] The histogram write is gated on a single explicit env var,
      not a heuristic.

### Progressive Complexity
- [x] Hosts without BLE / Android stay on `make test` (the simple
      case is unaffected).
- [x] CI hosts with the stack opt in via one env var.

### Error Quality
- [x] Each scenario asserts a kebab-token `reason` so a regression
      names the failing step.
- [x] The emulator-up script aborts loudly on any missing
      prerequisite with a copy-paste fix.

### Failure Safety
- [x] The teardown script is idempotent; re-running it is safe.
- [x] No write outside `target/`, `docs/perf-baselines.md`, and the
      AVD-local `.env.e2e`.

### Runtime Transparency
- [x] Every test prints a `e2e-real <case>: …` line so a CI log
      reader can grep one stream for the matrix.

### Debuggability
- [x] The histogram preserves the raw `Vec<Duration>` in memory
      long enough to print the worst sample on assertion failure.
- [x] `--nocapture` is in the Make target so the log lines reach
      stdout without rerunning.

### Cross-Surface Consistency
- [x] The reason tokens used here match the ones already pinned by
      `crates/syauth-pam/tests/pam_e2e.rs` (the S-009 fixture).
- [x] The `SYAUTH_E2E_REAL` gate mirrors the `SYAUTH_E2E` gate from
      S-010 in shape (env var literal `1`; everything else skips).

### Workflow Consistency
- [x] The Makefile target follows the existing `android-test` skip
      pattern (preflight checks, exit 0 on missing-prereq).
- [x] The journey doc and roadmap evidence section use the same
      shape as every prior S-### item.

### Change Safety
- [x] `make test` is unchanged. Existing tests on a developer box
      keep their behaviour.
- [x] The baseline file's append is gated; an unintended write is
      blocked by the missing env var.

### Experimentation Safety
- [x] The `--scripted-oob` flag is hidden from `--help` (clap
      `hide = true`) and warns on stderr when used; an operator
      cannot accidentally bypass interactive confirmation.

### Interaction Latency
- [x] Per-case wall-clock asserted; a regression to >2 s on golden
      fails the build immediately.

### Developer Feedback Speed
- [x] `--test-threads=1` keeps the test output linear in time so
      a CI failure is bisected in one log read.

### Team Scale
- [x] `.env.e2e` is git-ignored by the AVD-local convention; bond
      ids are not committed.
- [x] `docs/perf-baselines.md` is committed; everyone reads the
      same numbers.

### System Scale
- [x] Adding a tenth wire case is one new `#[tokio::test]` row.
- [x] The histogram helper takes the case name as a parameter so a
      new case is one line of code.

### Right Behavior by Default
- [x] Default behaviour on a stock host is "skip with a clear
      message". No accidental network or radio access.
- [x] Default for `E2E_RUN_COUNT` is 100; the operator can lower it
      for a fast smoke run.

### Anti-Bypass Design
- [x] The `--scripted-oob` flag prints a one-line warning to stderr
      when used; documented in the journey and the help text.
- [x] Production builds never reach the scripted path because the
      flag is opt-in; the production `pair` flow still prompts
      interactively.

## 4. Tests

### TC-01: Golden case under p95 budget

**Given** `SYAUTH_E2E_REAL=1`, the emulator is bonded, the desktop
adapter is powered, and the user has authenticated on the phone
within the test's setup window.
**When** the test runs `golden_case` for `E2E_RUN_COUNT` iterations.
**Then** every iteration returns `AuthOutcome::Success { peer_id }`
and the sorted p95 sample is below `GOLDEN_P95_BUDGET`
(`Duration::from_millis(2_000)`).

### TC-02: Offline case under p99 budget

**Given** the bonded peer is unreachable (the emulator's BT radio is
toggled off via `adb shell svc bluetooth disable`).
**When** the test runs `offline_case` for `E2E_RUN_COUNT` iterations.
**Then** every iteration returns
`AuthOutcome::AuthInfoUnavail { reason: "offline", .. }` and the p99
sample is below `OFFLINE_P99_BUDGET`
(`Duration::from_millis(1_200)`).

### TC-03: Slow case under p95 budget

**Given** the emulator deliberately delays its biometric prompt
acknowledgment by ~500 ms.
**When** the test runs `slow_case` once.
**Then** the outcome is `AuthOutcome::Success` and elapsed is below
`GOLDEN_P95_BUDGET` (the slow case must still fit the same
2.0-second budget; the slack is what justifies the budget existing).

### TC-04: Replay case → AuthErr

**Given** the test pre-seeds `auth::replay_seed` with a fixed nonce
and the emulator is instructed (via an in-app dev-knob) to reuse
that nonce in its response.
**When** the test runs `replay_case`.
**Then** the outcome is `AuthOutcome::AuthErr { reason: "replay", ..
}`.

### TC-05: Bad-signature case → AuthErr

**Given** the emulator's GATT server is instructed (via a debug
intent) to flip the first byte of its signature.
**When** the test runs `bad_sig_case`.
**Then** the outcome is
`AuthOutcome::AuthErr { reason: "bad-signature", .. }`.

### TC-06: Wrong-version case → AuthErr

**Given** the emulator's GATT server is instructed to set the
version byte to `0x02`.
**When** the test runs `wrong_version_case`.
**Then** the outcome is
`AuthOutcome::AuthErr { reason: "wrong-version", .. }`.

### TC-07: Revoked case → AuthInfoUnavail (no radio)

**Given** the bond entry on the desktop is marked `Revoked` (via
`syauth revoke <peer_id>` before the test).
**When** the test runs `revoked_case`.
**Then** the outcome is
`AuthOutcome::AuthInfoUnavail { reason: "no bonded peer", .. }`
AND the elapsed time is under
`REVOKED_WALL_CLOCK_UPPER_BOUND` (200 ms — the radio was never
touched).

### TC-08: MTU-split case → Success

**Given** the emulator forces a smaller MTU (e.g. 23) so the
challenge frame must be reassembled across multiple GATT
notifications.
**When** the test runs `mtu_split_case`.
**Then** the outcome is `AuthOutcome::Success` (reassembly correct).

### TC-09: Panic-in-core case → AuthErr, syslog captured

**Given** the desktop is configured with a fault-injection
environment variable `SYAUTH_TEST_PANIC=verify` so the verify step
panics inside the `catch_unwind` boundary.
**When** the test runs `panic_in_core_case`.
**Then** the outcome is
`AuthOutcome::AuthErr { reason: "panicked-in-core", .. }` and the
syslog (via `journalctl -t pam_syauth`) contains a corresponding
line.

### TC-10: Default skip when `SYAUTH_E2E_REAL` is unset

**Given** the env var is unset.
**When** the test binary runs every case once.
**Then** each `#[tokio::test]` prints
`e2e-real skipped: set SYAUTH_E2E_REAL=1` and returns 0 — `make
test` stays green.

### TC-11: `--scripted-oob` bypasses prompt, runs against mock

**Given** the `syauth-cli` `pair` subcommand is invoked with
`--scripted-oob deadbeef --yes` against a `MockPairBackend`.
**When** the test drives `run_pair_with_io` reading from an EMPTY
stdin.
**Then** the pair flow completes to `PairingPhase::Bonded` without
consuming any input from the reader, the writer contains the
`scripted-oob in effect` warning line, and the bond store gains
exactly one entry.

### TC-12: Inspection-only — Makefile target is wired

**Given** the repo at HEAD.
**When** an operator runs `grep -E '^e2e-real:' Makefile`.
**Then** the target exists, is `.PHONY`, gates on
`SYAUTH_E2E_REAL=1`, and the help section names it.

## Traceability
- Roadmap item: [`specs/syauth/ROADMAP.md` § S-019](../syauth/ROADMAP.md).
- Implementation files: `tests/e2e_real.rs`,
  `scripts/e2e-emulator-up.sh`, `scripts/e2e-emulator-down.sh`,
  `Makefile` (e2e-real target), `crates/syauth-cli/src/pair.rs`
  (`--scripted-oob` flag), `docs/perf-baselines.md`.
- Test files: `tests/e2e_real.rs`,
  `crates/syauth-cli/src/pair.rs` (unit test for `--scripted-oob`).
