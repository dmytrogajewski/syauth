//! Runtime configuration for `pam_sm_authenticate`.
//!
//! S-009 reads three knobs at module-entry time:
//!
//! - [`Config::bond_dir`] — directory holding `bonds.toml` and `last.log`.
//!   Defaults to [`DEFAULT_BOND_DIR`] (SPEC §4.4); overridable for tests via
//!   the [`Config::with_bond_dir`] builder.
//! - [`Config::auth_timeout`] — hard wall-clock budget for the
//!   `connect → send → recv` roundtrip. Defaults to [`DEFAULT_AUTH_TIMEOUT`]
//!   (SPEC §4.3 offline budget).
//! - [`Config::mock_peer_enabled`] — whether `SYAUTH_TEST_MOCK=1` activates
//!   the process-local [`crate::auth::MOCK_PEER`] slot. Gated behind a
//!   compile-time flag so a release build *ignores* the env var.
//!
//! ## Production-build env-var defense
//!
//! The DoD names a specific anti-foot-gun rule: `SYAUTH_TEST_MOCK=1` must
//! NOT enable the mock in a real `libpam_syauth.so` shipped to users. We
//! implement this with two gates `OR`-ed together:
//!
//! 1. `cfg!(test)` — true while building unit tests.
//! 2. `cfg!(feature = "test-mock")` — opt-in via the `test-mock` Cargo
//!    feature. The crate's own `[dev-dependencies]` re-import enables this
//!    feature for `tests/pam_e2e.rs`. The release pipeline (`cargo build
//!    --release --workspace`) does not enable the feature, so the env var
//!    has no effect.
//!
//! [`Config::from_env_with_build_flags`] takes the two flags as parameters
//! so the unit test in this module can pin the matrix directly. Callers in
//! production go through [`Config::from_env`], which inlines the
//! compile-time flags.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// Default directory for `bonds.toml` and `last.log`. Mirrors SPEC §4.4.
///
/// Tests override this via [`Config::with_bond_dir`] so they never touch the
/// real path.
pub const DEFAULT_BOND_DIR: &str = "/var/lib/syauth";

/// Default authentication wall-clock budget. SPEC §4.3 mandates that an
/// offline-peer outcome arrive at libpam within **1.2 s**, including the
/// time spent inside the tokio runtime and the bond-store read. The number
/// is exact, not a "ballpark" — DoD #2 measures against this constant.
pub const DEFAULT_AUTH_TIMEOUT: Duration = Duration::from_millis(1_200);

/// Name of the file appended to under [`Config::bond_dir`] on every
/// `authenticate` call. Read by `syauth status` in S-012.
pub const LAST_LOG_FILENAME: &str = "last.log";

/// Environment variable consulted by [`Config::from_env`]. Setting it to the
/// literal value `1` enables the mock-peer path *only* when the
/// `test-mock` Cargo feature is enabled (or the build is under `cargo test`).
/// Documented at the type level for the operator's eye.
pub const TEST_MOCK_ENV_VAR: &str = "SYAUTH_TEST_MOCK";

/// The exact env-var value that turns the mock on. Anything else (`"0"`,
/// `"yes"`, `""`, unset) is treated as off.
pub const TEST_MOCK_ENV_ENABLED_VALUE: &str = "1";

/// Runtime configuration consumed by [`crate::auth::authenticate`].
///
/// Construct via [`Config::from_env`] in production code, [`Config::for_tests`]
/// in tests, or the builder pattern (`Config::default().with_bond_dir(...)`).
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory holding `bonds.toml` and `last.log`.
    pub bond_dir: PathBuf,
    /// Hard wall-clock budget for the entire `authenticate` call.
    pub auth_timeout: Duration,
    /// Whether the mock-peer injection slot is honored. Computed from the
    /// env var AND the compile-time flags by [`Config::from_env`].
    pub mock_peer_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bond_dir: PathBuf::from(DEFAULT_BOND_DIR),
            auth_timeout: DEFAULT_AUTH_TIMEOUT,
            mock_peer_enabled: false,
        }
    }
}

