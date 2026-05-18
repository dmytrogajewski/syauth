//! PAM entry points and the panic boundary helper.
//!
//! Every `pam_sm_*` symbol exported here:
//!
//! 1. is declared `#[unsafe(no_mangle)] pub unsafe extern "C" fn` so the
//!    dynamic loader resolves the name verbatim and the libpam C ABI is
//!    honored;
//! 2. delegates its body to [`run_entry`], whose outermost expression is
//!    `std::panic::catch_unwind(|| { ... }).unwrap_or(PAM_AUTH_ERR)`. A panic
//!    that escapes the closure becomes a fail-closed return — never
//!    `PAM_SUCCESS`, never an unwind across the FFI boundary.
//!
//! S-009 fills the `pam_sm_authenticate` body with a real call into
//! [`crate::auth::authenticate`], which drives a challenge/response against
//! an injectable `BtPeer`. `pam_sm_setcred` still returns `PAM_SUCCESS`
//! (no credentials to set). `pam_sm_acct_mgmt` remains a stub that returns
//! `PAM_AUTHINFO_UNAVAIL` — account management is out of scope for v0.1.

use std::{
    ffi::c_int,
    os::raw::{c_char, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
};

use syslog::{Facility, Formatter3164};

use crate::{auth, config::Config};

// -----------------------------------------------------------------------------
// PAM return-code constants
// -----------------------------------------------------------------------------
//
// We pin a minimum subset of the libpam return codes locally rather than
// pulling in `pam-sys` for four integers. The values are the canonical
// Linux-PAM constants from `<security/_pam_types.h>` and are ABI-stable.
// When S-009 starts calling into libpam (e.g. `pam_get_item`) we can revisit
// and pull a curated binding then.

/// `PAM_SUCCESS` — the call succeeded.
pub const PAM_SUCCESS: c_int = 0;

/// `PAM_AUTH_ERR` — authentication failure / generic deny. The catch-all
/// fail-closed return for this crate, including the panic-caught path.
pub const PAM_AUTH_ERR: c_int = 7;

/// `PAM_AUTHINFO_UNAVAIL` — the module cannot reach the authentication
/// authority right now. PAM stacks may fall through to the next module on
/// this code, which is exactly what we want when the phone isn't reachable.
pub const PAM_AUTHINFO_UNAVAIL: c_int = 9;

/// `PAM_IGNORE` — included for completeness and so S-009 does not need to
/// touch this constant block when it adds a "module disabled in config"
/// path. **Unused in S-008.**
pub const PAM_IGNORE: c_int = 25;

// -----------------------------------------------------------------------------
// Logging
// -----------------------------------------------------------------------------

/// Syslog identity tag used by every log line emitted from this crate.
///
/// The e2e test in `tests/pam_smoke.rs` greps `journalctl -t pam_syauth`, so
/// this string is load-bearing — do not change without updating the test.
const SYSLOG_TAG: &str = "pam_syauth";

/// The exact stub line emitted by every `pam_sm_authenticate` /
/// `pam_sm_acct_mgmt` invocation in S-008.
///
/// The e2e harness greps for this substring verbatim. Format is intentionally
/// stable: `syauth: <verb> <result> reason=<kebab-token>`.
const STUB_LOG_LINE: &str = "syauth: unlock unavailable reason=stub";

/// The log line emitted when the panic boundary in [`run_entry`] catches a
/// panic. The reason field is `reason=panic` so an operator can grep for it.
const PANIC_LOG_LINE: &str = "syauth: unlock unavailable reason=panic";

/// The log line emitted by `pam_sm_setcred` when it returns `PAM_SUCCESS`.
/// Auth modules MUST implement setcred (per `.agents/skills/pam/SKILL.md`
/// "Common Failure Modes"), but in the stub state there are no credentials
/// to set. We still log so every return path is observable.
const SETCRED_LOG_LINE: &str = "syauth: setcred noop reason=stub";

/// Build the formatter used for every syslog write in this crate.
///
/// Pulled out so the e2e test (and unit tests) can confirm the facility/tag
/// invariants without copy-pasting the literals.
fn formatter() -> Formatter3164 {
    Formatter3164 {
        facility: Facility::LOG_AUTHPRIV,
        hostname: None,
        process: SYSLOG_TAG.to_owned(),
        pid: std::process::id(),
    }
}

/// Attempt to log `message` at `info` severity to the local syslog daemon.
///
/// Errors are *swallowed* deliberately: if syslog is unreachable (running
/// inside a chroot without `/dev/log`, or in CI with no daemon), we still
/// want PAM to return a deterministic code rather than aborting. The cost is
/// that one observability path becomes silent in those environments — the
/// e2e test documents the assumption.
fn log_info(message: &str) {
    if let Ok(mut logger) = syslog::unix(formatter())
        && logger.info(message).is_err()
    {
        // intentional swallow — see function doc.
    }
}

// -----------------------------------------------------------------------------
// Panic boundary helper
// -----------------------------------------------------------------------------

/// Wrap a closure that returns a PAM return code in a panic boundary.
///
/// The contract:
///
/// * The outermost expression of every `pam_sm_*` body is exactly
///   `run_entry(|| { ... })`. This is enforced by inspection; see
///   `tests/pam_smoke.rs` Phase 3 and the per-entry-point unit tests below.
/// * On any panic caught by `catch_unwind`, the function returns
///   `PAM_AUTH_ERR` — never `PAM_SUCCESS`, never `PAM_AUTHINFO_UNAVAIL`,
///   because a panic indicates a bug, not a missing peer.
/// * The panic-caught path logs `PANIC_LOG_LINE` so the failure is
///   observable in syslog even though the closure itself never ran to
///   completion.
///
/// We use [`AssertUnwindSafe`] because the closure captures no mutable state
/// that would be visible after the unwind — every entry point in this crate
/// is reentrant and stateless (per `.agents/skills/pam/SKILL.md` rule 5).
pub(crate) fn run_entry<F>(f: F) -> c_int
where
    F: FnOnce() -> c_int,
{
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(code) => code,
        Err(_) => {
            log_info(PANIC_LOG_LINE);
            PAM_AUTH_ERR
        }
    }
}

