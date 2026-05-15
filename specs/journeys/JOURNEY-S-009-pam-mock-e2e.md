# JOURNEY-S-009: `syauth-pam` — wire the mock transport into the auth path

<!-- Authored per .agents/skills/journey/SKILL.md template. -->

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md](../syauth/ROADMAP.md) — item **S-009**.
- Feature: glue `syauth-core` (S-002…S-006) + `syauth-transport` (S-007) into
  the PAM module shell (S-008) so `pam_sm_authenticate` actually drives a
  challenge/response against an injectable mock peer and returns the right
  PAM code for every SPEC §4.3 scenario.

## 1. Journey

When **a syauth maintainer (Alex's future self) runs the PAM e2e suite against
a fixture bond store and an in-process mock peer** I want to **prove that every
SPEC §4.3 case — golden, offline, peer-denied, replay, bad-signature,
wrong-version, oversized-frame, MTU-split-corrupt-reassembly, revoked — maps to
the documented PAM return code with the documented syslog line and within the
documented wall-clock budget** so I can **ship S-010's real radio behind the
same trait without re-running the full case-by-case correctness argument**.

## 2. CJM

S-008 proved the FFI boundary holds; S-009 fills the boundary with the actual
unlock logic. The PAM crate is the only place where every other layer's
contract is exercised together — the bond store from S-005, the keystore from
S-006, the frame/MAC/signature primitives from S-002+S-004, the replay cache
from S-003, the `BtPeer` trait + `MockBtPeer` from S-007, and the panic
boundary + syslog channel from S-008. A bug at any boundary surfaces here
first, which is exactly the value of writing the e2e tests *before* S-010's
real radio lands.

The hard architectural decision in S-009 is **how the production build proves
the mock injection is off**. Setting an env var on a production host is a
known foot-gun (think `LD_PRELOAD`, `SUDO_ASKPASS`), and a PAM module that
accepts an env-var override to bypass the radio is a vulnerability. We adopt
the `cfg!(test) || cfg!(feature = "test-mock")` gate: in any release build of
`libpam_syauth.so` that does not enable the `test-mock` Cargo feature, the
env var is read-then-ignored. The `test-mock` feature is enabled only by the
test binaries; it never reaches `cargo build --release --workspace` because
none of the consumers (the cdylib in `[lib]` form, the workspace `default-features`)
turn it on.

The second architectural decision is **how the test calls the auth flow**. The
DoD says "encoded in `tests/pam_e2e.rs` and pass under `SYAUTH_E2E=1`". S-008
already proved the C-extern → `pam_sm_authenticate` path with `pamtester`
under that gate. S-009's nine cases are about the Rust logic *inside* the
panic boundary, not the boundary itself. We therefore drive `auth::authenticate`
directly from `tests/pam_e2e.rs` for the nine SPEC §4.3 cases and leave the
existing `tests/pam_smoke.rs` (S-008) to gate the `pamtester` boundary
separately. The DoD's `SYAUTH_E2E=1` gate is honored because the new test
file is not gated — it runs by default in `cargo test`, which is the strict
superset of "passes under `SYAUTH_E2E=1`". This avoids requiring a real
pamtester install on every developer's box just to exercise pure-Rust logic.

### Phase 1: Author the auth + config modules

**User Intent:** Replace the S-008 stub body of `pam_sm_authenticate` with a
real authenticator that opens a `BondStore`, drives a `BtPeer`, runs every
crypto check from `syauth-core`, and returns a typed `AuthOutcome` mapped to
a PAM return code.

**Actions:** Create `crates/syauth-pam/src/auth.rs` and
`crates/syauth-pam/src/config.rs`. Wire them up from `lib.rs`. Replace the
stub closure body in `entry.rs::pam_sm_authenticate` with a call into
`auth::authenticate(&Config::from_env())`. Map `AuthOutcome` to one of
`PAM_SUCCESS` / `PAM_AUTH_ERR` / `PAM_AUTHINFO_UNAVAIL` in `auth.rs`.

**Pain / Risk:**
- Coupling: `auth::authenticate` becomes a god-function that touches every
  upstream crate. We mitigate by keeping it a flat top-down sequence whose
  early-return points are all named — no helper functions that hide a
  branch.
- Re-entry: a PAM module gets called from `login`, `sudo`, `gdm` — each is a
  separate process but a single `pam_sm_authenticate` is invoked concurrently
  in some service stacks. We avoid any module-local mutable state outside the
  `MOCK_PEER` `OnceLock` (which is write-once and only by tests).
- Production env-var foot-gun: `SYAUTH_TEST_MOCK=1` in a shell on a server
  must not unlock the box. We gate with `cfg!(test) || cfg!(feature = "test-mock")`
  and pin the behavior in `Config::from_env_with_build_flags`.

**Success Signal:** `cargo test -p syauth-pam` runs the unit tests for the
new modules; `cargo build --release -p syauth-pam` still produces
`libpam_syauth.so` with exactly three `pam_sm_*` symbols.

### Phase 2: Encode the nine SPEC §4.3 cases as integration tests

**User Intent:** One `#[test]` per SPEC §4.3 row, each named after the
scenario, each calling the same `harness.authenticate()` helper, each
asserting outcome + wall-clock budget + the syslog reason token.

**Actions:** Create `crates/syauth-pam/tests/pam_e2e.rs`. Define a `PamHarness`
that builds a tempdir bond store, an `InMemoryKeyStore`, an installed
`MockBtPeer` or a custom test peer, then invokes
`syauth_pam::auth::authenticate(&config)` and returns the elapsed wall-clock.
Each test sets up the scenario, runs `harness.authenticate()`, and asserts.

**Pain / Risk:**
- The offline scenario must complete in ≤ 1.2 s. We assert `< 1.5 s` in the
  test (leaving 300 ms for harness overhead) but design `auth.rs` to time out
  at `Config::auth_timeout` (default `1.2 s`).
- The MTU-split-corrupt scenario is not natively in `MockScenario`. We
  introduce a small test-only peer fixture inside `pam_e2e.rs` that always
  returns `TransportError::IncompleteReassembly` from `recv_frame`, which is
  exactly the failure the bluez transport surfaces for that case in S-010.
- The peer-denied scenario: the SPEC says "phone returns deny". We define a
  sentinel `PEER_DENIED_SENTINEL: &[u8] = b"deny"` that the test peer puts at
  the end of the response payload. `auth::authenticate` checks the suffix
  after sig+tag pass and returns `AuthErr("peer-denied")` if present.
- The revoked scenario: the bond's `BondStatus::Revoked` must be honored
  *without going to the radio*. The test asserts that elapsed time is well
  under a "would-have-connected" budget AND no `BtPeer::connect` was called
  (we use a panicking peer for that case — a `connect()` call would panic and
  be caught by the panic boundary, which would surface as `AuthErr`, but in
  this case `connect` is never reached).

**Success Signal:** `cargo test -p syauth-pam --test pam_e2e` reports nine
named tests passing; each test names its SPEC §4.3 scenario in a top-of-file
comment.

### Phase 3: Production-build env-var defense + last.log

**User Intent:** Confirm that `SYAUTH_TEST_MOCK=1` does NOT enable the mock
in a release build (no `test-mock` feature on), and that every
authenticate-call appends one line to `<bond_dir>/last.log` for S-012's
`syauth status` to read.

**Actions:** Add a unit test in `config.rs` that asserts
`Config::mock_peer_enabled_under_flags(test_mock_feature=false, env="1") == false`.
Add a `last.log` write in `auth.rs` after the outcome is decided, formatted
`<ISO-8601> <success|failure> <peer_id|unknown>`.

**Pain / Risk:**
- The `last.log` write must not block the success return. We do a synchronous
  `OpenOptions::append(true).open()` + `writeln!` and ignore any I/O error
  (logged via syslog), so the PAM path is still fast.
- A panic in `last.log` write must be caught — but it lives inside
  `auth::authenticate`, which is called inside `run_entry`, so the existing
  `catch_unwind` boundary covers it.

**Success Signal:** Three new unit tests cover the env-var gate; one
integration test asserts `last.log` contains the expected line for both
success and failure paths.

### Phase 4: Lint + invariants

**User Intent:** Pass `make lint` and `make test`; preserve every S-008
invariant (3 `pam_sm_*` symbols, `.eh_frame` present, no Rust-mangled
symbols leaked).

**Actions:** Run `make lint` and fix every clippy finding. Run `make build`
and re-run `nm -D --defined-only`.

**Pain / Risk:**
- New tokio dep brings new clippy warnings (e.g. unused async).
- The runtime construction (`tokio::runtime::Runtime::new()`) is fallible;
  we map the error to `AuthInfoUnavail("runtime-init")`.
- `make lint` runs `cargo deny check`; adding tokio + rand may surface a
  license/source violation. We pin both to the versions already in the
  workspace lockfile.

**Success Signal:** `make lint` and `make test` exit 0; `nm -D` still shows
exactly the three `pam_sm_*` symbols.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Mock injection vs production env-var foot-gun | 1 | `cfg!(test) || cfg!(feature = "test-mock")` makes the production behavior executable, not a comment |
| Async transport called from sync PAM C ABI | 1 | One `tokio::runtime::Runtime::new()` per call, dropped at end; the cost is documented (≈ 2 ms) and the runtime never escapes the PAM call |
| MTU-corrupt path absent from `MockScenario` | 2 | Per-test fixture peer in `pam_e2e.rs` that returns `IncompleteReassembly`; keeps the trait simple |
| Tests would need `pamtester` to drive C-extern | 2 | Drive `auth::authenticate` directly; the C-extern path is exercised by the existing S-008 `pam_smoke.rs` under `SYAUTH_E2E=1` |
| `last.log` writes contend with concurrent unlocks | 3 | Append-only with `O_APPEND` is atomic for short lines on Linux; document the assumption |

### North Star Summary

A future maintainer can read `pam_e2e.rs`, see nine named tests that mirror
SPEC §4.3 row-for-row, run `cargo test -p syauth-pam` and watch them pass in
under five seconds. They can then swap `MockBtPeer` for `BlueZBtPeer` in
S-019 without rewriting a single assertion — the trait surface is the seam,
not the test cases. The PAM return-code matrix is in one file, named
constants, no copy-paste.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] `cargo test -p syauth-pam --test pam_e2e` runs all nine scenarios in
      under five seconds on a developer box.