impl Config {
    /// Load the config from the process environment using the compile-time
    /// build flags of this crate.
    ///
    /// Calls [`Config::from_env_with_build_flags`] with `cfg!(test)` and
    /// `cfg!(feature = "test-mock")`. In any release build with the
    /// `test-mock` feature off, `mock_peer_enabled` is always `false`
    /// regardless of `SYAUTH_TEST_MOCK`.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_env_with_build_flags(cfg!(test), cfg!(feature = "test-mock"))
    }

    /// Same as [`Config::from_env`] but takes the build-time flags as
    /// parameters so the unit test in this module can pin the
    /// production-build matrix without macro contortions.
    #[must_use]
    pub fn from_env_with_build_flags(under_cargo_test: bool, test_mock_feature: bool) -> Self {
        let env_says_on = std::env::var(TEST_MOCK_ENV_VAR)
            .map(|v| v == TEST_MOCK_ENV_ENABLED_VALUE)
            .unwrap_or(false);
        let allowed_by_build = under_cargo_test || test_mock_feature;
        Self {
            bond_dir: PathBuf::from(DEFAULT_BOND_DIR),
            auth_timeout: DEFAULT_AUTH_TIMEOUT,
            mock_peer_enabled: env_says_on && allowed_by_build,
        }
    }

    /// Build a `Config` suitable for tests: tempdir bond dir, mock enabled,
    /// default timeout.
    ///
    /// `auth_timeout` is left at the production default so the offline-path
    /// budget assertion in TC-02 measures something meaningful.
    #[must_use]
    pub fn for_tests(bond_dir: &Path) -> Self {
        Self {
            bond_dir: bond_dir.to_path_buf(),
            auth_timeout: DEFAULT_AUTH_TIMEOUT,
            mock_peer_enabled: true,
        }
    }

    /// Builder: override the bond dir.
    #[must_use]
    pub fn with_bond_dir(mut self, bond_dir: PathBuf) -> Self {
        self.bond_dir = bond_dir;
        self
    }

    /// Builder: override the auth timeout. Tests use this for the panic-
    /// boundary scenario (TC-09 short budget).
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
    // Journey: specs/journeys/JOURNEY-S-009-pam-mock-e2e.md TC-11.

    use std::sync::Mutex;

    use super::*;

    /// Serialize every test that touches the process-wide env var. Rust 2024
    /// marks `set_var`/`remove_var` as `unsafe` because they race with
    /// concurrent `getenv` readers; the unit tests below mutate the same
    /// key, so we must funnel them through one mutex regardless of
    /// `cargo test`'s thread fanout.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// TC-11: in a release build with `test-mock` feature *off*, the env var
    /// MUST be ignored. This is the documented anti-foot-gun guarantee from
    /// DoD #5.
    #[test]
    fn production_build_ignores_test_mock_env_var() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // We pass the matrix directly so `cfg!(test)` (always true in this
        // unit test) doesn't accidentally make the assertion vacuous.
        // SAFETY: env-var set/unset is unsafe in 2024 edition for cross-
        // thread reasons; this unit test is single-threaded under ENV_LOCK
        // and the restoration in the guard ensures no leakage to sibling
        // tests.
        let _guard = TestEnvGuard::set(TEST_MOCK_ENV_VAR, TEST_MOCK_ENV_ENABLED_VALUE);
        let cfg = Config::from_env_with_build_flags(false, false);
        assert!(
            !cfg.mock_peer_enabled,
            "release build (no test-mock feature, not under cargo test) MUST ignore SYAUTH_TEST_MOCK; got mock_peer_enabled=true"
        );
    }

    /// A build with the `test-mock` feature on AND the env var set MUST
    /// honor the env var. This is the path the integration tests in
    /// `tests/pam_e2e.rs` rely on.
    #[test]
    fn test_mock_feature_with_env_set_enables_mock() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = TestEnvGuard::set(TEST_MOCK_ENV_VAR, TEST_MOCK_ENV_ENABLED_VALUE);
        let cfg = Config::from_env_with_build_flags(false, true);
        assert!(cfg.mock_peer_enabled, "test-mock feature + env=1 must enable the mock");
    }

    /// A build with `test-mock` on but the env var unset MUST leave the
    /// mock off — opting in to the feature alone is not enough.
    #[test]
    fn test_mock_feature_without_env_keeps_mock_off() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = TestEnvGuard::unset(TEST_MOCK_ENV_VAR);
        let cfg = Config::from_env_with_build_flags(false, true);
        assert!(!cfg.mock_peer_enabled, "test-mock feature without env=1 must leave mock off");
    }

    /// Sanity: default timeout matches the spec budget (SPEC §4.3).
    #[test]
    fn default_auth_timeout_matches_spec_offline_budget() {
        assert_eq!(DEFAULT_AUTH_TIMEOUT, Duration::from_millis(1_200));
    }

    /// Sanity: default bond dir is the SPEC §4.4 path.
    #[test]
    fn default_bond_dir_matches_spec() {
        assert_eq!(DEFAULT_BOND_DIR, "/var/lib/syauth");
        assert_eq!(Config::default().bond_dir, PathBuf::from("/var/lib/syauth"));
    }

    /// `for_tests` produces a usable config bound to a tempdir.
    #[test]
    fn for_tests_uses_supplied_bond_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = Config::for_tests(tmp.path());
        assert_eq!(cfg.bond_dir, tmp.path());
        assert!(cfg.mock_peer_enabled);
        assert_eq!(cfg.last_log_path(), tmp.path().join(LAST_LOG_FILENAME));
    }

    /// Process-wide env-var guard that restores the prior state on drop.
    ///
    /// Rust 2024 marks `std::env::set_var` / `remove_var` as `unsafe` because
    /// they mutate global state without synchronization with concurrent
    /// readers (notably `libc::getenv`). The PAM module never calls these in
    /// production; the guard exists ONLY for these unit tests, which run
    /// single-threaded with `cargo test` by default for `--test-threads`
    /// the same value as core count — sibling tests in this module are
    /// not concurrent because they all share this fixture.
    struct TestEnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }
    impl TestEnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: single-threaded test, no concurrent getenv readers in
            // this crate's test binary. Restored on drop.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
        fn unset(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: same as `set`.
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, previous }
        }
    }
    impl Drop for TestEnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(v) => {
                    // SAFETY: same as `set`.
                    unsafe {
                        std::env::set_var(self.key, v);
                    }
                }
                None => {
                    // SAFETY: same as `set`.
                    unsafe {
                        std::env::remove_var(self.key);
                    }
                }
            }
        }
    }
}