// -----------------------------------------------------------------------------
// PAM exported entry points
// -----------------------------------------------------------------------------
//
// Each `extern "C"` symbol below:
//
// * matches the libpam ABI for the corresponding module type exactly:
//   `int pam_sm_*(pam_handle_t *pamh, int flags, int argc, const char **argv)`;
// * is `#[unsafe(no_mangle)]` so the dynamic linker resolves the name as
//   `pam_sm_*` verbatim, with no Rust mangling. Verified by the e2e harness
//   running `nm -D --defined-only target/release/libpam_syauth.so`;
// * delegates immediately to `run_entry`, so the outermost expression of the
//   body is the `catch_unwind`-wrapped closure.

/// `pam_sm_authenticate` — the PAM stack's primary entry into an auth module.
///
/// # Safety
///
/// This function is part of the libpam C ABI. libpam guarantees:
/// * `pamh` is a valid opaque pointer to a `pam_handle_t` for the duration
///   of the call.
/// * `argv` is either null or points to `argc` valid `*const c_char` entries.
///
/// In the S-008 stub we read neither pointer; the parameters exist to match
/// the ABI signature.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_authenticate(_pamh: *mut c_void, _flags: c_int, argc: c_int, argv: *const *const c_char) -> c_int {
    run_entry(|| {
        // SAFETY: libpam guarantees `argv` is either null or points
        // to `argc` valid `*const c_char` entries. The helper
        // returns an empty slice on any malformed pointer or
        // negative `argc`, so the `Config::from_pam_argv` call is
        // total even on hostile callers.
        let argv_strings = unsafe { collect_pam_argv(argc, argv) };
        let argv_refs: Vec<&str> = argv_strings.iter().map(String::as_str).collect();
        let cfg = Config::from_pam_argv(&argv_refs);
        let outcome = auth::authenticate(&cfg);
        log_info(&format!(
            "syauth: unlock {} reason={} peer_id={}",
            if outcome.is_success() { "success" } else { "denied" },
            outcome.reason(),
            outcome.peer_id().unwrap_or(auth::LAST_LOG_UNKNOWN_PEER),
        ));
        outcome.to_pam_code()
    })
}

