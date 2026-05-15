# JOURNEY-S-014: `syauth-mobile` — UniFFI surface

<!-- Authored per .agents/skills/journey/SKILL.md template. -->

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md](../syauth/ROADMAP.md) — item **S-014**.
- Feature: a thin UniFFI-exported Rust crate that the Android companion (S-015..S-018) consumes as an AAR. Surface is four functions: `parse_invite_uri`, `verify_challenge_frame`, `sign_challenge_response`, `oob_code_for_bond`, plus a single error enum.

## 1. Journey

When **the Android app developer (Sam) who needs to call into the syauth protocol core from Kotlin without hand-writing JNI** I want to **drop a single `syauth_mobile.aar` into `syauth-android/app/libs/` and get type-safe Kotlin bindings for the four protocol entry points the phone-side flow needs** so I can **build the pairing screen (S-016) and the approve screen (S-017) against the *same* protocol code that runs in `pam_syauth.so`, with zero risk of an algorithm or wire-format drift between desktop and phone**.

## 2. CJM

S-014 is the seam that lets the Android app consume `syauth-core` without re-implementing the Frame layout, the BLAKE3 MAC, the Ed25519 signing rule, or the HKDF-SHA256 OOB derivation in Kotlin. Re-implementing any of those in Kotlin is exactly the failure mode SPEC §4.1 calls out — "the phone and the desktop MUST agree on byte-for-byte semantics" — and the prrr project already proved that a UniFFI 0.29 cdylib + generated Kotlin is the right tool for this seam.

Two design forces dominate the step:

1. **Mirror prrr-mobile.** The orchestrator brief is explicit: every Cargo.toml line, the `build.rs`, the `include_scaffolding!` invocation, the `[lib] crate-type` triple, and the AAR build script must match prrr-mobile's pattern. Deviating creates a maintenance dual that future agents will have to keep in sync by hand.
2. **No production panics.** The four exported functions sit at an FFI boundary. A Rust panic across that boundary is UB on Android. Every input is fallibly validated; every error returns a typed `MobileError::*` variant whose `#[error("...")]` string is opaque (no secret bytes leaked into log lines).

### Phase 1: Build the Rust crate

**User Intent:** A `cargo build -p syauth-mobile` succeeds on a desktop host without the Android NDK installed.

**Actions:** Sam runs `cargo build -p syauth-mobile` or `make build` from the worktree root.

**Pain / Risk:**
- UniFFI's `generate_scaffolding` runs in `build.rs`; if the UDL is malformed the build fails with a parser error rather than a Rust error. Mitigation: the UDL file is short (four functions, one error enum), reviewed in this journey, and matches the `prrr-mobile` syntax line-for-line.
- The crate-type triple (`cdylib`, `staticlib`, `lib`) means three different link artifacts. On hosts without `ld` configured for `cdylib`, the build can fail. Mitigation: the workspace already builds the `syauth-pam` cdylib on the CI host, so the linker path is proven.
- Cyclic dep risk: `syauth-mobile` imports `OOB_WORDS` from `syauth-cli`. Mitigation: we duplicate the OOB derivation in the mobile crate rather than depend on `syauth-cli` (which would pull in `bluer`, `clap`, etc., bloating the AAR). The duplication is annotated with a comment naming `crates/syauth-cli/src/oob.rs` as the source of truth and asserted by a deterministic-fixture round-trip test in `tests/oob_cross_crate.rs` (out of scope for this step but documented).

**Success Signal:** `make build` exits 0, producing both `target/release/libsyauth_mobile.so` (cdylib) and `target/release/libsyauth_mobile.a` (staticlib).

### Phase 2: Run the Rust unit tests

**User Intent:** Sam runs `cargo test -p syauth-mobile --lib` and sees every UDL-exported function exercised by at least one happy-path and one negative-path test.

**Actions:** `make test` from the repo root, or `cargo test -p syauth-mobile --lib`.

**Pain / Risk:**
- `parse_invite_uri` is the largest single attack surface (user-supplied string). Tests must cover: missing scheme, wrong scheme, missing path, missing pubkey query param, missing host query param, non-hex pubkey, short pubkey, long pubkey, valid invite. Mitigation: explicit `#[test]` cases for each.
- `verify_challenge_frame` requires building a wire-format frame with the matching BLAKE3 tag from `syauth-core::mac::compute_tag`. Without that test helper the test is unwritable. Mitigation: we re-export the `syauth-core` types we need and the tests build the frame from the typed `Frame` struct.
- `sign_challenge_response` returns 64 bytes. Mistakenly returning the body bytes (which start with a 1-byte version + 16-byte nonce) would also be a non-empty `Vec<u8>` and pass a naive "length > 0" assertion. Mitigation: the happy-path test verifies the signature with the matching pubkey via `syauth-core::sign::verify_frame`, not just length.
- `oob_code_for_bond` is byte-deterministic. A regression in the HKDF info string would silently change the words. Mitigation: a pinned-key fixture asserts the *exact* four-word output for a known bond key, mirroring the test in `crates/syauth-cli/src/oob.rs`.

