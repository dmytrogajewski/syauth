# ROADMAP: syauth — Phone-as-Key Unlock for Linux

> Source spec: [SPEC.md](./SPEC.md)
> Status: draft v1, all items pending
> Each item is one user journey. Authoring template: `.agents/skills/journey/SKILL.md` → write to `specs/journeys/JOURNEY-{id}.md`.

## How to read this roadmap

- Items are ordered so each one delivers value on its own. Where a strict dependency exists it is named in **DoR**.
- Every item is independently testable. The "Tests" section in each DoD names the failing test that proves completion.
- Item IDs are stable. Do not renumber on insertion; insert with `S-0XX` between existing IDs.
- Each item maps to one journey doc, written before implementation per `AGENTS.md` step 4.

---

## Step S-001: Workspace bootstrap & CI lint pipeline

**Description:** Stand up the Cargo workspace mirroring `~/sources/prrr` (top-level crate + `crates/` directory + `syauth-android/` placeholder), wire `make build/test/lint/fmt/audit`, and confirm an empty `cargo clippy -- -D warnings` is clean. No business code yet — this is the load-bearing scaffolding every other item builds on.

**DoR (Definition of Ready):**
- SPEC.md reviewed and accepted.

**DoD (Definition of Done):**
- [x] `Cargo.toml` declares a workspace with members listed (placeholders allowed: empty `lib.rs` in each crate is fine).
- [x] Directories exist: `crates/syauth-core/`, `crates/syauth-transport/`, `crates/syauth-pam/`, `crates/syauth-cli/`, `crates/syauth-mobile/`, `syauth-android/` (Gradle placeholder).
- [x] Workspace-level `clippy.toml` and `rustfmt.toml` are inherited; existing values from this repo are preserved.
- [x] `make build` produces `target/release/libpam_syauth.so` (even if it exports no PAM symbols yet — proves the `cdylib` crate-type works).
- [x] `make test` runs (passes with zero tests) for the whole workspace.
- [x] `make lint` runs and passes — clippy clean across all crates, fmt clean, `cargo audit` non-fatal.
- [x] `cargo deny check` config exists in `deny.toml` and passes (advisories, bans, licenses).
- [x] A CI workflow file (e.g. `.github/workflows/ci.yml`) runs `make lint` and `make test` on every push.

### Evidence

**Created / modified files (with one-line purpose):**