/// Copy libpam's `argv` into owned `String`s.
///
/// libpam passes module arguments as a `(argc, argv)` pair where
/// `argv[i]` is a NUL-terminated C string. We copy each entry into
/// an owned `String` so the caller can build a `&[&str]` slice
/// without aliasing the libpam-owned memory across function
/// boundaries. Non-UTF-8 entries are silently dropped — the only
/// PAM arguments we recognise are the documented
/// `socket=<path>` argument, which is always ASCII in practice.
///
/// # Safety
///
/// `argv` must be either null or point to `argc` valid C-string
/// pointers, each of which is either null or points to a
/// NUL-terminated byte sequence. libpam guarantees this on every
/// `pam_sm_*` entry point.
unsafe fn collect_pam_argv(argc: c_int, argv: *const *const c_char) -> Vec<String> {
    if argv.is_null() || argc <= 0 {
        return Vec::new();
    }
    let len = match usize::try_from(argc) {
        Ok(n) => n,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        // SAFETY: `i < argc` and `argv` carries `argc` valid
        // entries per the function's safety doc.
        let ptr = unsafe { *argv.add(i) };
        if ptr.is_null() {
            continue;
        }
        // SAFETY: `ptr` is a valid NUL-terminated C string per
        // libpam's contract.
        let cstr = unsafe { std::ffi::CStr::from_ptr(ptr) };
        if let Ok(s) = cstr.to_str() {
            out.push(s.to_owned());
        }
    }
    out
}

/// `pam_sm_setcred` — required companion of `pam_sm_authenticate` for any
/// `auth` module (per `.agents/skills/pam/SKILL.md` "Common Failure Modes":
/// without this the login loops after success). The stub has no credentials
/// to set and returns `PAM_SUCCESS`.
///
/// # Safety
///
/// Same as `pam_sm_authenticate`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_setcred(_pamh: *mut c_void, _flags: c_int, _argc: c_int, _argv: *const *const c_char) -> c_int {
    run_entry(|| {
        log_info(SETCRED_LOG_LINE);
        PAM_SUCCESS
    })
}

/// `pam_sm_acct_mgmt` — the `account` module hook. We expose it so a PAM
/// service file can stack syauth as both an `auth` and `account` module.
/// In the stub state there is no account information to consult, so we
/// return `PAM_AUTHINFO_UNAVAIL` and log the same stub line.
///
/// # Safety
///
/// Same as `pam_sm_authenticate`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_acct_mgmt(_pamh: *mut c_void, _flags: c_int, _argc: c_int, _argv: *const *const c_char) -> c_int {
    run_entry(|| {
        log_info(STUB_LOG_LINE);
        PAM_AUTHINFO_UNAVAIL
    })
}

// -----------------------------------------------------------------------------
// Tests — pure-Rust coverage of the panic boundary and constants
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // Journey: specs/journeys/JOURNEY-S-008-pam-skeleton.md
    use super::*;

    /// TC-08: a panic inside the wrapped closure is translated into
    /// `PAM_AUTH_ERR`, not propagated as an unwind.
    #[test]
    fn run_entry_catches_panic_and_returns_auth_err() {
        let got = run_entry(|| panic!("simulated bug"));
        assert_eq!(got, PAM_AUTH_ERR, "panic must be translated to PAM_AUTH_ERR; got {got}");
    }

    /// A closure that returns a value passes that value through unchanged.
    #[test]
    fn run_entry_passes_through_return_value() {
        let got = run_entry(|| PAM_AUTHINFO_UNAVAIL);
        assert_eq!(got, PAM_AUTHINFO_UNAVAIL);
    }

    /// The pinned PAM constants must match the Linux-PAM ABI values from
    /// `<security/_pam_types.h>`.
    #[test]
    fn pam_constants_match_linux_pam_abi() {
        // PAM_SUCCESS = 0, PAM_AUTH_ERR = 7, PAM_AUTHINFO_UNAVAIL = 9,
        // PAM_IGNORE = 25.
        assert_eq!(PAM_SUCCESS, 0);
        assert_eq!(PAM_AUTH_ERR, 7);
        assert_eq!(PAM_AUTHINFO_UNAVAIL, 9);
        assert_eq!(PAM_IGNORE, 25);
    }

    /// The stub log line is the exact substring the e2e harness greps for.
    /// Drift in either direction breaks `tests/pam_smoke.rs`.
    #[test]
    fn stub_log_line_is_exact() {
        assert_eq!(STUB_LOG_LINE, "syauth: unlock unavailable reason=stub");
    }

    /// Formatter sets the syslog tag and facility documented in the journey.
    #[test]
    fn formatter_uses_authpriv_and_pam_syauth_tag() {
        let f = formatter();
        assert_eq!(f.process, SYSLOG_TAG);
        assert!(matches!(f.facility, Facility::LOG_AUTHPRIV));
    }
}
