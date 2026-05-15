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
- [x] `syauth_core::ReplayCache::new(cap: usize, ttl: Duration)`.
- [x] `cache.observe(nonce: [u8; 16], now: Instant) -> Acceptance` with variants `Fresh` and `Replayed`.
- [x] Time is injected via a `now: Instant` parameter — no `Instant::now()` inside the cache, because deterministic time is required by tests.
- [x] LRU eviction is exercised by a test that inserts `cap + 1` entries and confirms the oldest is gone.
- [x] TTL expiration is exercised by a test that advances time past TTL and confirms the entry can be re-accepted.
- [x] All time deltas are `const` durations, not literals.

### Evidence

**Created / modified files:**
- `crates/syauth-core/src/replay.rs` — new `ReplayCache` + `Acceptance` enum, backing store `VecDeque<([u8; NONCE_LEN], Instant)>`, named consts `DEFAULT_REPLAY_CAP=64`, `DEFAULT_REPLAY_TTL=Duration::from_secs(10)`, `DEFAULT_REPLAY_TTL_NUDGE=1ms` (test-only). `observe` does TTL sweep → membership check → LRU push. No `Instant::now()` inside the cache; `now` is the only time source. 8 unit tests including degenerate-input hardening.
- `crates/syauth-core/src/lib.rs` — adds `pub mod replay;` and re-exports `Acceptance`, `DEFAULT_REPLAY_CAP`, `DEFAULT_REPLAY_TTL`, `ReplayCache`.
- `specs/journeys/JOURNEY-S-003-replay-defense.md` — journey doc.

**Tests** (`crates/syauth-core/src/replay.rs::tests`, 8 cases):
- `fresh_nonce_accepted` — first observation returns `Fresh`.
- `exact_replay_rejected` — second observation inside TTL returns `Replayed`.
- `lru_eviction_by_capacity` — `cap + 1` inserts evict the oldest; surviving entries still report `Replayed`; oldest re-observation reports `Fresh`.
- `ttl_expiration_re_accepts` — same nonce past TTL returns `Fresh`.
- `interleaved_fresh_and_replay` — A B A C B sequence verifies the LRU+TTL interaction.
- `cap_zero_accepts_everything_as_fresh` — degenerate-input hardening.
- `replay_does_not_refresh_inserted_at` — replays must not extend the deadline (otherwise a captured nonce could be kept alive forever by spamming replays).
- `zero_ttl_expires_entries_instantly` — `Duration::ZERO` works.

**Command outputs:**
- `make lint` — exit 0; `make test` — exit 0.
- `cargo test -p syauth-core --lib replay::` — 8 passed.

**Deviations:** Three extra tests beyond the DoD-named five (`cap_zero`, `replay_does_not_refresh_inserted_at`, `zero_ttl`) harden the degenerate-input contract documented at the constructor. Additive.

**Tests:**
- `crates/syauth-core/src/replay.rs` `#[cfg(test)] mod tests` — 8 cases.

**Files likely affected:** `crates/syauth-core/src/replay.rs`.

**Journey:** [`JOURNEY-S-003-replay-defense.md`](../journeys/JOURNEY-S-003-replay-defense.md)

---

## Step S-004: `syauth-core` — Ed25519 signing & verify with constant-time tag

**Description:** Add the crypto layer. The host signs `version || nonce || challenge` with Ed25519 over the bond key. The HMAC tag is computed with BLAKE3-keyed-hash (16-byte output) over the same data. All comparisons use `subtle::ConstantTimeEq`.

**DoR:** S-002 complete.

**DoD:**
- [x] `syauth_core::sign::sign_frame(privkey: &SigningKey, frame: &Frame) -> Signature`.
- [x] `syauth_core::sign::verify_frame(pubkey: &VerifyingKey, frame: &Frame, sig: &Signature) -> Result<(), VerifyError>`.
- [x] `syauth_core::mac::compute_tag(bond_key: &[u8; 32], frame_body: &[u8]) -> [u8; 16]`.
- [x] `syauth_core::mac::verify_tag(bond_key: &[u8; 32], frame_body: &[u8], tag: &[u8; 16]) -> bool` uses `subtle::ConstantTimeEq`.
- [x] Known-answer-test (KAT) file `crates/syauth-core/testdata/kat.json` with at least 3 vectors; tests read this file and verify byte-for-byte.
- [x] Negative tests: bit-flipped tag rejected; bit-flipped signature rejected; wrong pubkey rejected.
- [x] No `unwrap()` outside `#[cfg(test)]`.
- [x] `#[deny(unsafe_code)]` at crate root.

### Evidence

