//! `syauth-pam` — Linux PAM module for the syauth phone-as-key unlock flow.
//!
//! This crate is built as both a `cdylib` (the actual `libpam_syauth.so`
//! consumed by libpam's loader) and an `rlib` (so the panic-boundary helper
//! can be exercised by ordinary Rust unit tests — see `entry::tests`).
//!
//! # Stub state (S-008)
//!
//! In this roadmap item the three required entry points return
//! `PAM_AUTHINFO_UNAVAIL` (or `PAM_SUCCESS` for `pam_sm_setcred`, per the
//! libpam contract for `auth` modules). No real authentication runs yet — the
//! point of S-008 is to prove the FFI boundary is sound. S-009 wires the mock
//! transport behind the same entry points.
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

pub mod entry;
