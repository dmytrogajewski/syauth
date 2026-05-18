//! Runtime configuration for `pam_sm_authenticate`.
//!
//! S-008 reduces the configuration surface to two knobs:
//!
//! - [`Config::bond_dir`] — directory holding `bonds.toml` and `last.log`.
//!   Defaults to [`DEFAULT_BOND_DIR`] (SPEC §4.4); overridable for tests via
//!   the [`Config::with_bond_dir`] builder. PAM still needs to read
//!   `bonds.toml` to pick the `peer_id` that goes on the wire (the daemon
//!   owns the bond_key + pubkey).
//! - [`Config::socket_path`] — the Unix-socket path PAM connects to.
//!   Default `${XDG_RUNTIME_DIR}/syauth/auth.sock`; falls back to
//!   `/run/user/$UID/syauth/auth.sock` when `XDG_RUNTIME_DIR` is unset
//!   (SPEC §8 Risks row). Overridable via the libpam `socket=<path>`
//!   argument so test harnesses can point at a mock daemon
//!   (SPEC §3 scope item #12).
//!
//! The legacy S-009 `mock_peer_enabled`, `adapter_id`, and
//! `response_timeout` knobs are gone — `pam_sm_authenticate` no longer
//! drives BlueZ directly (SPEC §3 scope item #11).

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// Default directory for `bonds.toml` and `last.log`. Mirrors SPEC §4.4.
///
/// Tests override this via [`Config::with_bond_dir`] so they never touch the
/// real path.
pub const DEFAULT_BOND_DIR: &str = "/var/lib/syauth";

/// Default budget for the daemon round-trip. Matches the daemon's
/// own `DEFAULT_AUTH_TIMEOUT` (8000 ms) so the daemon's tokio
/// `time::timeout` trips first; the PAM-side budget is a
/// belt-and-suspenders fallback. 8000 ms accommodates real
/// BiometricPrompt reaction time on the phone (~4-5s typical).
pub const DEFAULT_AUTH_TIMEOUT: Duration = Duration::from_millis(8_000);

/// Name of the file appended to under [`Config::bond_dir`] on every
/// `authenticate` call.
pub const LAST_LOG_FILENAME: &str = "last.log";

/// Environment variable holding the per-user runtime directory. SPEC §8
/// Risks row: "Fall back to `/run/user/$UID/syauth/auth.sock` if
/// XDG_RUNTIME_DIR is unset".
pub const XDG_RUNTIME_DIR_ENV: &str = "XDG_RUNTIME_DIR";

/// Per-user runtime-directory prefix used when [`XDG_RUNTIME_DIR_ENV`]
/// is unset. The fallback path is
/// `<DEFAULT_RUNTIME_FALLBACK_PREFIX><uid>/syauth/auth.sock`.
pub const DEFAULT_RUNTIME_FALLBACK_PREFIX: &str = "/run/user/";

/// Subdirectory under the runtime dir that holds the daemon's socket.
/// Matches `syauth_presenced::RUNTIME_SUBDIR`.
pub const RUNTIME_SUBDIR: &str = "syauth";

/// Basename of the daemon's Unix socket. Matches
/// `syauth_presenced::DEFAULT_SOCKET_BASENAME`.
pub const DEFAULT_SOCKET_BASENAME: &str = "auth.sock";

/// Prefix the PAM module recognises in its `argv` for the
/// `socket=<path>` argument (SPEC §3 scope item #12). Anchored as a
/// constant so the parser and the unit test grep the same literal.
pub const PAM_SOCKET_ARG_PREFIX: &str = "socket=";

/// Runtime configuration consumed by [`crate::auth::authenticate`].
///
/// Construct via [`Config::from_pam_argv`] in production code,
/// [`Config::for_tests`] in tests, or the builder pattern
/// (`Config::default().with_bond_dir(...)`).
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory holding `bonds.toml` and `last.log`.
    pub bond_dir: PathBuf,
    /// Path to the daemon's Unix-domain socket. Defaults to
    /// `${XDG_RUNTIME_DIR}/syauth/auth.sock`; overridden by the libpam
    /// `socket=<path>` argument (SPEC §3 scope item #12).
    pub socket_path: PathBuf,
    /// Daemon round-trip budget. Caps the time PAM spends waiting for
    /// the daemon to write back `Response::Challenge`. SPEC §4.3.
    pub auth_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bond_dir: PathBuf::from(DEFAULT_BOND_DIR),
            socket_path: Self::resolve_socket_path(None),
            auth_timeout: DEFAULT_AUTH_TIMEOUT,
        }
    }
}

impl Config {
    /// Parse the libpam `argv` for the documented arguments (currently
    /// only `socket=<path>`) and assemble a `Config`. Unknown
    /// arguments are silently ignored — libpam stacks frequently
    /// carry arguments destined for other modules and rejecting them
    /// would break composition.
    #[must_use]
    pub fn from_pam_argv(argv: &[&str]) -> Self {
        let socket_override = argv
            .iter()
            .find_map(|arg| arg.strip_prefix(PAM_SOCKET_ARG_PREFIX))
            .map(PathBuf::from);
        Self {
            bond_dir: PathBuf::from(DEFAULT_BOND_DIR),
            socket_path: Self::resolve_socket_path(socket_override),
            auth_timeout: DEFAULT_AUTH_TIMEOUT,
        }
    }