**Success Signal:** `cargo test -p syauth-mobile --lib` reports at least 8 passed tests (2 per UDL-exported function).

### Phase 3: Build the AAR

**User Intent:** Sam (or CI with the NDK installed) runs `make android-aar` and gets `crates/syauth-mobile/target/syauth_mobile.aar` ready to drop into `syauth-android/app/libs/`.

**Actions:** `NDK_HOME=/path/to/android-ndk make android-aar`.

**Pain / Risk:**
- Most developer boxes don't have the Android NDK installed. The script must fail loudly *before* doing any work and tell the operator exactly which env var to set. Mitigation: `scripts/build_aar.sh` checks `NDK_HOME` early and exits with a documented "install the NDK from <link>" message.
- The `uniffi-bindgen` binary is not in the workspace by default — `cargo install uniffi-bindgen --version 0.29` is the one-time setup. Mitigation: the script checks for the binary and prints the install command on failure.
- `cargo ndk` is similarly a per-developer install. Mitigation: same check + same install hint.
- Without the toolchain installed, `make android-aar` cannot produce the AAR. To keep the DoD checkbox honest, we run the script in **dry-run mode** (a mode where it stops after the prerequisite checks and reports "would build N targets") to prove the pipeline is in place, and the DoD checkbox is annotated `[x] (dry-run; requires NDK)`.

**Success Signal:** On an NDK-equipped host, `make android-aar` exits 0 and the AAR exists at the documented path. On a host without the NDK, `make android-aar DRY_RUN=1` exits 0 and prints the build plan.

### Phase 4: Smoke the public surface

**User Intent:** Sam runs the example smoke test to convince themselves the four functions are callable from outside the crate (i.e. the public API is actually public and matches the UDL).

**Actions:** `cargo run -p syauth-mobile --example smoke`.

**Pain / Risk:**
- `cargo test --workspace` does not build examples by default, so a broken example does not break the lint gate. Mitigation: `cargo build -p syauth-mobile --examples` runs in the smoke test inside the example itself, and we run `cargo run -p syauth-mobile --example smoke` in `make test` indirectly via `--all-targets`.
- The example must use the same public surface the UDL exports — re-importing internals would be a lie. Mitigation: the example imports only `syauth_mobile::{parse_invite_uri, verify_challenge_frame, sign_challenge_response, oob_code_for_bond, Invite, MobileError}`, the exact set the UDL re-exports.

**Success Signal:** The example exits 0 and prints `OK` after calling each function.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Re-implementing protocol in Kotlin = wire-format drift risk | Phase 1 | UniFFI auto-generates Kotlin; drift impossible by construction. |
| NDK absent on most developer boxes | Phase 3 | `DRY_RUN=1` mode produces a build plan; full build runs only on CI. |
| OOB word table duplication in two crates | Phase 1 | Document `syauth-cli/src/oob.rs` as the source of truth; pin a cross-crate determinism test. |
| Panic across FFI = UB | Phase 2 | Every UDL function returns `Result<_, MobileError>`; no `unwrap`/`expect` outside `#[cfg(test)]`. |

### North Star Summary

The Android app developer never touches JNI, never duplicates a single line of protocol code in Kotlin, and gets a `syauth_mobile.aar` artifact that drops into the Android Gradle project. Every protocol bug is caught on the desktop side (where the test loop is fast) and ships to the phone for free. The four-function surface is the *only* contract the phone team and the desktop team have to agree on; everything else is generated.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] One `cargo run -p syauth-mobile --example smoke` invocation exercises every public function.
- [x] `make build` produces the cdylib without any extra toolchain setup.

### Onboarding Clarity
- [x] Every UDL-exported function has a doc comment naming its inputs, outputs, and error variants.
- [x] `MobileError` variants carry an opaque `reason: String` — never a secret byte.

### Production-Ready Defaults
- [x] The build script defaults to `release`; `DEBUG=1` switches to debug.
- [x] `parse_invite_uri` accepts URIs with arbitrary query-param order.

### Golden Path Quality
- [x] Happy-path tests verify the *cryptographic* round-trip (sign → verify with matching pubkey, compute_tag → verify_challenge_frame), not just non-empty output.
- [x] The OOB word table is byte-identical to `syauth-cli/src/oob.rs::OOB_WORDS`.

### Decision Load
- [x] Four functions, one error enum. No optional config knobs.
- [x] All input types are primitives (`String`, `Vec<u8>`) — no exotic types to teach the Kotlin caller.

### Progressive Complexity
- [x] Adding a new UDL function is one new entry in `mobile.udl` + one new Rust function. No scaffolding gymnastics.

### Error Quality
- [x] `MobileError::InvalidInvite { reason }` names the missing field; `InvalidKey { reason }` names the expected length.
- [x] Error strings are stable enough for a Kotlin caller to `when (e.reason.contains("pubkey")) { ... }` without breaking on patch releases.

