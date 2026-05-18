//! `syauth-pam` — Linux PAM module for the syauth phone-as-key unlock flow.
//!
//! This crate is built as both a `cdylib` (the actual `libpam_syauth.so`
//! consumed by libpam's loader) and an `rlib` (so the panic-boundary helper
//! can be exercised by ordinary Rust unit tests — see `entry::tests`).
//!
//! # S-008 state
//!
//! `pam_sm_authenticate` is a thin Unix-socket RPC client to the
//! `syauth-presenced` daemon (SPEC §3 scope item #11). The PAM module
//! no longer drives BlueZ directly; the daemon owns the GATT + advertise
//! stack and the heavy crypto. The module's only knob is the
//! `socket=<path>` libpam argument (SPEC §3 scope item #12). On
//! socket-missing / connect-refused / write-fail / response-timeout it
//! returns `PAM_AUTHINFO_UNAVAIL` within ≤ 50 ms (SPEC §4.3
//! daemon-down latency) so the stack falls through to FIDO / password.
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