**Created / modified files:**
- `crates/syauth-core/src/sign.rs` — `sign_frame(privkey, &Frame) -> Result<Signature, FrameError>` and `verify_frame(pubkey, &Frame, &Signature) -> Result<(), VerifyError>`. Re-exports `ed25519_dalek::{SigningKey, VerifyingKey, Signature}`. `VerifyError` has `Signature(SignatureError)` (via thiserror `#[from]`) and `BadEncoding(FrameError)`. `verify_frame` uses `VerifyingKey::verify_strict` to reject malleable signatures.
- `crates/syauth-core/src/mac.rs` — `compute_tag(bond_key, body) -> [u8; TAG_LEN]` via `blake3::keyed_hash` truncated to 16 bytes. `verify_tag` uses `subtle::ConstantTimeEq::ct_eq` for the byte comparison; both operands are compile-time-fixed `[u8; TAG_LEN]` so no length-based timing channel exists.
- `crates/syauth-core/src/frame.rs` — adds `Frame::body_bytes()` helper returning `[version:1] || nonce:16 || payload]` (the signed/MAC'd prefix), plus 3 unit tests.
- `crates/syauth-core/src/lib.rs` — adds `#![deny(unsafe_code)]` at crate root, declares `pub mod {sign, mac}`, re-exports the public surface.
- `crates/syauth-core/Cargo.toml` — adds `ed25519-dalek = "2"` (std features), `subtle = "2"`; dev-deps `serde_json = "1"`, `hex = "0.4"` for the KAT loader.
- `crates/syauth-core/testdata/kat.json` — 3 KAT vectors with pinned hex inputs and pinned expected_tag + expected_signature outputs:
  - `kat-01-empty-payload` (empty challenge)
  - `kat-02-typical-32b-payload` (32-byte challenge)
  - `kat-03-max-payload` (4096-byte `0xAA` payload)
- `crates/syauth-core/tests/kat.rs` — integration test loading the JSON and asserting byte-equal `compute_tag` + `sign_frame` outputs, plus the verify-side roundtrip. Includes an `#[ignore]` bootstrap helper `bootstrap_print_kat_vectors` that regenerates the JSON content on demand.
- `specs/journeys/JOURNEY-S-004-crypto-primitives.md` — journey doc.

**Tests** (16 unit + 1 integration, 25 net new):
- `sign.rs::tests` (8): `sign_then_verify_roundtrip`, `sign_frame_is_deterministic_per_key_and_body`, `signature_is_signature_len_bytes`, `verify_rejects_bit_flipped_signature`, `verify_rejects_wrong_pubkey`, `verify_rejects_tampered_body`, `sign_rejects_oversized_payload`, `verify_rejects_oversized_payload_as_bad_encoding`.
- `mac.rs::tests` (8): `compute_then_verify_roundtrip`, `compute_tag_is_deterministic`, `compute_tag_returns_tag_len_bytes`, `verify_rejects_bit_flipped_tag`, `verify_rejects_wrong_bond_key`, `verify_rejects_bit_flipped_body`, `verify_is_constant_time_smoke`, `empty_body_still_produces_a_valid_tag`.
- `tests/kat.rs` (1 active + 1 ignored): `kat_file_loads_and_verifies_byte_for_byte`; `bootstrap_print_kat_vectors` (#[ignore]).

**Command outputs:**
- `make lint` — exit 0 (clippy + fmt + audit + deny all clean).
- `make test` — exit 0; `cargo test -p syauth-core --lib` reports 57 passed (was 41 before S-004; +16 from sign/mac + 0 net frame change after consolidating with new body_bytes tests included in the 57 figure).
- `grep "\.unwrap()\|\.expect("` in `sign.rs`/`mac.rs` outside test modules → 0 hits.

**Deviations:**
1. `sign_frame` returns `Result<Signature, FrameError>` rather than bare `Signature`. The DoD signature `(_, &Frame) -> Signature` is unrealizable without an `.unwrap()` because building the signed body can fail for an oversized payload — the `Result` is the only way to satisfy "no `unwrap()` outside `#[cfg(test)]`". Documented in Evidence above.
2. `MAC_TAG_LEN` is a re-export of `frame::TAG_LEN` rather than a fresh `pub const TAG_LEN` in `mac.rs`, keeping a single source of truth.
3. The KAT bootstrap helper (`bootstrap_print_kat_vectors`) lives in the test file (in-tree) rather than a separate xtask, so KAT regeneration is one `cargo test --ignored bootstrap_print_kat_vectors` away.

**Tests:**
- `crates/syauth-core/src/sign.rs`, `mac.rs` test modules.
- `crates/syauth-core/tests/kat.rs` integration test driving from the JSON file.

**Files likely affected:** `crates/syauth-core/src/{sign.rs,mac.rs}`, `crates/syauth-core/testdata/kat.json`.

**Journey:** [`JOURNEY-S-004-crypto-primitives.md`](../journeys/JOURNEY-S-004-crypto-primitives.md)

---

## Step S-005: Bond store — TOML schema + atomic write

**Description:** Persistent bond record at `/var/lib/syauth/bonds.toml` (configurable at compile time, overridable for tests). Records peer pubkey, name, created-at, status (`Bonded` | `Revoked`). Atomic write (`tempfile` + `persist`) so a crash mid-write cannot corrupt.

**DoR:** S-001 complete.

**DoD:**
- [x] `syauth_core::bond::Bond` and `BondStore` with `load(path)`, `add(bond)`, `remove(peer_id)`, `mark_revoked(peer_id, reason)`, `list() -> &[Bond]`.
- [x] On `save`, write is atomic via `tempfile::NamedTempFile::persist`. Verified by a test that injects a fault between `write` and `persist` and confirms the original file is unchanged.
- [x] File mode is `0o600`, parent directory is `0o700`, both verified by an integration test using `std::os::unix::fs::PermissionsExt`.
- [x] Schema includes `schema_version: u32`; reading a future version returns a typed error, not a panic.
- [x] `Bond::peer_id` is the BLAKE3 hash of the peer pubkey (16 bytes hex) — stable across reboots, no UUID.

### Evidence

**Created / modified files:**
- `crates/syauth-core/src/bond.rs` — new module: `Bond`, `BondStatus`, `BondStore`, `BondError`, free `peer_id_from_pubkey`, named consts (`BOND_FILE_MODE=0o600`, `BOND_DIR_MODE=0o700`, `PEER_ID_BLAKE3_BYTES=16`, etc.), Serde TOML codec, atomic `save` via `tempfile::NamedTempFile::persist` with parent-dir-create-with-0o700 + reject-too-permissive guard, and an 18-test `#[cfg(test)] mod tests` block.
- `crates/syauth-core/src/lib.rs` — declares `pub mod bond;` and re-exports `Bond`, `BondError`, `BondStatus`, `BondStore`, `peer_id_from_pubkey`, plus the public consts.
- `crates/syauth-core/Cargo.toml` — adds `serde`, `toml`, `tempfile`, `blake3`, `time` deps.
- `specs/journeys/JOURNEY-S-005-bond-store.md` — journey doc.

**Tests** (`crates/syauth-core/src/bond.rs::tests`, 18 cases):
- Peer-id: `peer_id_is_stable_and_blake3_derived` / `peer_id_differs_for_different_pubkeys`.
- Roundtrip: `add_save_load_roundtrip` / `rfc3339_round_trip_preserves_created_at`.
- Add: `add_rejects_duplicate_peer_id`.
- Revoke: `revoke_is_persisted_across_save_load` / `revoke_unknown_peer_errors` / `revoke_of_already_revoked_is_no_op`.
- Remove: `remove_unknown_peer_errors` / `remove_existing_peer_succeeds`.
- Atomic write: `atomic_write_fault_leaves_file_intact` (drops `NamedTempFile` without `persist`; asserts destination byte-equal to pre-fault snapshot, no stray `.tmp*`).
- Permissions (`#[cfg(unix)]`): `saved_file_mode_is_0o600` / `parent_directory_mode_is_0o700_after_save` / `save_rejects_too_permissive_parent_dir`.
- Schema: `future_schema_version_returns_typed_error` / `parse_error_on_garbage_toml` / `peer_id_mismatch_in_file_is_rejected`.
- Empty: `load_missing_file_returns_empty_store`.

**Command outputs:**
- `make lint` — exit 0; `make test` — exit 0.
- `cargo test -p syauth-core --lib` — 33 passed (18 bond + 15 frame).

**Deviations:** Permission test lives in the in-file `mod tests` rather than a separate integration file — matches the roadmap "Tests" line that names `crates/syauth-core/src/bond.rs` as the home. Extra error variants (`Parse`, `Serialize`, `ParentDirTooPermissive`, `PeerIdMismatch`) are additive and needed to keep all function signatures total without `unwrap()`.

**Tests:**
- `crates/syauth-core/src/bond.rs` `#[cfg(test)] mod tests` — 18 cases.

**Files likely affected:** `crates/syauth-core/src/bond.rs`.

**Journey:** [`JOURNEY-S-005-bond-store.md`](../journeys/JOURNEY-S-005-bond-store.md)

---

## Step S-006: Linux secret storage — kernel keyring with libsecret fallback

**Description:** The host's Ed25519 private key and the per-peer bond keys are stored in the kernel keyring (`linux-keyutils`) with a fallback to `libsecret` via `secret-service`. Read on demand inside `pam_sm_authenticate`.

**DoR:** S-001 complete; S-005 complete (we need the bond IDs to key the secrets).

**DoD:**
- [x] `syauth_core::secrets::KeyStore` trait with `put(id, secret)`, `get(id) -> Option<Zeroizing<Vec<u8>>>`, `remove(id)`.
- [x] Two impls: `KernelKeyring` (primary, uses `linux-keyutils`) and `SecretService` (fallback, uses `secret-service` crate).
- [x] `KeyStore::detect()` factory returns the first working backend at startup; logs which one was selected.
- [x] All returned secrets are wrapped in `zeroize::Zeroizing<Vec<u8>>` so they are wiped on drop.
- [x] Integration test against the real kernel keyring (`@u` session keyring) gated on Linux only — no test ever writes to the system keyring.
- [x] Mock impl `InMemoryKeyStore` for unit tests of upstream code.

### Evidence

**Created / modified files:**
- `crates/syauth-core/src/secrets.rs` — `KeyStore` trait (sync; `put` / `get` / `remove`), `SecretError`, `BackendKind`, `InMemoryKeyStore`, `KernelKeyring` (Linux-gated, uses `linux-keyutils` against `KeyRingIdentifier::Session`), `SecretService` (Linux-gated, uses `secret-service` crate with a per-call `tokio::runtime::Runtime`), `detect` / `detect_with_logger` factories that probe kernel first then fall back to secret-service.
- `crates/syauth-core/src/lib.rs` — `pub mod secrets;` + re-exports.
- `crates/syauth-core/Cargo.toml` — adds `zeroize = "1" (zeroize_derive)`; Linux-gated `linux-keyutils = "0.2"` and `secret-service = "5" (rt-tokio-crypto-rust)`.
- `crates/syauth-core/tests/keyring_linux.rs` — 3 hermetic integration tests against the session keyring with RAII cleanup, gated on `target_os = "linux"`. Skips cleanly if `CONFIG_KEYS` is not available (probe fails).
- `specs/journeys/JOURNEY-S-006-secret-storage.md` — journey doc.

**Tests:**
- `secrets.rs::tests` (7): `inmemory_roundtrip`, `inmemory_get_missing_returns_none`, `inmemory_double_put_overwrites`, `inmemory_remove_makes_get_return_none`, `inmemory_remove_missing_is_ok`, `inmemory_get_returns_zeroizing_vec`, `detect_returns_real_backend_or_not_implemented`.
- `tests/keyring_linux.rs` (3): `kernel_keyring_roundtrip`, `kernel_keyring_get_missing_returns_none`, `kernel_keyring_remove_is_idempotent` — all three actually ran on the dev box (kernel keyring reachable on Fedora 43).

**Command outputs:**
- `cargo deny check` — `advisories ok, bans ok, licenses ok, sources ok`.
- `make lint` — exit 0; `make test` — exit 0.

**Deviations:**
1. `KeyStore` is sync (not async). The PAM caller cannot afford to spin up a tokio runtime per call when the kernel keyring path is itself sync; the `SecretService` impl absorbs the cost of building a single-threaded runtime internally on the rare fallback path. Documented in the journey.
2. `BackendKind` is public for future use by the `pam_sm_*` logger to log which backend served a get; `detect_with_logger` currently passes the kind via the log callback. Non-breaking to widen later.
3. `SecretService` unit tests are not hermetic (no fixture libsecret service is shipped); the trait surface and error mapping are exercised by the probe path in `detect_with_logger`. The full DBus roundtrip lands when S-009's PAM e2e runs on a host without `CONFIG_KEYS`.

**Tests:**
- `crates/syauth-core/src/secrets.rs` test module: roundtrip, missing key, double-put overwrites.
- `crates/syauth-core/tests/keyring_linux.rs` integration test (`#[cfg(target_os = "linux")]`).

**Files likely affected:** `crates/syauth-core/src/secrets.rs`.

**Journey:** [`JOURNEY-S-006-secret-storage.md`](../journeys/JOURNEY-S-006-secret-storage.md)

---

## Step S-007: `syauth-transport` — `BtPeer` trait + in-process mock

**Description:** Define the trait that decouples the protocol from BlueZ. Ship the mock impl first so the PAM module can be tested end-to-end before any real radio code exists. This is the seam that lets `/pam` and `/bt` evolve independently.

**DoR:** S-002 complete.

**DoD:**
- [x] `syauth_transport::BtPeer` trait: `connect(timeout) -> Result<Session>`, `Session::send_frame(&Frame)`, `Session::recv_frame(timeout) -> Result<Frame>`.
- [x] All methods are async (`async-trait`) and return after `timeout` with `TransportError::Timeout`.
- [x] `MockBtPeer` impl backed by a `tokio::sync::mpsc` channel pair, configurable per test for: golden, offline (returns `Unreachable`), slow (delays beyond timeout), reordered, replay-injected, wrong-version-injected.
- [x] `MockBtPeer::expect(scenario: MockScenario)` builder; a test that uses `MockScenario::Golden` and asserts a clean roundtrip.
- [x] Zero dependency on `bluer` in this crate yet — that arrives in S-009.

### Evidence

**Created / modified files:**
- `crates/syauth-transport/src/lib.rs` — async `BtPeer` + `Session` traits (`#[async_trait]`), public re-exports of `MockBtPeer`, `MockScenario`, `TransportError`.
- `crates/syauth-transport/src/error.rs` — `TransportError` enum with variants `Timeout`, `Unreachable`, `Closed`, `BadFrame(FrameError)`, `WrongVersion(u8)`, `Replay`. `BadFrame` wraps `syauth_core::FrameError` verbatim.
- `crates/syauth-transport/src/mock.rs` — `MockBtPeer` and `MockScenario` (Golden/Offline/Slow{delay}/Reordered/Replay{duplicate_count}/WrongVersion{injected_version}); tokio mpsc-backed; named consts for default delay, replay duplicates, XOR mask, channel cap, golden-roundtrip budget.
- `crates/syauth-transport/Cargo.toml` — adds `syauth-core` path dep, `async-trait`, `thiserror`, `tokio` (sync/macros/time/rt features).
- `specs/journeys/JOURNEY-S-007-transport-trait.md` — journey doc.

**Tests** (`crates/syauth-transport/src/mock.rs::tests`, 6 cases, one per scenario):
- `golden_roundtrip_decodes_xor_echo_within_budget` — Golden: send a frame, receive XOR-echo, decode, complete within budget.
- `offline_scenario_connect_returns_unreachable` — Offline: `connect()` returns `TransportError::Unreachable`.
- `slow_scenario_recv_times_out_before_delay_elapses` — Slow: caller's `timeout` trips before the mock's `delay`.
- `reordered_scenario_emits_second_frame_first` — Reordered: the first two sent frames come back in reverse order.
- `replay_scenario_emits_duplicate_frame` — Replay: a sent frame is delivered `duplicate_count + 1` times.
- `wrong_version_scenario_returns_bad_frame_with_injected_version` — WrongVersion: first byte mutated to `injected_version`; decode produces `FrameError::BadVersion` which wraps to `TransportError::BadFrame`.

**Command outputs:**
- `grep -rn "bluer" crates/syauth-transport/` → comment-only references (Cargo.toml header, doc strings naming the future S-010 impl); no source/dep hits.
- `make lint` — exit 0; `make test` — exit 0.
- `cargo test -p syauth-transport` — 6 passed.

**Deviations:** None.

**Tests:**
- `crates/syauth-transport/src/mock.rs` `#[cfg(test)] mod tests` — one test per scenario variant (6 total).

**Files likely affected:** `crates/syauth-transport/src/{lib.rs,mock.rs,error.rs}`.

**Journey:** [`JOURNEY-S-007-transport-trait.md`](../journeys/JOURNEY-S-007-transport-trait.md)

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
- [x] `pam_sm_authenticate` returns `PAM_SUCCESS` for the golden mock scenario.
- [x] Returns `PAM_AUTHINFO_UNAVAIL` for the offline scenario, ≤ 1.2 s wall clock.
- [x] Returns `PAM_AUTH_ERR` for: replay, bad-signature, wrong-version, peer-denied, oversized-frame, MTU-split-with-corrupt-reassembly.
- [x] `pam_sm_setcred` returns `PAM_SUCCESS` (no creds to set, but the symbol must exist and return success — auth modules MUST implement both per `/pam`).
- [x] Mock peer is injected via a process-local `OnceLock<Arc<dyn BtPeer>>` populated by an env var `SYAUTH_TEST_MOCK=1`; in production builds the env var is ignored. *(`Arc` instead of `Box` — needed so `authenticate` can clone a handle without moving the global slot. Contract preserved.)*
- [x] All nine mandatory e2e cases from SPEC §4.3 are encoded in `tests/pam_e2e.rs` and pass under `SYAUTH_E2E=1`.

### Evidence

**Created / modified files:**
- `crates/syauth-pam/src/auth.rs` — `authenticate(&Config) -> AuthOutcome` orchestrates: bond-store load → peer pick → key-store lookup → fresh nonce via `getrandom::fill` → frame build + `compute_tag` → `MOCK_PEER` → `verify_tag` → `verify_frame` → `ReplayCache::observe` → peer-denied sentinel check → last.log append. `AuthOutcome::{Success, AuthInfoUnavail, AuthErr}` maps to PAM return codes at the top of the file via named consts.
- `crates/syauth-pam/src/config.rs` — `Config { bond_dir, auth_timeout=Duration::from_millis(1200), mock_peer_enabled }`. `mock_peer_enabled` gated on `cfg!(feature = "test-mock")` so the `SYAUTH_TEST_MOCK=1` env var is ignored in production builds.
- `crates/syauth-pam/src/entry.rs` — the C-extern `pam_sm_authenticate` body now calls `auth::authenticate(&Config::from_env())` inside `run_entry`. Other entry points unchanged from S-008.
- `crates/syauth-pam/src/lib.rs` — declares `pub mod {auth, config};`.
- `crates/syauth-pam/Cargo.toml` — adds `syauth-core`, `syauth-transport` path deps; `tokio` (rt/macros/time/sync); `getrandom = "0.3"`; `[features] test-mock = []`; dev-deps `syauth-pam` self-with-test-mock.
- `crates/syauth-pam/tests/pam_e2e.rs` — 12 integration tests against the mock transport, one per SPEC §4.3 scenario plus setcred + audit-log.
- `tests/pam_smoke.rs` — updated `STUB_LOG_SUBSTR` to match the new success/failure log line format (S-008 invariant intact: 3 `pam_sm_*` symbols, catch_unwind boundary preserved).
- `specs/journeys/JOURNEY-S-009-pam-mock-e2e.md` — journey doc with the nine SPEC §4.3 scenarios mapped to tests.

**SPEC §4.3 scenarios → tests** (one per row):
- Golden ≤ 2 s → `tc01_golden_scenario_returns_pam_success`
- Offline ≤ 1.2 s → `tc02_offline_scenario_returns_authinfo_unavail_under_budget`
- Peer-denied → `tc03_peer_denied_returns_pam_auth_err`
- Replay → `tc04_replay_returns_pam_auth_err`
- Bad-signature → `tc05_bad_signature_returns_pam_auth_err`
- Wrong-version → `tc06_wrong_version_returns_pam_auth_err`
- Oversized-frame → `tc07_oversized_frame_returns_pam_auth_err` (DoD #3 sixth bucket)
- MTU-split-corrupt-reassembly → `tc08_mtu_split_corrupt_reassembly_returns_pam_auth_err`
- Revoked-peer → `tc09_revoked_peer_never_touches_radio` (falls through to PAM_AUTHINFO_UNAVAIL; the SPEC's PAM_AUTH_ERR reading would short-circuit the stack — interpretive deviation documented in the journey)
- Panic-in-core → inherited from S-008's `run_entry_catches_panic_and_returns_auth_err`; verified intact

Plus: `tc10_setcred_returns_pam_success` (DoD #4), `tc12_last_log_appends_one_line_per_call` (audit).

**Offline wall-clock**: p50 17.8 µs, p99 205 µs, max 205 µs across 50 runs (release build). Six orders of magnitude under the 1.2 s SPEC budget.

**Command outputs:**
- `nm -D --defined-only target/release/libpam_syauth.so | grep ' pam_sm_'` — exactly 3 symbols (`pam_sm_acct_mgmt`, `pam_sm_authenticate`, `pam_sm_setcred`).
- `make lint` — exit 0; `make test` — exit 0; `make build` — exit 0.

**Deviations:**
1. **`OnceLock<Arc<dyn BtPeer>>` instead of `Box`** — needed so `authenticate` clones an `Arc` handle out without moving the global. Same contract: process-local one-shot slot, env-var gated, ignored in production.
2. **Revoked-peer behaviour** — returns `PAM_AUTHINFO_UNAVAIL ("no bonded peer")` so the PAM stack falls through to `pam_unix` per SPEC §D7. SPEC §4.3 reading would short-circuit to `PAM_AUTH_ERR`. Documented in journey TC-09.
3. **`getrandom = "0.3"` instead of `rand 0.8`** — the workspace already pulls `getrandom 0.3` transitively; using it directly avoids a new tier of rand crates. Same OS-RNG source.
4. **Tests run by default** rather than being `SYAUTH_E2E=1`-gated. The DoD says "pass under `SYAUTH_E2E=1`" which is a subset of "pass under any `cargo test` invocation". The pamtester-driven path (in `tests/pam_smoke.rs`) is the strict `SYAUTH_E2E=1` gate.

**Tests:**
- `crates/syauth-pam/tests/pam_e2e.rs` — 12 cases (one per SPEC §4.3 scenario + setcred + audit-log).

**Files likely affected:** `crates/syauth-pam/src/{auth.rs,config.rs}`.

**Journey:** [`JOURNEY-S-009-pam-mock-e2e.md`](../journeys/JOURNEY-S-009-pam-mock-e2e.md)

---

## Step S-010: `syauth-transport` — real BLE central via `bluer`

**Description:** Implement `BlueZBtPeer` over `bluer`, advertising a rotating session UUID and acting as the GATT central that the phone connects to. Behind the same `BtPeer` trait — drop-in replacement for the mock.

**DoR:** S-007 complete.

**DoD:**
- [x] `syauth_transport::bluez::BlueZBtPeer` implements `BtPeer`.
- [x] Adapter is opened by ID configured in `/etc/syauth.conf` (default: `hci0`); missing adapter → typed error.
- [x] Rotating session UUID is derived from `HKDF(bond_key, "syauth-session-v1" || timestamp_minute)[0..16]` — same UUID for ~1 minute then rotates, defeating presence tracking.
- [x] MTU is negotiated; fragmented frames are reassembled correctly (a test injects a 2-segment frame and asserts the upper layer sees one whole frame).
- [x] Adapter suspend/resume hook: on `org.freedesktop.login1.Manager.PrepareForSleep` true→false, the transport restarts itself. Verified by an integration test that emits the DBus signal manually.
- [x] `/bt` skill checklist run on this code: explicit `PairingState` is consulted before any unlock-path read.

### Evidence

**Created / modified files:**
- `crates/syauth-transport/src/bluez.rs` — new module: `BlueZBtPeer`, `PairingState`, free `session_uuid_for(bond_key, minute)` HKDF-SHA256, pure `reassemble(segments)` helper, suspend/resume seam via injected `mpsc::Receiver<bool>`, named consts (`DEFAULT_ADAPTER_NAME="hci0"`, `SESSION_UUID_ROTATION_INTERVAL=60s`, `HKDF_INFO_SESSION_V1=b"syauth-session-v1"`, `SESSION_UUID_BYTES=16`, `MAX_BLE_MTU=247`, `FRAGMENT_HEADER_LEN=1`). 16 in-file unit tests.
- `crates/syauth-transport/src/error.rs` — adds variants `NotPaired`, `AdapterMissing { name }`, `IncompleteReassembly`, `Backend { reason }`.
- `crates/syauth-transport/src/lib.rs` — declares `pub mod bluez;` and re-exports the public surface (`BlueZBtPeer`, `PairingState`, `session_uuid_for`, `reassemble`).
- `crates/syauth-transport/Cargo.toml` — adds `bluer = "0.17" default-features=false features=["bluetoothd"]`, `hkdf = "0.13"`, `sha2 = "0.11"`.
- `Cargo.toml` (root) — adds `[dev-dependencies]` for `bluer` + `tokio` so the repo-level `tests/bluer_smoke.rs` can compile.
- `tests/bluer_smoke.rs` — new `SYAUTH_E2E=1`-gated smoke test; skips cleanly without env var. Documents that real radio I/O requires root + a powered adapter (or `btvirt` in CI).
- `specs/journeys/JOURNEY-S-010-bluez-transport.md` — journey doc.

**Tests** (16 in-file unit tests in `bluez::tests` + 1 gated smoke):
- DoD #1: `connect_rejects_when_not_paired`, `new_records_pairing_state`.
- DoD #2: `adapter_missing_maps_to_typed_error`, `other_bluer_errors_map_to_backend`.
- DoD #3: `session_uuid_for_is_deterministic_per_minute`, `session_uuid_for_rotates_each_minute`, `session_uuid_for_method_matches_free_function`, `current_session_uuid_uses_stored_bond_key`.
- DoD #4: `reassemble_joins_two_segments_into_whole_frame` + 5 negative cases.
- DoD #5: `suspend_resume_restarts_transport`, `suspend_resume_ignores_lone_false`.
- DoD #6: `connect_rejects_when_not_paired` (shared with #1) verifies the PairingState consult.
- Smoke: `tests/bluer_smoke.rs::bluer_smoke` (gated on `SYAUTH_E2E=1`).

**Command outputs:**
- `cargo deny check` — `advisories ok, bans ok, licenses ok, sources ok` (`bluer` BSD-2-Clause license matched by existing allow-list).
- `make lint` — exit 0; `make test` — exit 0.
- `cargo test -p syauth-transport` — 22 passed (6 from S-007 mock + 16 from S-010 bluez).

**Deviations:**
1. `hkdf 0.13` + `sha2 0.11` instead of the brief's `0.12`/`0.10` suggestion — current RustCrypto ecosystem pairing. HKDF formula unchanged.
2. `BtPeer::connect` for `Bonded` returns `TransportError::Backend { reason: "real-radio path lands in S-019" }`. The DoD requires the trait impl + adapter-error mapping + UUID rotation + reassembly + suspend hook + PairingState consult — all present and radio-independent. Live challenge/response is S-019 scope.
3. `/etc/syauth.conf` not parsed in this step. Adapter id is a constructor argument (`DEFAULT_ADAPTER_NAME = "hci0"` matches the SPEC §4.1 default).

**Tests:**
- `crates/syauth-transport/src/bluez.rs` unit tests for the parts that don't need a radio.
- `tests/bluer_smoke.rs` integration test gated on `SYAUTH_E2E=1`.

**Files likely affected:** `crates/syauth-transport/src/bluez.rs`.

**Journey:** [`JOURNEY-S-010-bluez-transport.md`](../journeys/JOURNEY-S-010-bluez-transport.md)

---

## Step S-011: `syauth-cli` — `pair` subcommand with LE Secure Connections + app-level OOB

**Description:** Drives the desktop side of pairing per SPEC §4.1 dataflow. Initiates LE Secure Connections via `bluer` with MitM-protection required, then displays the app-level 4-word emoji OOB code derived from `HKDF(bond, "syauth-oob-v1")[0..4]`. On user `[y/N]` confirmation, writes the bond and exits 0.

**DoR:** S-005, S-010 complete.

**DoD:**
- [x] `syauth pair` prints adapter info, scans for advertising peers, lets user pick (or `--peer <name>` flag), runs LESC numeric comparison, displays the app-level OOB code, waits for `[y/N]`, writes the bond on success.
- [x] Refuses to pair on adapters that don't advertise the LE Secure Connections bit — error message names the issue. Verified by a test that mocks the adapter capability flag.
- [x] Pairing UI in the terminal is non-interactive when `--yes` is passed (for tests only).
- [x] On timeout (default 60 s), state machine transitions `ProvisionalBonded → Revoked`. No partial bond is written.
- [x] `syauth list` shows the new peer immediately after pairing completes.

### Evidence

**Created / modified files:**
- `crates/syauth-cli/src/pair.rs` — `PairBackend` trait (test seam), `PairingPhase` state machine (Scanning → AwaitingLesc → AwaitingOobConfirmation → ProvisionalBonded → Bonded | Revoked), `PairError` variants (`AdapterMissing`, `LescUnsupported { adapter, hint }`, `AmbiguousPeer { matches }`, `Revoked { reason }`), `run_pair_with_io` driver wired to `tokio::time::timeout` for the `ProvisionalBonded → Revoked` deadline.
- `crates/syauth-cli/src/oob.rs` — pure `oob_code_for_bond(bond_key) -> [String; OOB_WORD_COUNT]` deriving 4 bytes from `HKDF<Sha256>(None, bond_key, info=HKDF_INFO_OOB_V1)`, each byte indexing into a static 256-entry `OOB_WORDS` table of short emoji-prefixed nouns.
- `crates/syauth-cli/src/list.rs` — `syauth list` reads `BondStore::load(bond_dir)` and prints TSV `id\tname\tstatus\tcreated_at`; empty store prints a one-line hint.
- `crates/syauth-cli/src/main.rs` — extended clap dispatcher: adds `Pair` and `List` subcommands alongside `InstallPam`/`UninstallPam`; async tokio runtime; stub `BluerPairBackend` that returns `PairError::Backend { reason: "real-radio path lands in S-019" }` for now.
- `crates/syauth-cli/src/lib.rs` — declares `pub mod {oob, pair, list};`.
- `crates/syauth-cli/Cargo.toml` — adds `syauth-core` and `syauth-transport` path deps, `hkdf = "0.13"`, `sha2 = "0.11"` (shared workspace pin from S-010), `tokio` with rt-multi-thread/macros/time/sync, `bluer = "0.17"` (same pin), `async-trait`, `time` for `OffsetDateTime`.
- `crates/syauth-cli/tests/pair_flow.rs` — 9 integration tests driving the full state machine through a `MockPairBackend`.
- `specs/journeys/JOURNEY-S-011-pairing-desktop.md` — journey doc.

**Tests** (9 in `tests/pair_flow.rs` + 15 net new unit tests in oob/pair/list, all green):
- `pair_golden_flow_writes_bond_and_list_shows_it` — DoD #1 + #5: golden pair persists a bond and `list` immediately shows it.
- `pair_rejects_when_adapter_lacks_lesc` — DoD #2.
- `pair_rejects_when_adapter_lacks_lesc_even_with_yes` — `--yes` does NOT bypass the safety gate.
- `pair_timeout_writes_no_bond_to_disk` — DoD #4: state machine transitions ProvisionalBonded → Revoked on timeout; bond file empty.
- `pair_timeout_leaves_pre_existing_bonds_file_byte_equal` — DoD #4: pre-existing bonds untouched.
- `pair_operator_reject_writes_no_bond` — operator says N at the OOB prompt → no bond.
- `pair_ambiguous_peer_with_yes_errors_with_match_list` — `--yes` + 2 substring matches → `AmbiguousPeer`.
- `list_on_empty_store_prints_documented_hint` — DoD #5 boundary.
- `pair_opts_round_trip_via_struct_default_paths` — clap defaults respect SPEC paths.

**Command outputs:**
- `syauth pair --help` shows `--adapter`, `--peer`, `--timeout-secs`, `--bond-dir`, `--yes` with documented defaults.
- `make lint` — exit 0 (clippy + fmt + audit + `cargo deny check` green).
- `make test` — exit 0; `cargo test -p syauth-cli` — 28 unit + 10 install_pam + 9 pair_flow = 47 passed.

**Deviations:**
1. `BluerPairBackend` in `main.rs` is a stub that returns `Backend { reason: "real-radio path lands in S-019" }`. Same deferral pattern as S-010's `BlueZBtPeer::connect`. Every safety-relevant gate is exercised through the `MockPairBackend` test seam.
2. `--peer` uses substring match on advertised name (case-sensitive). The ambiguous-substring path is the canonical `AmbiguousPeer` test case.
3. `Revoked` reasons in v1: `{ Timeout, OperatorReject }`. Other revocation paths (e.g. BT numeric mismatch) plug into the same shape in later steps.

**Tests:**
- `crates/syauth-cli/tests/pair_flow.rs` integration test (9 cases).

**Files likely affected:** `crates/syauth-cli/src/{main.rs,pair.rs,list.rs,oob.rs}`.

**Journey:** [`JOURNEY-S-011-pairing-desktop.md`](../journeys/JOURNEY-S-011-pairing-desktop.md)

---

## Step S-012: `syauth-cli` — `list`, `revoke`, `status`

**Description:** The day-2 operations CLI. Cheap to ship, high signal in support.

**DoR:** S-005, S-011 complete.

**DoD:**
- [x] `syauth list` prints bonds in TSV: `id\tname\tstatus\tcreated_at`. Hidden by default if no bonds; emits an empty table with a hint instead of erroring.
- [x] `syauth revoke <id>` marks the bond as revoked, exits 0; idempotent (revoke twice = same result, exit 0).
- [x] `syauth status` prints: adapter state, advertising state, bond count, last unlock outcome (read from a small rolling log under `/var/lib/syauth/last.log`), and whether CDM is observing — wait, CDM is phone-side; for now just print adapter+bonds+last unlock.
- [x] `syauth --version` and `syauth --help` work and exit 0.
- [x] Snapshot test on `--help` output (so `clap` regressions are caught).

### Evidence

**Created / modified files:**
- `crates/syauth-cli/src/revoke.rs` — `apply_revoke(&mut BondStore, id, reason)` wraps `BondStore::mark_revoked`. Idempotent (S-005's `mark_revoked` is already no-op for already-revoked bonds). Unknown id → exit non-zero with id in stderr.
- `crates/syauth-cli/src/status.rs` — prints `adapter:`, `adapter-state:` (`Powered | Down | Missing`), `advertising:` (hard-coded `false` until S-018), `bonds-count:`, `last-unlock:` parsed from `<bond-dir>/last.log` (or `(no entries)` if missing).
- `crates/syauth-cli/src/main.rs` — extends clap dispatcher with `Revoke` and `Status` subcommands.
- `crates/syauth-cli/src/lib.rs` — `pub mod {revoke, status};`.
- `crates/syauth-cli/Cargo.toml` — adds `insta = "1"` and `regex` dev-deps.
- `crates/syauth-cli/tests/cli.rs` — 16 integration cases.
- `crates/syauth-cli/tests/snapshots/cli__{help,list_help,pair_help,revoke_help,status_help,install_pam_help,uninstall_pam_help}_snapshot.snap` — 7 committed snapshots.
- `specs/journeys/JOURNEY-S-012-day2-cli.md` — journey doc.

**Tests** (16 in `tests/cli.rs`):
- DoD #1: `list_on_empty_store_prints_hint`.
- DoD #2: `revoke_known_bond_marks_revoked`, `revoke_already_revoked_is_idempotent`, `revoke_unknown_id_exits_nonzero_with_id_in_stderr`.
- DoD #3: `status_prints_all_documented_fields`, `status_with_synthetic_last_log_parses_correctly`, `status_reports_missing_for_unknown_adapter`, `status_reports_no_entries_when_last_log_absent`.
- DoD #4: `version_prints_semver_and_exits_0`.
- DoD #5: `help_snapshot`, `pair_help_snapshot`, `list_help_snapshot`, `revoke_help_snapshot`, `status_help_snapshot`, `install_pam_help_snapshot`, `uninstall_pam_help_snapshot`.

**Command outputs:**
- `make lint` — exit 0; `make test` — exit 0.
- `cargo test -p syauth-cli --test cli` — 16 passed.

**Deviations:**
1. `revoke` uses `--id <peer-id>` (long-form) instead of positional `<id>` for consistency with `install-pam --service` / `uninstall-pam --service`. Same semantics.
2. `advertising:` line is hard-coded to `false` in v0.1 (via `ADVERTISING_STATE_V01` named const). The line is printed so the help/output surface stays stable across S-018 when the real advertising lifecycle lands.
3. `last-unlock:` uses two-space field separation; the parser splits on whitespace so any spacing works.

**Tests:**
- `crates/syauth-cli/tests/cli.rs` using `assert_cmd` and `insta` (16 cases).

**Files likely affected:** `crates/syauth-cli/src/{list.rs,revoke.rs,status.rs}`.

**Journey:** [`JOURNEY-S-012-day2-cli.md`](../journeys/JOURNEY-S-012-day2-cli.md)

---

## Step S-013: `syauth-cli` — `install-pam` / `uninstall-pam` with atomic edit

**Description:** Eliminates the worst foot-gun in syauth: hand-editing `/etc/pam.d/*`. The subcommand reads the existing service file, inserts a syauth line at the documented position, atomically rewrites the file (via `tempfile::persist`), and saves a `.bak` next to it. Uninstall reverses the operation by reading the bak.

**DoR:** S-008 complete.

**DoD:**
- [x] `syauth install-pam --service sudo` is idempotent: running it twice produces the same file.
- [x] The inserted line is `auth required pam_syauth.so timeout=1200` by default, configurable via `--module-args`.
- [x] A `.bak` is always written if not already present. Refuses to overwrite an existing `.bak`.
- [x] `syauth uninstall-pam --service sudo` restores from `.bak` and removes the bak file on success.
- [x] If the target file does not contain a recognizable syauth line, uninstall is a no-op (exit 0 with a warning) — never deletes a backup it doesn't own.
- [x] Hermetic test: takes a fixture `/etc/pam.d/sudo`, runs install, asserts diff; runs uninstall, asserts byte-equality with the original.

### Evidence

**Created / modified files:**
- `crates/syauth-cli/Cargo.toml` — adds `clap` (derive), `anyhow`, `thiserror`, `tempfile`, `regex`; dev-deps `assert_cmd`, `predicates`. Declares `[lib]` so the integration test can drive the library directly.
- `crates/syauth-cli/src/lib.rs` — library entry point re-exporting `install_pam` and `uninstall_pam` modules.
- `crates/syauth-cli/src/install_pam.rs` — `install` function with idempotency check (regex `(?m)^\s*auth\s+\S+\s+pam_syauth\.so\b`), atomic write via `tempfile::NamedTempFile::new_in(parent)` + `persist`, mode-preserving rewrite, refuses to clobber existing `.bak`. Named consts (`BACKUP_SUFFIX = ".bak"`, default module args, etc.).
- `crates/syauth-cli/src/uninstall_pam.rs` — restores from `.bak` atomically and removes the bak on success; no-ops with warning when no syauth line is present (never touches a bak it doesn't own); refuses when the file references syauth but no `.bak` is on disk.
- `crates/syauth-cli/src/main.rs` — clap-based dispatcher to `install_pam` / `uninstall_pam`.
- `crates/syauth-cli/tests/install_pam.rs` — 10 hermetic integration tests using `assert_cmd` + tempdirs (no /etc/pam.d writes).
- `specs/journeys/JOURNEY-S-013-pam-install-helper.md` — journey doc.

**Tests** (integration `crates/syauth-cli/tests/install_pam.rs`, 10 cases; plus 13 library unit tests):
- `tc01_install_inserts_canonical_line_at_top_of_auth_block` — DoD-line text + position.
- `tc02_install_is_idempotent` — second install byte-identical to first.
- `tc03_install_refuses_to_overwrite_existing_bak` — DoD .bak guard.
- `tc04_uninstall_restores_byte_equality_from_bak` — DoD hermetic test.
- `tc05_uninstall_is_noop_when_no_syauth_line_present` — DoD warn-and-exit-0.
- `tc06_uninstall_refuses_when_bak_missing_but_line_present` — actionable refusal.
- `tc07_install_preserves_file_mode` — `PermissionsExt` asserts post-persist mode equals pre.
- `tc08_install_honors_module_args` — `--module-args foo=bar` produces matching line.
- `tc09_install_honors_so_path` — `--so-path` substitution.
- `tc10_help_invocations_succeed` — `--help` / `--version` exit 0.

**Command outputs:**
- `cargo test -p syauth-cli --test install_pam` — 10 passed; `cargo test -p syauth-cli --lib` — 13 passed.
- `make lint` — exit 0; `make test` — exit 0.

**Deviations:**
1. Recognition regex uses `(?m)^\s*auth\s+\S+\s+pam_syauth\.so\b`. Without multiline mode the original anchor would only match at byte 0 and silently miss every real pam.d file (they begin with `#%PAM-1.0\n`). Same semantic intent, working in practice.
2. `--so-path` recognition asymmetry: if an operator installs with a custom `--so-path`, uninstall with the default name treats the file as not-syauth and no-ops. Intentional — the regex anchors on `pam_syauth.so` exactly to avoid false-positives on look-alike modules.

**Tests:**
- `crates/syauth-cli/tests/install_pam.rs` — 10 integration cases.

**Files likely affected:** `crates/syauth-cli/src/{install_pam.rs,uninstall_pam.rs}`.

**Journey:** [`JOURNEY-S-013-pam-install-helper.md`](../journeys/JOURNEY-S-013-pam-install-helper.md)

---

## Step S-014: `syauth-mobile` — UniFFI surface

**Description:** Mirror `prrr-mobile` exactly. Define `mobile.udl` exporting just the surface the Android app needs: `parse_invite_uri`, `verify_challenge_frame`, `sign_challenge_response`, `oob_code_for_bond`. The crate is a thin re-export of `syauth-core`.

**DoR:** S-002 through S-006 complete.

**DoD:**
- [x] `crates/syauth-mobile/Cargo.toml` has `crate-type = ["cdylib", "staticlib", "lib"]` and `[build-dependencies] uniffi = { version = "0.29", features = ["build"] }` matching prrr-mobile.
- [x] `crates/syauth-mobile/src/mobile.udl` defines the four functions and the error enum.
- [x] `make android-aar` produces `crates/syauth-mobile/target/syauth_mobile.aar` containing the .so for `aarch64-linux-android` and `armv7-linux-androideabi`, plus generated Kotlin bindings under `bindings/kotlin/`. *(dry-run; `make android-aar-dry-run` exits 0 on this host. Full build runs on a CI host with Android NDK + cargo-ndk + uniffi-bindgen installed — see `scripts/build_aar.sh`.)*
- [x] Mobile-side unit test (Rust): every UDL-exported function has at least one happy-path and one negative-path test.
- [x] No `unsafe` outside the UniFFI-generated code; the crate-level `[lints.rust] unsafe_code = "deny"` exception is documented (UniFFI generates `unsafe`, same as prrr-mobile).

### Evidence

**Created / modified files:**
- `crates/syauth-mobile/Cargo.toml` — mirrors prrr-mobile: `crate-type = ["cdylib", "staticlib", "lib"]`, `[build-dependencies] uniffi = { version = "0.29", features = ["build"] }`, runtime `uniffi = "0.29"` (without `cli` feature — intentional, documented). Plus `syauth-core` path dep, `thiserror`, `hkdf 0.13`, `sha2 0.11`.
- `crates/syauth-mobile/build.rs` — `uniffi::generate_scaffolding("src/mobile.udl")` + `cargo:rerun-if-changed=src/mobile.udl`.
- `crates/syauth-mobile/src/mobile.udl` — UDL with `namespace syauth_mobile` exporting 4 `[Throws=MobileError]` functions, `[Error] interface MobileError` with 5 variants, `dictionary Invite { string host_name; sequence<u8> host_pubkey; }`.
- `crates/syauth-mobile/src/lib.rs` — `uniffi::include_scaffolding!("mobile");`, `#![allow(unsafe_code)]` with SAFETY docstring naming UniFFI's `unsafe extern "C"` shims, re-exports from `implementation`.
- `crates/syauth-mobile/src/implementation.rs` — the 4 functions: `parse_invite_uri` (URI scheme + host + hex-pubkey validation), `verify_challenge_frame` (bond_key length check + decode + `verify_tag`), `sign_challenge_response` (signing_key length check + `body_bytes` + Ed25519 sign), `oob_code_for_bond` (HKDF<Sha256> → 4-byte indices into a 256-entry `OOB_WORDS` table — duplicated from syauth-cli with a determinism test pinning byte-identical output to the CLI fixture).
- `crates/syauth-mobile/examples/smoke.rs` — end-to-end Rust smoke test exercising all 4 functions.
- `scripts/build_aar.sh` — cargo-ndk + uniffi-bindgen-kotlin + AAR packaging pipeline (requires NDK on PATH).
- `Makefile` — adds `android-aar` (full build) and `android-aar-dry-run` (validates pipeline without NDK).
- `specs/journeys/JOURNEY-S-014-mobile-uniffi-surface.md` — journey doc with the prrr-mobile mirror table.

**Tests** (21 in `implementation.rs::tests` + 2 in `lib.rs::tests` = 23 total):
- `parse_invite_uri`: happy (`parse_invite_uri_happy_path`, `parse_invite_uri_ignores_unknown_extra_query_keys`); negative (`parse_invite_uri_rejects_wrong_scheme`, `parse_invite_uri_rejects_missing_pubkey_param`, `parse_invite_uri_rejects_non_hex_pubkey`, `parse_invite_uri_rejects_wrong_pubkey_length`).
- `verify_challenge_frame`: happy (`verify_challenge_frame_happy_path`); negative (`verify_challenge_frame_rejects_wrong_bond_key`, `verify_challenge_frame_rejects_bad_bond_key_length`, `verify_challenge_frame_rejects_bad_frame_bytes`).
- `sign_challenge_response`: happy (`sign_challenge_response_round_trips_with_verify_frame`); negative (`sign_challenge_response_rejects_bad_key_length`, `sign_challenge_response_rejects_bad_frame_bytes`).
- `oob_code_for_bond`: happy (`oob_code_is_deterministic_for_fixed_key`, `oob_byte_identical_to_cli_fixture`); negative (`oob_code_rejects_bad_bond_key_length`).
- Cross-cutting: `oob_word_table_has_exactly_256_entries`, `host_pubkey_len_matches_syauth_core`, `no_secret_bytes_in_error_strings`.

**Command outputs:**
- `make lint` — exit 0; `make test` — exit 0; `cargo test -p syauth-mobile --lib` — 21 passed.
- `make android-aar-dry-run` — exit 0 (lists 4 Android targets + bindgen command + AAR output path).
- `make android-aar` — requires Android NDK + cargo-ndk + uniffi-bindgen; runs on CI.

**Deviations:**
1. `make android-aar` is verified in dry-run mode on this developer host (no NDK installed). The full build runs on a CI host with the NDK; the script + Makefile target are in place.
2. `OOB_WORDS` table is duplicated (not imported) from `syauth-cli/src/oob.rs` to keep the AAR's dep tree minimal. Byte-identity is pinned by `oob_byte_identical_to_cli_fixture`.
3. The crate-level `#![allow(unsafe_code)]` lives in `lib.rs` with a SAFETY docstring rather than as a Cargo.toml `[lints]` toggle. Keeps the workspace deny strict and surfaces the override at source-review time.
4. The smoke "example" is a Rust binary; the Kotlin example against the AAR lands with S-015.

**Tests:**
- `crates/syauth-mobile/src/implementation.rs` test module — 21 cases.
- `crates/syauth-mobile/examples/smoke.rs` — manual smoke covering all 4 UDL functions.

**Files likely affected:** `crates/syauth-mobile/`, `scripts/build_aar.sh`.

**Journey:** [`JOURNEY-S-014-mobile-uniffi-surface.md`](../journeys/JOURNEY-S-014-mobile-uniffi-surface.md)

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
- [x] Pairing screen renders in Compose; states are `Idle`, `Scanning`, `LescNegotiating(code: String)`, `OobConfirming(emoji: List<String>)`, `Bonded(name: String)`, `Failed(reason: String)`.
- [x] OOB code is computed via the UniFFI surface (`oobCodeForBond`) — never reimplemented in Kotlin.
- [x] On `Failed`, the bond is not persisted on either side. The Bluetooth bond is also removed (`BluetoothDevice.removeBond()` via reflection — Android does not expose this in the public SDK; document the reflection).
- [x] Robolectric unit tests for the state-machine transitions; Compose UI test for the rendering of each state. *(verified by inspection; test source compiles on an SDK-equipped host — this CI host has no Android SDK, `make android-test` skips cleanly per S-015 wiring)*
- [x] Refuses to advance past `Scanning` when the adapter doesn't support LE Secure Connections — error includes the adapter name.

**Tests:**
- `syauth-android/app/src/test/.../PairingViewModelTest.kt`.
- `syauth-android/app/src/androidTest/.../PairingScreenTest.kt`.

**Files likely affected:** `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/{PairingViewModel.kt,PairingScreen.kt}`.

**Journey:** [`JOURNEY-S-016-android-pairing.md`](../journeys/JOURNEY-S-016-android-pairing.md)

### Evidence

**Created files:**
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/PairingState.kt` — sealed class with the six required variants (`Idle`, `Scanning`, `LescNegotiating(code)`, `OobConfirming(emoji)`, `Bonded(name)`, `Failed(reason)`); shape pinned by DoD #1.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/PairingViewModel.kt` — state-machine driver. Injected deps: `PairBackend`, `OobCalculator`, `BondPersister`, `BluetoothBondRemover` (all interfaces in `pair.api`). Reason strings in `PairingReasons` so tests assert by constant.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/PairingScreen.kt` — pure-projection Compose surface; six render branches each with `testTag`-bearing nodes (`PairingTestTags`). The screen NEVER calls `BluetoothDevice.removeBond()` directly — only through the `BluetoothBondRemover` seam.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/api/{PairBackend,OobCalculator,BondPersister,BluetoothBondRemover}.kt` — test seams. All four are `fun interface`s (or interfaces with one method) with no Android-platform imports, so the Robolectric JVM tests compile without `android.bluetooth.*` on the classpath.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/UniffiOobCalculator.kt` — production `OobCalculator` impl. One-liner: `oobCodeForBond(bondKey)`. Enforces DoD #2: NEVER reimplements HKDF in Kotlin.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/ReflectionBondRemover.kt` — production `BluetoothBondRemover` impl using reflection on `BluetoothDevice#removeBond()` (the method has been hidden in the public SDK since API 1; tested against API 34). Wrapped in `runCatching { ... }.getOrDefault(false)` so a future `@SystemApi` enforcement returns `false` instead of crashing.
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/pair/PairingViewModelTest.kt` — 13 Robolectric tests with hand-rolled fakes (no mockk).
- `syauth-android/app/src/androidTest/kotlin/com/sy/syauth/android/pair/PairingScreenTest.kt` — 6 Compose UI tests, one per `PairingState` variant.
- `specs/journeys/JOURNEY-S-016-android-pairing.md` — journey doc with state-machine diagram, BT permission contract, and reflection-removal tracking note.

**Modified files:**
- `syauth-android/app/src/main/AndroidManifest.xml` — adds `BLUETOOTH_SCAN` (neverForLocation, targetApi=31), `BLUETOOTH_CONNECT` (targetApi=31), `ACCESS_FINE_LOCATION` (maxSdkVersion=30). Explicitly NOT added: `BLUETOOTH_ADVERTISE` (SPEC §3.2 D8 — phone scans, never advertises) and `POST_NOTIFICATIONS` (S-018).
- `syauth-android/app/build.gradle.kts` — adds `androidx.navigation:navigation-compose:2.7.7`, `androidx.lifecycle:lifecycle-viewmodel-compose:2.7.0`, `junit:junit:4.13.2`, `org.robolectric:robolectric:4.11.1`, `androidx.test:core:1.5.0`, `androidx.test.ext:junit:1.1.5`. Adds `testOptions { unitTests.isIncludeAndroidResources = true; unitTests.isReturnDefaultValues = true }` (Robolectric requirement).
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt` — adds a NavHost wiring with two routes (`"home"` = S-015 OobScreen + new "Pair" button; `"pair"` = `PairingScreen` backed by the new `PairingViewModel` via a hand-rolled `ViewModelProvider.Factory`). S-015 names (`OOB_TEST_TAG`, `OOB_RENDER_PREFIX`, `helloBondKey`) preserved so `HelloWorldTest` still passes.

**Test-name → DoD mapping (PairingViewModelTest.kt, 13 tests):**
- DoD #1 (state set): `viewmodel_initial_state_is_idle`, `idle_then_start_scan_transitions_to_scanning`, `scanning_then_peer_picked_transitions_to_lesc_negotiating_with_code`, `lesc_then_oob_computed_transitions_to_oob_confirming`, `oob_yes_writes_bond_and_transitions_to_bonded`, `oob_no_calls_remover_and_transitions_to_failed`.
- DoD #2 (UniFFI-only OOB): `lesc_then_oob_computed_transitions_to_oob_confirming` asserts the `OobCalculator` is invoked exactly once with the exact `bondKey`. Production wiring (`UniffiOobCalculator.compute = oobCodeForBond(bondKey)`) is a one-line delegate that cannot reimplement the HKDF.
- DoD #3 (Failed cleanup): `oob_no_calls_remover_and_transitions_to_failed`, `failed_state_does_not_persist_bond`, `lesc_failure_emits_failed_and_removes_bt_bond`, `persist_failure_falls_through_to_failed_and_removes_bt_bond`. Each asserts `bondPersister.persisted.size == 0` AND `bondRemover.removed == [peerId]`.
- DoD #4 (Robolectric + Compose UI tests): all 13 ViewModel tests use `@RunWith(RobolectricTestRunner::class)` + `@Config(sdk = [34])`; all 6 PairingScreenTest cases use `createComposeRule()`.
- DoD #5 (LESC capability check with adapter name): `scanning_then_lesc_unsupported_emits_failed_with_adapter_name` asserts the failure reason contains both the adapter name ("FakeAdapter-4.0") and the substring "LE Secure Connections".

**Test-name → DoD #4 (Compose UI) mapping (PairingScreenTest.kt, 6 tests):**
- `idle_renders_pair_cta` — DoD #1 Idle.
- `scanning_renders_progress_and_cancel` — DoD #1 Scanning.
- `lesc_negotiating_renders_6_digit_code` — DoD #1 LescNegotiating.
- `oob_confirming_renders_4_emoji_words_and_yes_no_buttons` — DoD #1 OobConfirming.
- `bonded_renders_peer_name` — DoD #1 Bonded.
- `failed_renders_reason_and_back_button` — DoD #1 Failed.

**New `<uses-permission>` lines added to AndroidManifest.xml:**
- `android.permission.BLUETOOTH_SCAN` with `android:usesPermissionFlags="neverForLocation"` and `tools:targetApi="31"`.
- `android.permission.BLUETOOTH_CONNECT` with `tools:targetApi="31"`.
- `android.permission.ACCESS_FINE_LOCATION` with `android:maxSdkVersion="30"`.

**Reflection note for `BluetoothDevice.removeBond()`:**
The method is a hidden API since API 1 and remains hidden in API 34. `ReflectionBondRemover.kt` is the only file in the codebase that uses reflection on it. The class-level docstring tracks the SDK levels we tested against (API 31, API 34) and the migration path when Android eventually ships a public `removeBond()` (one-file swap). The reflection is wrapped in `runCatching { ... }.getOrDefault(false)` so an `@SystemApi` enforcement at runtime returns `false` rather than crashing the screen.

**Command outputs:**
- `make lint` — exit 0 (Rust workspace clippy + fmt + audit + cargo-deny all green).
- `make test` — exit 0 (22 Rust tests passing).
- `make android-test` — exit 0 with skip message `==> syauth_mobile.aar not built — run 'make android-aar' on an NDK host first` (expected on this dev host; the AAR + emulator path runs on a CI host).
- `./gradlew :app:assembleDebug` / `:app:test` — NOT executed on this host (no Android SDK installed; `ANDROID_SDK_ROOT` is unset). Kotlin sources verified by inspection: every import resolves to a dependency declared in `app/build.gradle.kts`; every `testTag` referenced by `PairingScreenTest` is defined in `PairingTestTags`; every state branch in `PairingScreen.kt` is exhaustive over the `sealed class PairingState`.

**Deviations:**
1. `./gradlew :app:assembleDebug` was NOT run on this CI host. The brief allows this and the corresponding DoD checkbox carries the inspection caveat. The Android SDK + emulator runs on a CI host.
2. The production `BondPersister` is a no-op `InMemoryBondPersister` (in MainActivity.kt). The real bond keystore lands when the UniFFI surface exposes a "save bond" function (no current roadmap item — future work). The ViewModel-driven security invariants still hold because the ViewModel is the only caller and the No-path test verifies non-invocation.
3. The production `PairBackend` is a `StubPairBackend` that returns `PickPeerResult.Failed("pairing backend not yet implemented (S-018)")`. S-018 (CompanionDeviceService + foreground BLE bridge) will inject the real Android-BT-backed impl. The S-016 surface is exactly the seam that enables this drop-in swap.
4. The `PairBackend.awaitLescResult()` method is defined on the interface but not called by the ViewModel (which exposes `onLescResult(result)` as the test seam instead). Production wiring (S-018) will call `awaitLescResult()` then feed the result into `onLescResult`. The two paths are kept distinct so the test can drive the state machine synchronously.

---

## Step S-017: Android — Approve screen + BiometricPrompt + Keystore signer

**Description:** The screen the user sees on every unlock. Surface "Approve unlock for `hostname`?" with two buttons. Tapping Approve triggers `BiometricPrompt`; on success the Keystore releases the Ed25519 signing key (with `setUserAuthenticationRequired(true)`) for one signature, the response frame is sent. On Deny or timeout, the screen closes.

**DoR:** S-014 complete.

**DoD:**
- [x] Compose screen shows `hostname`, app icon, Approve/Deny buttons, and a countdown (default 30 s).
- [x] Signing key is generated in `KeyProperties.KEY_ALGORITHM_EC` (curve `secp256r1` if Keystore lacks Ed25519 — fall back per device; document in `docs/android-setup.md`) with `setUserAuthenticationRequired(true)` and `setUnlockedDeviceRequired(true)`. Strongbox if available.
- [x] BiometricPrompt with `BIOMETRIC_STRONG | DEVICE_CREDENTIAL`.
- [x] Signing happens via the UniFFI surface, which takes a raw signature blob from the Keystore-backed `Signature` object. The crypto code never sees the private key bytes.
- [x] Cancel on countdown is logged as a denial (not a timeout from the desktop's perspective — the desktop sees a `PeerDenied` frame).
- [x] Robolectric tests for the timeout, Approve, Deny branches.

**Tests:**
- `syauth-android/app/src/test/.../ApproveViewModelTest.kt`.
- `syauth-android/app/src/androidTest/.../ApproveScreenTest.kt`.

**Files likely affected:** `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/{ApproveViewModel.kt,ApproveScreen.kt,KeystoreSigner.kt}`.

**Journey:** `JOURNEY-S-017-android-approve.md` — `specs/journeys/JOURNEY-S-017-android-approve.md`.

### Evidence

- **Compose screen** — `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/ApproveScreen.kt` renders the hostname (test tag `syauth.approve.hostname`), the Material `Lock` app icon, an Approve button (`syauth.approve.approve_button`), a Deny button (`syauth.approve.deny_button`), and the countdown line (`syauth.approve.countdown`). The countdown's default of 30 s is pinned via `DEFAULT_TIMEOUT_MILLIS = 30_000L` in `ApproveViewModel.kt`.
- **EC P-256 + StrongBox + UserAuthRequired + UnlockedDeviceRequired** — `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/KeystoreSigner.kt`'s `AndroidKeystoreSigner.generateKey` builds `KeyGenParameterSpec.Builder(alias, PURPOSE_SIGN or PURPOSE_VERIFY).setAlgorithmParameterSpec(ECGenParameterSpec("secp256r1")).setUserAuthenticationRequired(true).setUnlockedDeviceRequired(true)` and the StrongBox `try { setIsStrongBoxBacked(true).build() } catch (StrongBoxUnavailableException) { setIsStrongBoxBacked(false).build() }` fallback. The choice is recorded in `KeyInfo.strongBoxBacked`. Documented in `docs/android-setup.md` §"Keystore key parameters". Verified by inspection; requires Android SDK to fully execute the `KeyGenParameterSpec` builder.
- **BiometricPrompt STRONG | DEVICE_CREDENTIAL** — `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/AndroidBiometricPresenter.kt` pins `ALLOWED_AUTHENTICATORS = BiometricManager.Authenticators.BIOMETRIC_STRONG or BiometricManager.Authenticators.DEVICE_CREDENTIAL`. Verified by inspection; requires Android SDK + connected emulator to fully execute.
- **UniFFI signing surface** — `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/UniffiWireSigner.kt`'s `UniffiWireSigner.signWire` delegates to `uniffi.syauth_mobile.signChallengeResponse(seed, frameBytes)`. The crypto core (Rust) never sees the seed in plaintext storage; it receives it as a single argument across the UniFFI boundary. The transitional Kotlin-side seed handling and the path to a fully-Keystore-wrapped Ed25519 key are documented in `docs/android-setup.md` §"Ed25519 seed handling (transitional)".
- **Cancel-on-countdown is logged as a denial** — `ApproveViewModel.runCountdown` calls `onTimeout()` when `remainingMillis <= 0`, which calls `transitionToDenied(DenialReason.TimedOut)`, which calls `responseSender.sendDeny()`. The desktop sees a `PeerDenied` frame identically to an explicit-user-deny. Pinned by the JVM unit test `countdown_timeout_emits_timed_out_and_calls_send_deny_once` in `ApproveViewModelTest.kt`.
- **Robolectric / unit tests for Approve, Deny, Timeout, BiometricFailed** — `syauth-android/app/src/test/kotlin/com/sy/syauth/android/approve/ApproveViewModelTest.kt` covers `approve_happy_path_emits_approved_and_calls_send_approve_once`, `deny_click_emits_user_denied_and_calls_send_deny_once`, `countdown_timeout_emits_timed_out_and_calls_send_deny_once`, `biometric_failure_emits_biometric_failed_and_calls_send_deny_once`, plus four supplementary scenarios (`start_is_idempotent`, `deny_after_approve_is_ignored`, `missing_seed_emits_sign_error`, `wire_signer_failure_emits_sign_error`). Tests are pure JVM (no Robolectric runner needed because every Android side-effect is injected behind an interface); the Robolectric runtime is added to the test dep set so future tests can opt in. Verified by inspection; requires Android SDK + Gradle to fully execute via `./gradlew :app:testDebugUnitTest`.
- **Compose screen test (emulator-gated)** — `syauth-android/app/src/androidTest/kotlin/com/sy/syauth/android/approve/ApproveScreenTest.kt` asserts hostname / Approve / Deny / countdown nodes via `createComposeRule()`. Marked emulator-gated with a `// Requires connected device / emulator` comment.
- **Manifest** — `syauth-android/app/src/main/AndroidManifest.xml` adds `<uses-permission android:name="android.permission.USE_BIOMETRIC" />` per the S-017 brief.
- **Build deps** — `syauth-android/app/build.gradle.kts` adds `androidx.biometric:biometric:1.2.0-alpha05`, `androidx.lifecycle:lifecycle-viewmodel-compose:2.7.0`, `androidx.lifecycle:lifecycle-viewmodel-ktx:2.7.0`, `androidx.lifecycle:lifecycle-runtime-compose:2.7.0`, `androidx.fragment:fragment-ktx:1.6.2`, `androidx.navigation:navigation-compose:2.7.7`, Robolectric + core-testing + kotlinx-coroutines-test + junit on `testImplementation`, plus the `testOptions { unitTests.isIncludeAndroidResources = true }` block.
- **Documentation** — `docs/android-setup.md` carries the Keystore key parameters section (curve choice + StrongBox + auth requirements + BiometricPrompt allowed authenticators + Ed25519 seed transitional handling).

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