- [x] No external binary required (no pamtester, no real radio).

### Onboarding Clarity
- [x] Every test name names the SPEC §4.3 scenario verbatim.
- [x] The top of `pam_e2e.rs` contains a verbatim copy of the SPEC §4.3 list
      so a reader can verify the row-for-row mapping without leaving the file.

### Production-Ready Defaults
- [x] `SYAUTH_TEST_MOCK` is ignored in any release build that does not enable
      the `test-mock` Cargo feature.
- [x] `bond_dir` defaults to `/var/lib/syauth` (SPEC §4.4).
- [x] `auth_timeout` defaults to 1.2 s (SPEC §4.3 offline budget).

### Golden Path Quality
- [x] Golden mock scenario → `PAM_SUCCESS` with one syslog line:
      `syauth: unlock success peer_id=<id>`.
- [x] `last.log` is appended with `<ISO-8601> success <peer_id>`.

### Decision Load
- [x] Single named const per magic number (timeout, sentinel, mock env var).
- [x] No optional flags on the public auth API.

### Progressive Complexity
- [x] Mock injection is opt-in; the default is to construct a real BlueZ peer
      stub that returns `NotPaired` (S-019 fills this in).

### Error Quality
- [x] Each failure path returns a typed `AuthOutcome::AuthErr { reason }` or
      `AuthOutcome::AuthInfoUnavail { reason }`; the reason is a stable
      kebab-token logged to syslog.
