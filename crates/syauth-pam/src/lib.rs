//! `syauth-pam` — Linux PAM module for the syauth phone-as-key unlock flow.
//!
//! This crate is built as both a `cdylib` (the actual `libpam_syauth.so`
//! consumed by libpam's loader) and an `rlib` (so the panic-boundary helper
//! can be exercised by ordinary Rust unit tests — see `entry::tests`).
//!
//! # S-009 state
//!
//! `pam_sm_authenticate` now drives a real challenge / response against an
//! injectable `BtPeer`, verifies signature + tag + nonce-freshness, and
//! returns the right PAM code. The mock peer is wired in by the integration
//! tests in `tests/pam_e2e.rs`; production builds (where the `test-mock`
//! Cargo feature is off and `cfg!(test)` is false) fall back to a stub real
//! peer that returns `TransportError::NotPaired` — the real BlueZ peer
//! arrives in S-019.
//!
//! `pam_sm_setcred` still returns `PAM_SUCCESS` (no creds to set). The
//! `pam_sm_acct_mgmt` symbol still returns `PAM_AUTHINFO_UNAVAIL` — account
//! management is not in scope for v0.1.
//!
//! # Unsafe code policy
//!
//! The workspace declares `[lints.rust] unsafe_code = "deny"` so the rest of
//! syauth is forbidden from writing raw `unsafe` blocks. This crate is the
//! single documented exception: every PAM entry point must be
//! `pub unsafe extern "C" fn` to match the libpam ABI. We therefore set
//! `unsafe_code = "allow"` at the crate level in `Cargo.toml`. Every internal
//! `unsafe` block still carries a `// SAFETY:` comment naming the invariant
//! it relies on, per `.agents/skills/ffi/SKILL.md`.
//!
//! # Logging policy
//!
//! No `println!` / `eprintln!` ever appears in this crate. All output is
//! routed through the `syslog` crate with facility `LOG_AUTHPRIV` and
//! identity `pam_syauth`. Stdout/stderr inside a PAM-driven login process is
//! either invisible or leaks into the user's terminal — both are bugs.

pub mod auth;
pub mod config;
pub mod entry;