### Failure Safety
- [x] No `unsafe` outside UniFFI-generated scaffolding; crate-level `#![allow(unsafe_code)]` is documented with a SAFETY comment naming UniFFI as the only producer of unsafe code (mirrors prrr-mobile's pattern).
- [x] No production panic path: every input is fallibly validated; tests pin the panic-free property.

### Runtime Transparency
- [x] All four functions are pure (no I/O, no globals). Logging is deferred to the Kotlin caller.

## 4. Mirror of prrr-mobile

S-014 is a faithful mirror of `~/sources/prrr/prrr-mobile`. Each row below names the file in prrr-mobile, the corresponding file in syauth-mobile, and the only deltas.

| prrr-mobile | syauth-mobile | Delta |
|-------------|---------------|-------|
| `Cargo.toml` `[lib] crate-type = ["cdylib", "staticlib", "lib"]` | `crates/syauth-mobile/Cargo.toml` same triple | None. |
| `Cargo.toml` `[build-dependencies] uniffi = { version = "0.29", features = ["build"] }` | same | None. |
| `Cargo.toml` `[dependencies] uniffi = { version = "0.29", features = ["cli"] }` | `uniffi = "0.29"` (no `cli` feature; we never invoke the bindgen CLI from this crate) | The `cli` feature pulls in `clap` and a binary entry point we don't need. |
| `build.rs` calls `uniffi::generate_scaffolding("src/mobile.udl").unwrap()` | same path, same call | None. |
| `src/lib.rs` `uniffi::include_scaffolding!("mobile");` | same | None. |
| `src/lib.rs` `#![allow(clippy::empty_line_after_doc_comments)]` (UniFFI-generated artifact) | same | None. |
| `src/lib.rs` crate-level allow for unsafe (commented out `[lints.rust] unsafe_code = "deny"` because UniFFI generates unsafe) | `crates/syauth-mobile/src/lib.rs` carries `#![allow(unsafe_code)]` with SAFETY comment pinning UniFFI as sole producer | The workspace-level `unsafe_code = "deny"` is *not* overridden at the crate `[lints]` level; we use the source-level `#![allow(unsafe_code)]` so the override is visible at code-review time and lints elsewhere remain strict. Documented in lib.rs. |
| `src/mobile.udl` `namespace prrr_mobile {};` + `[Error] interface MobileError { ... };` | `crates/syauth-mobile/src/mobile.udl` `namespace syauth_mobile {};` + a four-function namespace block + `[Error] interface MobileError` | Function set differs (syauth's four functions vs prrr's connection/config surface). UDL syntax identical. |
| `scripts/build-android.sh` | `scripts/build_aar.sh` | Renames `prrr-mobile`→`syauth-mobile`, `prrr_mobile.aar`→`syauth_mobile.aar`, adds `DRY_RUN=1` mode for hosts without NDK. |
| `scripts/generate-bindings.sh` | inlined into `scripts/build_aar.sh` | We fold the bindings-generation step into the AAR script to keep the syauth surface to one `make` target. |
| `examples/kotlin_example.kt` | `crates/syauth-mobile/examples/smoke.rs` | We ship a Rust smoke example (callable as `cargo run -p syauth-mobile --example smoke`) rather than a Kotlin one, because S-015 ships the Kotlin example in the Android Gradle project. The Rust smoke covers the same purpose — proving the public surface is callable from outside the crate. |

### Unsafe-code exception (audit trail)

The workspace `[workspace.lints.rust] unsafe_code = "deny"` rule is overridden in **`crates/syauth-mobile/src/lib.rs`** with `#![allow(unsafe_code)]`. The exception scope is the UniFFI-generated scaffolding only — UniFFI 0.29's `generate_scaffolding` emits `unsafe extern "C"` ABI shims (FFI boundaries are inherently `unsafe` in Rust). The lib.rs banner names UniFFI as the sole producer and points at the audit row T-101 in the threat model (Rust↔C/JNI FFI). No hand-written `unsafe` block exists in this crate.

This mirrors prrr-mobile's pattern: prrr-mobile commits out the `[lints.rust] unsafe_code = "deny"` block in its `Cargo.toml`; we instead carry the workspace deny and override at the file level so the exception is visible in source review (and so other files in the crate, were any added, would still trip the workspace rule unless they too opted out).

## 5. Compliance & Open Questions

- [x] DoR satisfied: S-002 (frame), S-003 (replay), S-004 (sign + mac), S-005 (bond), S-006 (KeyStore) all on master.
- [x] No production `unwrap()`/`expect()` — `cargo clippy` `-D warnings` blocks it.
- [x] No `TODO` comments — every loose end is either filed as a follow-up roadmap item or tested.
- [x] No emojis in source files; the OOB word table is duplicated with a comment naming `crates/syauth-cli/src/oob.rs` as the source of truth.
- [ ] Open: cross-crate determinism test (assert `syauth_mobile::oob_code_for_bond` == `syauth_cli::oob::oob_code_for_bond` for a fixed key). Filed as a follow-up; not part of S-014's DoD. The in-crate fixture covers byte-determinism within syauth-mobile; the cross-crate property is enforced by both crates pinning the same fixture output.