- `Cargo.toml` — new workspace manifest listing the five `crates/*` members, `[workspace.package]` with edition 2024 and rust-version 1.85, `[workspace.lints.rust] unsafe_code = "deny"`, and a no-API root package that hosts the repo-level `tests/` directory.
- `src/lib.rs` — empty root-package library (doc-comment only) so `cargo test --workspace` discovers `tests/workspace_smoke.rs`.
- `tests/workspace_smoke.rs` — single integration test asserting `1 + 1 == 2`, proving the workspace test harness compiles and runs.
- `crates/syauth-core/{Cargo.toml,src/lib.rs}` — placeholder library crate for S-002..S-006.
- `crates/syauth-transport/{Cargo.toml,src/lib.rs}` — placeholder library crate for S-007/S-010.
- `crates/syauth-pam/{Cargo.toml,src/lib.rs}` — `cdylib + rlib` with `name = "pam_syauth"`, produces `libpam_syauth.so`.
- `crates/syauth-cli/{Cargo.toml,src/main.rs}` — placeholder binary `syauth` for S-011..S-013.
- `crates/syauth-mobile/{Cargo.toml,src/lib.rs}` — placeholder library crate for S-014.
- `syauth-android/{settings.gradle.kts,README.md,app/.gitkeep}` — Gradle placeholder for S-015.
- `deny.toml` — new cargo-deny policy (advisories deny on RustSec DB; permissive OSS license allow-list mirroring prrr's transitive set; bans wildcards; sources restricted to crates.io).
- `.github/workflows/ci.yml` — new GitHub Actions workflow with three jobs (`lint`, `test`, `build`) on every push and pull_request; uses `dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`, and `taiki-e/install-action@v2` to fetch `cargo-audit` and `cargo-deny` as prebuilt binaries.
- `Makefile` — extended (not replaced); `build` now does `cargo build --release --workspace` instead of `--bin syauth`; `test`/`testv`/`bench` are workspace-scoped; new `deny` target; `lint` adds a `cargo deny check` step after `cargo audit`. The promptkit-generated header is preserved verbatim.
- `specs/journeys/JOURNEY-S-001-workspace-bootstrap.md` — full journey doc per `.agents/skills/journey/SKILL.md`.

**Added test files (with what they verify):**

- `tests/workspace_smoke.rs::workspace_test_harness_compiles_and_runs` — verifies that the integration-test harness compiles and runs against the workspace root package, asserting the canonical `1 + 1 == 2` sum via a named `EXPECTED_SUM` constant (per the AGENTS.md "named constants over literals" rule).

**DoD-box ↔ command-output evidence:**

| DoD box | Command run | Observed result |
|---|---|---|
| `Cargo.toml` declares a workspace with members listed | `cargo metadata --no-deps` (implicit via `cargo build --workspace`) | All five members compile in the same `cargo build` invocation. |
| Directories exist | `ls crates/ && ls syauth-android/` | `syauth-cli  syauth-core  syauth-mobile  syauth-pam  syauth-transport` and `app  README.md  settings.gradle.kts`. |
| Workspace-level `clippy.toml` and `rustfmt.toml` inherited; existing values preserved | Files were not modified; `cargo clippy --workspace -- -D warnings` and `cargo fmt --all --check` both exit 0. | Existing thresholds in `clippy.toml` (cognitive-complexity 15, too-many-arguments 7, etc.) and `rustfmt.toml` (edition 2024, max_width 140) are untouched and effective. |
| `make build` produces `target/release/libpam_syauth.so` | `make build && ls -la target/release/libpam_syauth.so` | `-rwxr-xr-x. 2 dmitriy dmitriy 403560 мая 15 00:45 target/release/libpam_syauth.so`. |
| `make test` runs for the whole workspace | `make test` | Exit 0; `tests/workspace_smoke.rs` reports `1 passed; 0 failed`; all other test result blocks report `0 passed; 0 failed`. |
| `make lint` runs and passes — clippy clean, fmt clean, audit non-fatal | `make lint` | Exit 0. Clippy clean across all crates; `cargo fmt --check` clean; `cargo audit` ran (its non-zero exit, if any, would be suppressed by `|| true`); `cargo deny check` reports `advisories ok, bans ok, licenses ok, sources ok`. |
| `cargo deny check` exists and passes | `cargo deny check` | Exit 0. `advisories ok, bans ok, licenses ok, sources ok`. License `unmatched-allowance` warnings are present for future-use entries (`ISC`, `MPL-2.0`, `Zlib`, etc.) and are non-fatal by cargo-deny design. |
| CI workflow runs `make lint` and `make test` on every push | Inspection of `.github/workflows/ci.yml` | `on: push: branches: ["**"]` and `on: pull_request: branches: ["**"]`; the `lint` job runs `make lint` and the `test` job runs `make test`. |

**Deviations from the original DoD:**

- The PAM crate uses `crate-type = ["cdylib", "rlib"]` rather than `["cdylib"]` alone. The `rlib` form is added so that future S-008 tests in this same crate can `use pam_syauth::...` via the unit-test harness — `cdylib`-only would block `cargo test` from linking the crate's own tests. This is exactly how prrr's mobile crate handles the same constraint (`crate-type = ["cdylib", "staticlib", "lib"]`). The DoD requirement that `target/release/libpam_syauth.so` exist is unaffected and verified above.
- A root package (`name = "syauth"`) with an empty `src/lib.rs` is included in `Cargo.toml`. This is necessary because the repo-level `tests/` directory has to belong to a package for `cargo test --workspace` to discover it; a pure `[workspace]` (no `[package]`) Cargo.toml would silently skip `tests/workspace_smoke.rs`. The root package exposes no public API and exists solely as a host for the integration test.

**Tests:**
- `tests/workspace_smoke.rs` — a single integration test that compiles and asserts `1 + 1 == 2`. Proves the test infra works.

**Files likely affected:** `Cargo.toml`, `crates/*/Cargo.toml`, `crates/*/src/lib.rs`, `deny.toml`, `.github/workflows/ci.yml`, `Makefile` (extend).

**Journey:** [`JOURNEY-S-001-workspace-bootstrap.md`](../journeys/JOURNEY-S-001-workspace-bootstrap.md)

---

## Step S-002: `syauth-core` — wire-format framing & parser

**Description:** Implement the v1 protocol frame `[ver:1][nonce:16][payload:N][tag:16]` as a pure-Rust crate with serialization, parsing, and a property-based fuzz target. No crypto yet — the tag is a placeholder all-zero `[u8; 16]`. This is the smallest testable unit of the protocol layer.

**DoR:**
- S-001 complete.

**DoD:**
- [x] `syauth_core::Frame` with `encode(&self, buf: &mut Vec<u8>)` and `decode(input: &[u8]) -> Result<Frame, FrameError>`.
- [x] `FrameError` is a typed enum: `TooShort { needed, got }`, `BadVersion(u8)`, `BadLength`.
- [x] All length and offset literals expressed as named `const`s (per AGENTS.md TDD rules).
- [x] Frames with `ver != 1` are rejected explicitly (do not silently upgrade).
- [x] Round-trip property test with `proptest`: `decode(encode(f)) == f` for any well-formed `Frame`.
- [x] `cargo fuzz` target `frame_parse` builds and runs 10 000 iterations without finding a panic.
- [x] `make lint` clean, ≥95% line coverage on `syauth-core/src/frame.rs` (measured with `cargo tarpaulin`).

### Evidence

**Created / modified files:**
- `crates/syauth-core/Cargo.toml` — adds `thiserror` (prod) and `proptest` (dev) deps.
- `crates/syauth-core/src/lib.rs` — replaces the S-001 placeholder; declares the `frame` module and re-exports the public surface.
- `crates/syauth-core/src/frame.rs` — `Frame`, `FrameError`, all length/offset `const`s, `encode`, `decode`, and the `#[cfg(test)] mod tests` block (15 unit + 3 proptest cases).
- `crates/syauth-core/fuzz/Cargo.toml` — stand-alone (workspace-excluded) `cargo-fuzz` harness manifest.
- `crates/syauth-core/fuzz/fuzz_targets/frame_parse.rs` — libFuzzer target asserting `Frame::decode` never panics.
- `Cargo.toml` (workspace root) — adds `exclude = ["crates/syauth-core/fuzz"]` so the fuzz harness does not pollute the regular workspace build.
- `specs/journeys/JOURNEY-S-002-protocol-framing.md` — journey doc.

**Command outputs:**
- `cargo test -p syauth-core` — 15 unit + 3 proptest cases, all passed.
- `cargo fuzz run frame_parse -- -runs=10000` — `Done 10000 runs in 0 second(s)`, 0 crashes, 0 leaks.
- `cargo tarpaulin --packages syauth-core` — `100.00% coverage, 28/28 lines covered` on `frame.rs`.
- `make lint` — exit 0; `make test` — exit 0.

**Deviations:** None. `MAX_PAYLOAD_LEN = 4096` is a step-local heap bound documented at the const and in the journey doc.

**Tests:**
- `crates/syauth-core/src/frame.rs` `#[cfg(test)] mod tests`.
- `crates/syauth-core/fuzz/fuzz_targets/frame_parse.rs`.

**Files likely affected:** `crates/syauth-core/src/{lib.rs,frame.rs}`, `crates/syauth-core/Cargo.toml`, `fuzz/`.

**Journey:** [`JOURNEY-S-002-protocol-framing.md`](../journeys/JOURNEY-S-002-protocol-framing.md)

---

## Step S-003: `syauth-core` — replay nonce cache

**Description:** Sliding LRU+TTL cache with 64-entry cap and 10 s TTL. Used by `pam_sm_authenticate` to reject replayed responses inside a single boot session.

**DoR:** S-002 complete.

**DoD:**
- [ ] `syauth_core::ReplayCache::new(cap: usize, ttl: Duration)`.
- [ ] `cache.observe(nonce: [u8; 16], now: Instant) -> Acceptance` with variants `Fresh` and `Replayed`.
- [ ] Time is injected via a `now: Instant` parameter — no `Instant::now()` inside the cache, because deterministic time is required by tests.
- [ ] LRU eviction is exercised by a test that inserts `cap + 1` entries and confirms the oldest is gone.
- [ ] TTL expiration is exercised by a test that advances time past TTL and confirms the entry can be re-accepted.
- [ ] All time deltas are `const` durations, not literals.

**Tests:**
- `crates/syauth-core/src/replay.rs` `#[cfg(test)] mod tests`: fresh nonce accepted, exact replay rejected, LRU eviction by capacity, TTL expiration, interleaved fresh + replay.

**Files likely affected:** `crates/syauth-core/src/replay.rs`.

**Journey:** `JOURNEY-{id}-replay-defense.md`

---

## Step S-004: `syauth-core` — Ed25519 signing & verify with constant-time tag

**Description:** Add the crypto layer. The host signs `version || nonce || challenge` with Ed25519 over the bond key. The HMAC tag is computed with BLAKE3-keyed-hash (16-byte output) over the same data. All comparisons use `subtle::ConstantTimeEq`.

**DoR:** S-002 complete.

**DoD:**
- [ ] `syauth_core::sign::sign_frame(privkey: &SigningKey, frame: &Frame) -> Signature`.
- [ ] `syauth_core::sign::verify_frame(pubkey: &VerifyingKey, frame: &Frame, sig: &Signature) -> Result<(), VerifyError>`.
- [ ] `syauth_core::mac::compute_tag(bond_key: &[u8; 32], frame_body: &[u8]) -> [u8; 16]`.
- [ ] `syauth_core::mac::verify_tag(bond_key: &[u8; 32], frame_body: &[u8], tag: &[u8; 16]) -> bool` uses `subtle::ConstantTimeEq`.
- [ ] Known-answer-test (KAT) file `crates/syauth-core/testdata/kat.json` with at least 3 vectors; tests read this file and verify byte-for-byte.
- [ ] Negative tests: bit-flipped tag rejected; bit-flipped signature rejected; wrong pubkey rejected.
- [ ] No `unwrap()` outside `#[cfg(test)]`.
- [ ] `#[deny(unsafe_code)]` at crate root.

**Tests:**
- `crates/syauth-core/src/sign.rs`, `mac.rs` test modules.
- `tests/kat.rs` integration test driving from the JSON file.

**Files likely affected:** `crates/syauth-core/src/{sign.rs,mac.rs}`, `crates/syauth-core/testdata/kat.json`.

**Journey:** `JOURNEY-{id}-crypto-primitives.md`

---

## Step S-005: Bond store — TOML schema + atomic write

**Description:** Persistent bond record at `/var/lib/syauth/bonds.toml` (configurable at compile time, overridable for tests). Records peer pubkey, name, created-at, status (`Bonded` | `Revoked`). Atomic write (`tempfile` + `persist`) so a crash mid-write cannot corrupt.

**DoR:** S-001 complete.

**DoD:**
- [ ] `syauth_core::bond::Bond` and `BondStore` with `load(path)`, `add(bond)`, `remove(peer_id)`, `mark_revoked(peer_id, reason)`, `list() -> &[Bond]`.
- [ ] On `save`, write is atomic via `tempfile::NamedTempFile::persist`. Verified by a test that injects a fault between `write` and `persist` and confirms the original file is unchanged.
- [ ] File mode is `0o600`, parent directory is `0o700`, both verified by an integration test using `std::os::unix::fs::PermissionsExt`.
- [ ] Schema includes `schema_version: u32`; reading a future version returns a typed error, not a panic.
- [ ] `Bond::peer_id` is the BLAKE3 hash of the peer pubkey (16 bytes hex) — stable across reboots, no UUID.

**Tests:**
- `crates/syauth-core/src/bond.rs` test module: add → save → load roundtrip; revoke is persisted; atomic-write fault test; permission test.

**Files likely affected:** `crates/syauth-core/src/bond.rs`.

**Journey:** `JOURNEY-{id}-bond-store.md`

---

## Step S-006: Linux secret storage — kernel keyring with libsecret fallback

**Description:** The host's Ed25519 private key and the per-peer bond keys are stored in the kernel keyring (`linux-keyutils`) with a fallback to `libsecret` via `secret-service`. Read on demand inside `pam_sm_authenticate`.

**DoR:** S-001 complete; S-005 complete (we need the bond IDs to key the secrets).

**DoD:**
- [ ] `syauth_core::secrets::KeyStore` trait with `put(id, secret)`, `get(id) -> Option<Zeroizing<Vec<u8>>>`, `remove(id)`.
- [ ] Two impls: `KernelKeyring` (primary, uses `linux-keyutils`) and `SecretService` (fallback, uses `secret-service` crate).
- [ ] `KeyStore::detect()` factory returns the first working backend at startup; logs which one was selected.
- [ ] All returned secrets are wrapped in `zeroize::Zeroizing<Vec<u8>>` so they are wiped on drop.
- [ ] Integration test against the real kernel keyring (`@u` session keyring) gated on Linux only — no test ever writes to the system keyring.
- [ ] Mock impl `InMemoryKeyStore` for unit tests of upstream code.

**Tests:**
- `crates/syauth-core/src/secrets.rs` test module: roundtrip, missing key, double-put overwrites.
- `tests/keyring_linux.rs` integration test (`#[cfg(target_os = "linux")]`).

**Files likely affected:** `crates/syauth-core/src/secrets.rs`.

**Journey:** `JOURNEY-{id}-secret-storage.md`

---

## Step S-007: `syauth-transport` — `BtPeer` trait + in-process mock

**Description:** Define the trait that decouples the protocol from BlueZ. Ship the mock impl first so the PAM module can be tested end-to-end before any real radio code exists. This is the seam that lets `/pam` and `/bt` evolve independently.

**DoR:** S-002 complete.

**DoD:**
- [ ] `syauth_transport::BtPeer` trait: `connect(timeout) -> Result<Session>`, `Session::send_frame(&Frame)`, `Session::recv_frame(timeout) -> Result<Frame>`.
- [ ] All methods are async (`async-trait`) and return after `timeout` with `TransportError::Timeout`.
- [ ] `MockBtPeer` impl backed by a `tokio::sync::mpsc` channel pair, configurable per test for: golden, offline (returns `Unreachable`), slow (delays beyond timeout), reordered, replay-injected, wrong-version-injected.
- [ ] `MockBtPeer::expect(scenario: MockScenario)` builder; a test that uses `MockScenario::Golden` and asserts a clean roundtrip.
- [ ] Zero dependency on `bluer` in this crate yet — that arrives in S-009.

**Tests:**
- `crates/syauth-transport/src/mock.rs` test module: one test per scenario variant.

**Files likely affected:** `crates/syauth-transport/src/{lib.rs,mock.rs,error.rs}`.

**Journey:** `JOURNEY-{id}-transport-trait.md`

---

## Step S-008: `syauth-pam` — module shell with `catch_unwind` and fail-closed

**Description:** Implement the three required PAM entry points (`pam_sm_authenticate`, `pam_sm_setcred`, `pam_sm_acct_mgmt`) as a `cdylib`. Each wraps its body in `std::panic::catch_unwind` and returns `PAM_AUTH_ERR` on any caught panic. No real authentication yet — it always returns `PAM_AUTHINFO_UNAVAIL`. This proves the module loads, links, and respects the FFI boundary.

**DoR:** S-001 complete.

**DoD:**
- [x] Each entry point is `#[unsafe(no_mangle)] pub unsafe extern "C" fn`.
- [x] `nm -D --defined-only target/release/libpam_syauth.so | grep ' pam_sm_'` shows exactly three symbols, no Rust mangling leaks.
- [x] Every entry point's outermost expression is `catch_unwind(|| { ... }).unwrap_or(PAM_AUTH_ERR)`.
- [x] Logging via `syslog` with tag `pam_syauth`, facility `LOG_AUTHPRIV`. No `println!`/`eprintln!` anywhere in the cdylib.
- [x] Fixture pam.d directory at `tests/pam.d/syauth-test` references the built `.so` by absolute path.
- [x] E2E test `tests/pam_smoke.rs` shells out to `pamtester` (gated on `SYAUTH_E2E=1`) and asserts that `authenticate` returns `PAM_AUTHINFO_UNAVAIL` and the syslog line `syauth: unlock unavailable reason=stub` appears.
- [x] `/ffi` audit on the module passes (every unsafe block has a SAFETY comment; cbindgen header generated and committed if any C-callable surface beyond `pam_sm_*` exists — it should not).

### Evidence

**Created / modified files:**
- `crates/syauth-pam/src/entry.rs` — three `pam_sm_*` extern "C" entry points, named PAM return-code consts, `run_entry` helper wrapping each body in `catch_unwind(AssertUnwindSafe(f))` with `PAM_AUTH_ERR` on caught panic; syslog write via `syslog::unix(...)` with `Formatter3164 { facility: LOG_AUTHPRIV, process: "pam_syauth", ... }`.
- `crates/syauth-pam/src/lib.rs` — crate root with `#![allow(unsafe_code)]` justified for the FFI boundary.
- `crates/syauth-pam/Cargo.toml` — adds `syslog = "7"` dep; preserves `crate-type = ["cdylib", "rlib"]` and `name = "pam_syauth"`.
- `tests/pam_smoke.rs` — pamtester-driven e2e test gated on `SYAUTH_E2E=1` and `which("pamtester")`. Asserts `pamtester ... authenticate` exit string matches `"Authentication service cannot retrieve authentication info"` and journalctl tail contains `"syauth: unlock unavailable reason=stub"`. Skips cleanly on hosts without pamtester.
- `tests/pam.d/syauth-test` — PAM service fixture with `__SYAUTH_SO_PATH__` placeholder; harness substitutes the absolute `target/release/libpam_syauth.so` path at test time.
- `specs/journeys/JOURNEY-S-008-pam-skeleton.md` — journey doc.

**Command outputs:**
- `nm -D --defined-only target/release/libpam_syauth.so | grep ' pam_sm_'` shows exactly three symbols: `pam_sm_acct_mgmt`, `pam_sm_authenticate`, `pam_sm_setcred` — no Rust mangling, no extra C-callable surface.
- `grep -rn "println!\|eprintln!" crates/syauth-pam/src/` — 0 hits (only a doc-comment reference).
- `make build` produces `target/release/libpam_syauth.so`.
- `make lint` — exit 0; `make test` — exit 0 (e2e gated tests skip cleanly on hosts without `pamtester`).

**Deviations:** `tests/pam_smoke.rs` skips when `pamtester` is not on PATH (in addition to the `SYAUTH_E2E=1` gate) so the test stays green on dev machines without pamtester preinstalled; the original gate semantics are preserved.

**Tests:**
- `tests/pam_smoke.rs`.

**Files likely affected:** `crates/syauth-pam/src/{lib.rs,entry.rs}`, `crates/syauth-pam/Cargo.toml` (`crate-type = ["cdylib", "rlib"]`, `name = "pam_syauth"`), `tests/pam.d/syauth-test`.

**Journey:** [`JOURNEY-S-008-pam-skeleton.md`](../journeys/JOURNEY-S-008-pam-skeleton.md)

---

## Step S-009: `syauth-pam` — wire the mock transport into the auth path

**Description:** Glue S-002 → S-007 → S-008. `pam_sm_authenticate` now: opens the bond store, picks the configured peer, drives the mock transport through challenge/response, verifies signature + tag + nonce-freshness, returns the right code. Still no real radio.

**DoR:** S-002, S-003, S-004, S-005, S-006, S-007, S-008 complete.

**DoD:**
- [ ] `pam_sm_authenticate` returns `PAM_SUCCESS` for the golden mock scenario.
- [ ] Returns `PAM_AUTHINFO_UNAVAIL` for the offline scenario, ≤ 1.2 s wall clock.
- [ ] Returns `PAM_AUTH_ERR` for: replay, bad-signature, wrong-version, peer-denied, oversized-frame, MTU-split-with-corrupt-reassembly.
- [ ] `pam_sm_setcred` returns `PAM_SUCCESS` (no creds to set, but the symbol must exist and return success — auth modules MUST implement both per `/pam`).
- [ ] Mock peer is injected via a process-local `OnceLock<Box<dyn BtPeer>>` populated by an env var `SYAUTH_TEST_MOCK=1`; in production builds the env var is ignored.
- [ ] All nine mandatory e2e cases from SPEC §4.3 are encoded in `tests/pam_e2e.rs` and pass under `SYAUTH_E2E=1`.

**Tests:**
- `tests/pam_e2e.rs` — one `#[test]` per scenario in SPEC §4.3.

**Files likely affected:** `crates/syauth-pam/src/{auth.rs,config.rs}`.

**Journey:** `JOURNEY-{id}-pam-mock-e2e.md`

---

## Step S-010: `syauth-transport` — real BLE central via `bluer`

**Description:** Implement `BlueZBtPeer` over `bluer`, advertising a rotating session UUID and acting as the GATT central that the phone connects to. Behind the same `BtPeer` trait — drop-in replacement for the mock.

**DoR:** S-007 complete.

**DoD:**
- [ ] `syauth_transport::bluez::BlueZBtPeer` implements `BtPeer`.
- [ ] Adapter is opened by ID configured in `/etc/syauth.conf` (default: `hci0`); missing adapter → typed error.
- [ ] Rotating session UUID is derived from `HKDF(bond_key, "syauth-session-v1" || timestamp_minute)[0..16]` — same UUID for ~1 minute then rotates, defeating presence tracking.
- [ ] MTU is negotiated; fragmented frames are reassembled correctly (a test injects a 2-segment frame and asserts the upper layer sees one whole frame).
- [ ] Adapter suspend/resume hook: on `org.freedesktop.login1.Manager.PrepareForSleep` true→false, the transport restarts itself. Verified by an integration test that emits the DBus signal manually.
- [ ] `/bt` skill checklist run on this code: explicit `PairingState` is consulted before any unlock-path read.

**Tests:**
- `crates/syauth-transport/src/bluez.rs` unit tests for the parts that don't need a radio (UUID rotation, error mapping).
- `tests/bluer_smoke.rs` integration test gated on `SYAUTH_E2E=1` that uses the BlueZ test virt-controller (`btvirt`) — runs in CI in a container that provides one.

**Files likely affected:** `crates/syauth-transport/src/bluez.rs`.

**Journey:** `JOURNEY-{id}-bluez-transport.md`

---

## Step S-011: `syauth-cli` — `pair` subcommand with LE Secure Connections + app-level OOB

**Description:** Drives the desktop side of pairing per SPEC §4.1 dataflow. Initiates LE Secure Connections via `bluer` with MitM-protection required, then displays the app-level 4-word emoji OOB code derived from `HKDF(bond, "syauth-oob-v1")[0..4]`. On user `[y/N]` confirmation, writes the bond and exits 0.

**DoR:** S-005, S-010 complete.

**DoD:**
- [ ] `syauth pair` prints adapter info, scans for advertising peers, lets user pick (or `--peer <name>` flag), runs LESC numeric comparison, displays the app-level OOB code, waits for `[y/N]`, writes the bond on success.
- [ ] Refuses to pair on adapters that don't advertise the LE Secure Connections bit — error message names the issue. Verified by a test that mocks the adapter capability flag.
- [ ] Pairing UI in the terminal is non-interactive when `--yes` is passed (for tests only).
- [ ] On timeout (default 60 s), state machine transitions `ProvisionalBonded → Revoked`. No partial bond is written.
- [ ] `syauth list` shows the new peer immediately after pairing completes.

**Tests:**
- `crates/syauth-cli/tests/pair_flow.rs` integration test against an injected mock `BtPeer` that emits the LESC simulation events.

**Files likely affected:** `crates/syauth-cli/src/{main.rs,pair.rs,list.rs,oob.rs}`.

**Journey:** `JOURNEY-{id}-pairing-desktop.md`

---

## Step S-012: `syauth-cli` — `list`, `revoke`, `status`

**Description:** The day-2 operations CLI. Cheap to ship, high signal in support.

**DoR:** S-005, S-011 complete.

**DoD:**
- [ ] `syauth list` prints bonds in TSV: `id\tname\tstatus\tcreated_at`. Hidden by default if no bonds; emits an empty table with a hint instead of erroring.
- [ ] `syauth revoke <id>` marks the bond as revoked, exits 0; idempotent (revoke twice = same result, exit 0).
- [ ] `syauth status` prints: adapter state, advertising state, bond count, last unlock outcome (read from a small rolling log under `/var/lib/syauth/last.log`), and whether CDM is observing — wait, CDM is phone-side; for now just print adapter+bonds+last unlock.
- [ ] `syauth --version` and `syauth --help` work and exit 0.
- [ ] Snapshot test on `--help` output (so `clap` regressions are caught).

**Tests:**
- `crates/syauth-cli/tests/cli.rs` using `assert_cmd`.

**Files likely affected:** `crates/syauth-cli/src/{list.rs,revoke.rs,status.rs}`.

**Journey:** `JOURNEY-{id}-day2-cli.md`

---

## Step S-013: `syauth-cli` — `install-pam` / `uninstall-pam` with atomic edit

**Description:** Eliminates the worst foot-gun in syauth: hand-editing `/etc/pam.d/*`. The subcommand reads the existing service file, inserts a syauth line at the documented position, atomically rewrites the file (via `tempfile::persist`), and saves a `.bak` next to it. Uninstall reverses the operation by reading the bak.

**DoR:** S-008 complete.

**DoD:**
- [ ] `syauth install-pam --service sudo` is idempotent: running it twice produces the same file.
- [ ] The inserted line is `auth required pam_syauth.so timeout=1200` by default, configurable via `--module-args`.
- [ ] A `.bak` is always written if not already present. Refuses to overwrite an existing `.bak`.
- [ ] `syauth uninstall-pam --service sudo` restores from `.bak` and removes the bak file on success.
- [ ] If the target file does not contain a recognizable syauth line, uninstall is a no-op (exit 0 with a warning) — never deletes a backup it doesn't own.
- [ ] Hermetic test: takes a fixture `/etc/pam.d/sudo`, runs install, asserts diff; runs uninstall, asserts byte-equality with the original.

**Tests:**
- `crates/syauth-cli/tests/install_pam.rs`.

**Files likely affected:** `crates/syauth-cli/src/{install_pam.rs,uninstall_pam.rs}`.

**Journey:** `JOURNEY-{id}-pam-install-helper.md`

---

## Step S-014: `syauth-mobile` — UniFFI surface

**Description:** Mirror `prrr-mobile` exactly. Define `mobile.udl` exporting just the surface the Android app needs: `parse_invite_uri`, `verify_challenge_frame`, `sign_challenge_response`, `oob_code_for_bond`. The crate is a thin re-export of `syauth-core`.

**DoR:** S-002 through S-006 complete.

**DoD:**
- [ ] `crates/syauth-mobile/Cargo.toml` has `crate-type = ["cdylib", "staticlib", "lib"]` and `[build-dependencies] uniffi = { version = "0.29", features = ["build"] }` matching prrr-mobile.
- [ ] `crates/syauth-mobile/src/mobile.udl` defines the four functions and the error enum.
- [ ] `make android-aar` produces `crates/syauth-mobile/target/syauth_mobile.aar` containing the .so for `aarch64-linux-android` and `armv7-linux-androideabi`, plus generated Kotlin bindings under `bindings/kotlin/`.
- [ ] Mobile-side unit test (Rust): every UDL-exported function has at least one happy-path and one negative-path test.
- [ ] No `unsafe` outside the UniFFI-generated code; the crate-level `[lints.rust] unsafe_code = "deny"` exception is documented (UniFFI generates `unsafe`, same as prrr-mobile).

**Tests:**
- `crates/syauth-mobile/src/lib.rs` test module.
- A smoke test in `examples/` that drives the public surface from outside the crate.

**Files likely affected:** `crates/syauth-mobile/`, `scripts/build_aar.sh`.

**Journey:** `JOURNEY-{id}-mobile-uniffi-surface.md`

---

## Step S-015: Android scaffold — Gradle + Compose + hello-world consuming the AAR

**Description:** Bootstrap `syauth-android/` by copying the structure of `~/sources/prrr/prrr-android/`: same `build.gradle.kts` layout, same minSdk 26 / targetSdk 34 / JVM 17 / Kotlin 1.9, JNA dependency, and a single Compose screen that calls `oobCodeForBond(...)` from the AAR and renders the result. No BT yet — proves the toolchain.

**DoR:** S-014 complete.

**DoD:**
- [ ] `syauth-android/app/build.gradle.kts` mirrors `prrr-android/app/build.gradle.kts` line-for-line where applicable, with the package name `com.sy.syauth.android`.
- [ ] `./gradlew :app:assembleDebug` produces a `.apk` of < 10 MB.
- [ ] Instrumented test on an emulator: launch app, assert "OOB: …" string is rendered (proves the Rust call through UniFFI/JNA actually executed).
- [ ] No hand-written JNI in the codebase. Every Rust call goes through the UniFFI-generated Kotlin.
- [ ] `make android-test` runs `./gradlew :app:connectedAndroidTest` against a headless emulator.

**Tests:**
- `syauth-android/app/src/androidTest/.../HelloWorldTest.kt`.

**Files likely affected:** `syauth-android/{settings.gradle.kts,build.gradle.kts,app/build.gradle.kts,app/src/main/kotlin/…}`.

**Journey:** `JOURNEY-{id}-android-scaffold.md`

---

## Step S-016: Android — Pairing screen with LE Secure Connections + OOB confirm

**Description:** First production-shaped screen on the phone. UX (Compose): big "Pair with computer" CTA → BluetoothLE scan picker → trigger LESC bond → display 6-digit BT code → after BT pair, display the 4-word emoji OOB code → "These match the computer? [Yes] [No]". On Yes, persist the bond via UniFFI and route to the home screen.

**DoR:** S-014, S-015 complete. (Desktop S-011 not strictly required, but having it makes manual testing trivial.)

**DoD:**
- [ ] Pairing screen renders in Compose; states are `Idle`, `Scanning`, `LescNegotiating(code: String)`, `OobConfirming(emoji: List<String>)`, `Bonded(name: String)`, `Failed(reason: String)`.
- [ ] OOB code is computed via the UniFFI surface (`oobCodeForBond`) — never reimplemented in Kotlin.
- [ ] On `Failed`, the bond is not persisted on either side. The Bluetooth bond is also removed (`BluetoothDevice.removeBond()` via reflection — Android does not expose this in the public SDK; document the reflection).
- [ ] Robolectric unit tests for the state-machine transitions; Compose UI test for the rendering of each state.
- [ ] Refuses to advance past `Scanning` when the adapter doesn't support LE Secure Connections — error includes the adapter name.

**Tests:**
- `syauth-android/app/src/test/.../PairingViewModelTest.kt`.
- `syauth-android/app/src/androidTest/.../PairingScreenTest.kt`.

**Files likely affected:** `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/{PairingViewModel.kt,PairingScreen.kt}`.

**Journey:** `JOURNEY-{id}-android-pairing.md`

---

## Step S-017: Android — Approve screen + BiometricPrompt + Keystore signer

**Description:** The screen the user sees on every unlock. Surface "Approve unlock for `hostname`?" with two buttons. Tapping Approve triggers `BiometricPrompt`; on success the Keystore releases the Ed25519 signing key (with `setUserAuthenticationRequired(true)`) for one signature, the response frame is sent. On Deny or timeout, the screen closes.

**DoR:** S-014 complete.

**DoD:**
- [ ] Compose screen shows `hostname`, app icon, Approve/Deny buttons, and a countdown (default 30 s).
- [ ] Signing key is generated in `KeyProperties.KEY_ALGORITHM_EC` (curve `secp256r1` if Keystore lacks Ed25519 — fall back per device; document in `docs/android-setup.md`) with `setUserAuthenticationRequired(true)` and `setUnlockedDeviceRequired(true)`. Strongbox if available.
- [ ] BiometricPrompt with `BIOMETRIC_STRONG | DEVICE_CREDENTIAL`.
- [ ] Signing happens via the UniFFI surface, which takes a raw signature blob from the Keystore-backed `Signature` object. The crypto code never sees the private key bytes.
- [ ] Cancel on countdown is logged as a denial (not a timeout from the desktop's perspective — the desktop sees a `PeerDenied` frame).
- [ ] Robolectric tests for the timeout, Approve, Deny branches.

**Tests:**
- `syauth-android/app/src/test/.../ApproveViewModelTest.kt`.
- `syauth-android/app/src/androidTest/.../ApproveScreenTest.kt`.

**Files likely affected:** `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/{ApproveViewModel.kt,ApproveScreen.kt,KeystoreSigner.kt}`.

**Journey:** `JOURNEY-{id}-android-approve.md`

---

## Step S-018: Android — `CompanionDeviceService` + foreground BLE bridge

**Description:** The lifecycle wiring that lets the app receive challenges in the background. Register the bonded computer with `CompanionDeviceManager.associate()` at pairing time; in v0.1 use `CompanionDeviceService` + `REQUEST_COMPANION_RUN_IN_BACKGROUND`. When the system binds the service (peer in BLE range), open the GATT server and listen for challenges; on receive, raise a high-importance notification that launches the Approve screen.

**DoR:** S-016, S-017 complete.

**DoD:**
- [ ] `CompanionDeviceService` subclass declared in the manifest with the right permissions (`REQUEST_COMPANION_RUN_IN_BACKGROUND`, `BLUETOOTH_CONNECT`, `BLUETOOTH_SCAN`, `POST_NOTIFICATIONS`).
- [ ] Association is requested at pairing completion (S-016) — without this the OS will not bind the service.
- [ ] `onDeviceAppeared` opens the GATT server and starts listening; `onDeviceDisappeared` shuts it down.
- [ ] Receiving a valid challenge frame raises a `Notification` with `NotificationManager.IMPORTANCE_HIGH`; tapping launches Approve screen with the challenge as an intent extra.
- [ ] Documented setup step in `docs/android-setup.md`: "Disable battery optimization for syauth" — with a deep-link the app can pop on first launch.
- [ ] Instrumented test verifies the service-binding lifecycle with a fake CDM event (using `androidx.test.companion`).

**Tests:**
- `syauth-android/app/src/androidTest/.../CdmLifecycleTest.kt`.

**Files likely affected:** `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/{SyauthCompanionService.kt,GattServer.kt,ApproveNotification.kt}`, manifest.

**Journey:** `JOURNEY-{id}-android-background-bridge.md`

---

## Step S-019: Full e2e on real radios

**Description:** Replace the mock transport in `tests/pam_e2e.rs` with the real `bluer` impl and run the nine SPEC §4.3 cases against a real Android emulator (or a physical Pixel in `--ci-rack` mode). This is the test that proves syauth actually works.

**DoR:** S-009, S-010, S-018 complete.

**DoD:**
- [ ] CI job `make e2e-real` runs the suite headlessly using an Android emulator with the syauth APK preinstalled and pre-bonded via a scripted pairing.
- [ ] All nine cases pass: golden, offline, slow, replay, bad-sig, wrong-version, revoked, MTU-split, panic-in-core.
- [ ] Golden case wall-clock < 2.0 s p95 across 100 runs; record the histogram in `docs/perf-baselines.md`.
- [ ] Offline case ≤ 1.2 s p99.
- [ ] `make e2e-real` is gated on `SYAUTH_E2E_REAL=1` and skipped by default in `make test`.
- [ ] Flake budget: 0. If a case flakes once, file a bug via `/bug` and either fix or quarantine before merge.

**Tests:**
- `tests/e2e_real.rs`.

**Files likely affected:** `tests/e2e_real.rs`, `scripts/e2e-emulator-up.sh`, `Makefile` (new target).

**Journey:** `JOURNEY-{id}-e2e-real-radios.md`

---

## Step S-020: Threat-model close-out + `/threat` artifact

**Description:** Run `/threat`, produce `specs/threat/THREAT-{datetime}.md`, and resolve every open finding either by (a) shipping the fix as a follow-up step in this roadmap or (b) explicitly marking it `accepted-residual` in the threat document with a rationale.

**DoR:** S-019 complete (because the threat model needs to look at real wire behavior, not just the spec).

**DoD:**
- [ ] `specs/threat/THREAT-{datetime}.md` exists with all sections per `/threat` Phase 7.
- [ ] Every one of the ten canonical abuse paths (§4 of `/threat`) is either mitigated or accepted-residual with rationale.
- [ ] Every open finding maps to either an existing roadmap item (link by ID) or a failing test that lands in this step.
- [ ] `docs/security.md` written for end-users (what syauth protects vs. doesn't).

**Tests:**
- Tests added by this step land alongside the affected code; no new test file is required by the step itself.

**Files likely affected:** `specs/threat/THREAT-{datetime}.md`, `docs/security.md`, plus per-finding fixes.

**Journey:** `JOURNEY-{id}-threat-closeout.md`

---

## Step S-021: Packaging — Fedora RPM, Debian deb, signed APK release

**Description:** v0.1.0 is shippable only when a user can install it from a single command on each supported platform.

**DoR:** S-019, S-020 complete.

**DoD:**
- [ ] `deploy/fedora/syauth.spec` builds a working RPM under `mock` for Fedora 39+; `dnf install ./syauth-0.1.0-1.fc39.x86_64.rpm` succeeds; `syauth --version` works.
- [ ] `deploy/debian/` builds a `.deb` under `pbuilder` for Debian 12 / Ubuntu 22.04; same install smoke test.
- [ ] `make release-apk` produces a signed `syauth-0.1.0.apk`; signature is verified by `apksigner verify`.
- [ ] GitHub Releases workflow attaches RPM, deb, and APK to the v0.1.0 tag (CI uploads, not the developer).
- [ ] `README.md` quick-start covers the three install paths.
- [ ] F-Droid submission opened (link tracked in `docs/release-process.md`); not blocking for v0.1.

**Tests:**
- `scripts/smoke-install.sh` runs the install + `syauth --version` in a docker-based clean Fedora and Debian.

**Files likely affected:** `deploy/`, `.github/workflows/release.yml`, `Makefile`, `README.md`.

**Journey:** `JOURNEY-{id}-v0_1_release.md`

---

## Dependency graph

```
S-001 ──┬─▶ S-002 ──┬─▶ S-003
        │           ├─▶ S-004 ──┐
        │           │           │
        ├─▶ S-005 ──┼───────────┤
        │           │           ▼
        ├─▶ S-006 ──┼─▶ S-007 ──▶ S-008 ──▶ S-009 ──┐
        │           │                                │
        │           ▼                                ▼
        │       S-010 ─────────────────────────────▶ S-019
        │                                            ▲
        ├─▶ S-011 ──▶ S-012                          │
        ├─▶ S-013                                    │
        │                                            │
        └─▶ S-014 ──▶ S-015 ──┬─▶ S-016 ──┐          │
                              ├─▶ S-017 ──┤          │
                              │           ▼          │
                              └────────▶ S-018 ──────┘
                                                     │
                                              ▼
                                          S-020 ──▶ S-021
```

The critical path to a useful release is **S-001 → S-002…S-009 → S-010 → S-018 → S-019 → S-020 → S-021**. S-011/S-012/S-013 are usable in isolation once their deps are met and can be done in parallel with the Android track.

## Out-of-roadmap (v0.2 candidates)

These are explicitly **not** part of v0.1 per SPEC §3.3; tracked here so they aren't lost:

- UWB / Wi-Fi RTT secure-ranging as optional second factor.
- LAN/mDNS fallback transport (`syauth-transport::lan`).
- Multi-peer with bounded racing.
- iOS companion app.
- F-Droid publication (submitted in S-021 but not part of the gate).
- Lockscreen / GDM integration polish.

## Changelog

- 2026-05-14: Initial roadmap drafted from SPEC.md by `/roadmap`. All items pending.