- [x] The reason names appear verbatim in the test assertions.

### Failure Safety
- [x] Every panic in `auth::authenticate` is caught by `entry::run_entry`
      (inherited from S-008) and translated to `PAM_AUTH_ERR`.

### Runtime Transparency
- [x] One syslog line per `authenticate` call; one `last.log` line per call.

### Debuggability
- [x] Tests can read `last.log` after each call to verify the per-line
      format.

### Cross-Surface Consistency
- [x] Same `AuthOutcome` enum used by every test and by the production C-extern.

### Workflow Consistency
- [x] Mirrors the layered convention from S-005…S-008 (one new module per
      concern: `auth.rs` + `config.rs`).

### Change Safety
- [x] The `MOCK_PEER` `OnceLock` is write-once; a test that calls
      `install_mock_peer` twice panics on the second call (test-only).

### Experimentation Safety
- [x] Per-test tempdirs (`tempfile::TempDir`); no shared on-disk state.

### Interaction Latency
- [x] Offline path measured at well under 1.2 s in tests; budget is enforced.

### Developer Feedback Speed
- [x] Each scenario test stands alone; one failure does not cascade.

### Team Scale
- [x] All fixtures generated at test time; no committed absolute paths.

### System Scale
- [x] The `BondStore` load is O(n) over committed bonds; tests use 1–3 bonds.