    /// Resolve the daemon's Unix-socket path. Precedence:
    ///
    /// 1. `override_path` (the libpam `socket=<path>` argument).
    /// 2. `${XDG_RUNTIME_DIR}/syauth/auth.sock`.
    /// 3. `/run/user/<euid>/syauth/auth.sock` (SPEC §8 Risks fallback).
    #[must_use]
    pub fn resolve_socket_path(override_path: Option<PathBuf>) -> PathBuf {
        if let Some(p) = override_path {
            return p;
        }
        if let Ok(dir) = std::env::var(XDG_RUNTIME_DIR_ENV)
            && !dir.is_empty()
        {
            return PathBuf::from(dir).join(RUNTIME_SUBDIR).join(DEFAULT_SOCKET_BASENAME);
        }
        // `geteuid(2)` cannot fail and `nix::unistd::geteuid` is a
        // safe wrapper. Avoiding `libc::geteuid` keeps the workspace
        // lint `unsafe_code = "deny"` happy (the PAM crate opts in at
        // the crate level but we still prefer typed wrappers).
        let uid = nix::unistd::geteuid().as_raw();
        PathBuf::from(format!("{DEFAULT_RUNTIME_FALLBACK_PREFIX}{uid}"))
            .join(RUNTIME_SUBDIR)
            .join(DEFAULT_SOCKET_BASENAME)
    }

    /// Build a `Config` suitable for tests: tempdir bond dir, default
    /// timeout, and a tempdir-local socket path the caller fills in.
    #[must_use]
    pub fn for_tests(bond_dir: &Path) -> Self {
        Self {
            bond_dir: bond_dir.to_path_buf(),
            socket_path: bond_dir.join(DEFAULT_SOCKET_BASENAME),
            auth_timeout: DEFAULT_AUTH_TIMEOUT,
        }
    }

    /// Builder: override the bond dir.
    #[must_use]
    pub fn with_bond_dir(mut self, bond_dir: PathBuf) -> Self {
        self.bond_dir = bond_dir;
        self
    }

    /// Builder: override the socket path.
    #[must_use]
    pub fn with_socket_path(mut self, socket_path: PathBuf) -> Self {
        self.socket_path = socket_path;
        self
    }

    /// Builder: override the auth timeout. Tests use this for the
    /// daemon-down-latency assertion (TC-01) and the panic-boundary
    /// scenario (legacy TC-09).
    #[must_use]
    pub fn with_auth_timeout(mut self, timeout: Duration) -> Self {
        self.auth_timeout = timeout;
        self
    }

    /// Absolute path to the `last.log` audit file. Always
    /// `<bond_dir>/<LAST_LOG_FILENAME>`.
    #[must_use]
    pub fn last_log_path(&self) -> PathBuf {
        self.bond_dir.join(LAST_LOG_FILENAME)
    }

    /// Absolute path to the bonds TOML file.
    #[must_use]
    pub fn bonds_file_path(&self) -> PathBuf {
        self.bond_dir.join("bonds.toml")
    }
}

#[cfg(test)]
mod tests {
    // Journey: specs/journeys/JOURNEY-S-008-pam-unix-socket-client.md

    use super::*;

    /// TC-12: the PAM module's argv parser picks up `socket=<path>`.
    #[test]
    fn pam_sm_authenticate_parses_socket_argument() {
        let argv = ["socket=/tmp/x.sock"];
        let cfg = Config::from_pam_argv(&argv);
        assert_eq!(cfg.socket_path, PathBuf::from("/tmp/x.sock"));
    }

    /// Unknown arguments are ignored, leaving the default socket path.
    #[test]
    fn from_pam_argv_ignores_unknown_arguments() {
        let argv = ["nullok", "use_first_pass"];
        let cfg = Config::from_pam_argv(&argv);
        // The default resolves either to XDG_RUNTIME_DIR or the
        // /run/user/<uid> fallback; both end in `syauth/auth.sock`.
        let resolved = cfg.socket_path.to_string_lossy();
        assert!(
            resolved.ends_with("syauth/auth.sock"),
            "default socket path should end in syauth/auth.sock; got {resolved}"
        );
    }

    /// Sanity: default timeout matches the SPEC §4.3 budget.
    #[test]
    fn default_auth_timeout_matches_spec_offline_budget() {
        assert_eq!(DEFAULT_AUTH_TIMEOUT, Duration::from_millis(8_000));
    }

    /// Sanity: default bond dir matches SPEC §4.4.
    #[test]
    fn default_bond_dir_matches_spec() {
        assert_eq!(DEFAULT_BOND_DIR, "/var/lib/syauth");
    }

    /// `for_tests` produces a usable config bound to a tempdir.
    #[test]
    fn for_tests_uses_supplied_bond_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = Config::for_tests(tmp.path());
        assert_eq!(cfg.bond_dir, tmp.path());
        assert_eq!(cfg.socket_path, tmp.path().join(DEFAULT_SOCKET_BASENAME));
        assert_eq!(cfg.last_log_path(), tmp.path().join(LAST_LOG_FILENAME));
    }

    /// `with_socket_path` plumbs the override through to the field.
    #[test]
    fn with_socket_path_sets_field() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = Config::for_tests(tmp.path()).with_socket_path(PathBuf::from("/tmp/other.sock"));
        assert_eq!(cfg.socket_path, PathBuf::from("/tmp/other.sock"));
    }
}