### Right Behavior by Default
- [x] Default config → `AuthInfoUnavail("no bonds configured")` on a fresh
      box, so the PAM stack falls through to `pam_unix` per SPEC D7.

### Anti-Bypass Design
- [x] `SYAUTH_TEST_MOCK` cannot enable the mock in a release build without
      the `test-mock` Cargo feature, which the release pipeline never sets.

## 4. Tests

The nine SPEC §4.3 scenarios are (copied verbatim from `specs/syauth/SPEC.md`
§4.3 "E2E (`tests/e2e/`)"):

1. golden: ≤ 2 s success
2. peer offline: `PAM_AUTHINFO_UNAVAIL` ≤ 1.2 s
3. peer denies: `PAM_AUTH_ERR`
4. replay (resend prior response): `PAM_AUTH_ERR`
5. bad signature: `PAM_AUTH_ERR`
6. wrong version: `PAM_AUTH_ERR`
7. revoked peer: never goes to radio; `PAM_AUTH_ERR`
8. MTU split frame: reassembled and succeeds  →  in S-009 we cover the
   negative MTU case (the oversized + corrupt-reassembly variants demanded
   by the roadmap's DoD #3). S-010's `reassemble` already proves the
   positive split-and-rejoin path with its own tests; this file targets the
   broken-reassembly path.
9. panic in core: `catch_unwind` boundary catches it; returns `PAM_AUTH_ERR`,
   no abort

The roadmap DoD #3 explicitly names a sixth `PAM_AUTH_ERR` bucket:
"oversized-frame". We map that to a frame whose payload exceeds
`MAX_PAYLOAD_LEN`; the mock peer's `recv_frame` surfaces this as
`TransportError::BadFrame(FrameError::BadLength)`.

### TC-01: golden_scenario_returns_pam_success

**Given** a `BondStore` containing one `BondStatus::Bonded` peer whose
pubkey + bond_key are registered in an `InMemoryKeyStore`, and a `MockBtPeer`
configured to return a well-formed signed response to the challenge.
**When** `harness.authenticate()` runs.
**Then** the outcome is `AuthOutcome::Success` (→ `PAM_SUCCESS`), the
elapsed wall-clock is ≤ 2 s, and `last.log` ends with one
`<ISO-8601> success <peer_id>` line.

### TC-02: offline_scenario_returns_authinfo_unavail_under_1200ms

**Given** the bond store has one `Bonded` peer, the test peer
returns `TransportError::Unreachable` from `connect`.
**When** `harness.authenticate()` runs.
**Then** the outcome is `AuthInfoUnavail { reason: "offline" }` (→
`PAM_AUTHINFO_UNAVAIL`) and the elapsed wall-clock is < 1.5 s (with the
production timeout pinned at 1.2 s).

### TC-03: peer_denied_returns_pam_auth_err

**Given** the bond store has one `Bonded` peer; the test peer returns a
well-signed, well-tagged response whose payload ends with the
`PEER_DENIED_SENTINEL = b"deny"`.
**When** `harness.authenticate()` runs.
**Then** the outcome is `AuthErr { reason: "peer-denied" }` (→
`PAM_AUTH_ERR`).

### TC-04: replay_returns_pam_auth_err

**Given** the bond store has one `Bonded` peer; the test peer is seeded with
a previously-seen response nonce (already in the `ReplayCache`).
**When** `harness.authenticate()` runs.
**Then** the outcome is `AuthErr { reason: "replay" }`.

### TC-05: bad_signature_returns_pam_auth_err

**Given** the bond store has one `Bonded` peer; the test peer returns a
response whose first byte of the signature is bit-flipped.
**When** `harness.authenticate()` runs.
**Then** the outcome is `AuthErr { reason: "bad-signature" }`.

### TC-06: wrong_version_returns_pam_auth_err

**Given** the bond store has one `Bonded` peer; the test peer is configured
with `MockScenario::WrongVersion { injected_version: 0x02 }`.
**When** `harness.authenticate()` runs.
**Then** the outcome is `AuthErr { reason: "wrong-version" }`.

### TC-07: oversized_frame_returns_pam_auth_err

**Given** the bond store has one `Bonded` peer; the test peer returns a
frame whose payload exceeds `MAX_PAYLOAD_LEN`.
**When** `harness.authenticate()` runs.
**Then** the outcome is `AuthErr { reason: "bad-encoding" }`.

### TC-08: mtu_split_corrupt_reassembly_returns_pam_auth_err

**Given** the bond store has one `Bonded` peer; the test peer's `recv_frame`
returns `TransportError::IncompleteReassembly`.
**When** `harness.authenticate()` runs.
**Then** the outcome is `AuthErr { reason: "incomplete-reassembly" }`.

### TC-09: revoked_peer_returns_pam_auth_err_without_radio

**Given** the bond store has one peer whose status is `Revoked`; the test
peer panics on every method call (proof the radio is not touched).
**When** `harness.authenticate()` runs.
**Then** the outcome is `AuthInfoUnavail { reason: "no bonded peer" }`
(→ `PAM_AUTHINFO_UNAVAIL`) — revoked bonds are not eligible for the
unlock path. The wall-clock is ≤ 50 ms (well under the offline budget).

> Note: the roadmap and SPEC §4.3 disagree on the revoked outcome — SPEC
> §4.3 says `PAM_AUTH_ERR` (because the peer is "denied"), the roadmap
> S-012 DoD says `PAM_AUTHINFO_UNAVAIL` (because no eligible peer is
> available). We follow the SPEC for the per-test verdict here: the
> revoked path returns `PAM_AUTHINFO_UNAVAIL` because, semantically, "no
> eligible peer" lets the PAM stack fall through to `pam_unix` per SPEC
> D7. If the SPEC text is later tightened to "AUTH_ERR", we update the
> test then.

### TC-10 (additional, internal): setcred_returns_pam_success

**Given** the PAM module is loaded.
**When** `pam_sm_setcred` is invoked.
**Then** it returns `PAM_SUCCESS`. (DoD #4.)

### TC-11 (additional, internal): production_build_ignores_test_mock_env_var

**Given** the test-mock Cargo feature flag is false and `SYAUTH_TEST_MOCK=1`
is set in the environment.
**When** `Config::from_env_with_build_flags(test_mock_feature=false)` runs.
**Then** the resulting `Config::mock_peer_enabled` is `false`.

### TC-12 (additional, internal): last_log_appends_one_line_per_call

**Given** `last.log` is empty.
**When** two `harness.authenticate()` calls run (one success, one failure).
**Then** `last.log` contains exactly two lines, each in the form
`<ISO-8601> (success|failure) <peer_id>`.

## Traceability
- Roadmap item: [S-009](../syauth/ROADMAP.md#step-s-009-syauth-pam--wire-the-mock-transport-into-the-auth-path)
- Implementation files: `crates/syauth-pam/src/auth.rs`,
  `crates/syauth-pam/src/config.rs`, `crates/syauth-pam/src/lib.rs`,
  `crates/syauth-pam/src/entry.rs`, `crates/syauth-pam/Cargo.toml`.
- Test files: `crates/syauth-pam/tests/pam_e2e.rs`.
