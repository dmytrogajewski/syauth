//! `syauth install-presenced` — install the `syauth-presenced` daemon binary
//! and its systemd user unit.
//!
//! Contract (S-009):
//! * Copies the daemon binary to `/usr/local/libexec/syauth-presenced` (or
//!   `--unit-dir`-rooted dry-run destination), writes the systemd user unit
//!   to `$XDG_CONFIG_HOME/systemd/user/syauth-presenced.service` (or
//!   `--unit-dir`), and in live mode runs `systemctl --user daemon-reload`
//!   followed by `systemctl --user enable --now syauth-presenced.service`.
//! * `--dry-run` skips the `systemctl` calls and prints `would-run:` lines.
//! * The unit-file body is `include_str!`'d at compile time from
//!   `crates/syauth-presenced/dist/syauth-presenced.service` so the bytes the
//!   installer writes match the bytes the workspace ships.
//!
//! Journey: specs/journeys/JOURNEY-S-009-install-presenced-retire-burst.md

use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use clap::Parser;
use thiserror::Error;

/// Default installed location of the daemon binary. SPEC §3 Scope item #9
/// anchors this path; the systemd unit's `ExecStart=` references the same
/// constant via the bundled `dist/syauth-presenced.service`.
pub const DEFAULT_DAEMON_BIN_PATH: &str = "/usr/local/libexec/syauth-presenced";

/// Filename of the systemd user unit. The installer writes it under the
/// resolved user-unit directory.
pub const SYSTEMD_USER_UNIT_NAME: &str = "syauth-presenced.service";

/// Bundled unit-file body. Compiled into the binary so the bytes we ship
/// match the bytes we install — no drift between `dist/` and the live
/// installation.
pub const SYSTEMD_USER_UNIT_BUNDLED: &str = include_str!("../../syauth-presenced/dist/syauth-presenced.service");

/// Filename of the daemon binary we look for next to the running `syauth`
/// binary when `--from` is not supplied.
pub const DAEMON_BIN_NAME: &str = "syauth-presenced";

/// Subdirectory under `XDG_CONFIG_HOME` (or `~/.config`) where systemd user
/// units live.
pub const SYSTEMD_USER_UNIT_SUBDIR: &str = "systemd/user";

/// Env var that, per the XDG base-dir spec, names the user-config root.
pub const XDG_CONFIG_HOME_ENV: &str = "XDG_CONFIG_HOME";

/// Fallback subdirectory under `$HOME` when `XDG_CONFIG_HOME` is unset.
pub const XDG_CONFIG_HOME_FALLBACK_SUBDIR: &str = ".config";

/// Stdout banner prefix for the dry-run `systemctl` lines. Tests assert on
/// this verbatim.
pub const WOULD_RUN_PREFIX: &str = "would-run: ";

/// CLI options for the `install-presenced` subcommand.
#[derive(Debug, Parser, Clone)]
pub struct InstallPresencedOpts {
    /// Path to the `syauth-presenced` source binary to install. When omitted,
    /// the installer probes for a sibling next to the running `syauth`
    /// binary (`current_exe().parent().join("syauth-presenced")`).
    #[arg(long)]
    pub from: Option<PathBuf>,

    /// Override the destination directory for the systemd user unit. When
    /// omitted, resolves to `$XDG_CONFIG_HOME/systemd/user` (falling back to
    /// `$HOME/.config/systemd/user`). Tests pass a tempdir here.
    #[arg(long)]
    pub unit_dir: Option<PathBuf>,

    /// Print what the installer would do without copying the binary or
    /// running `systemctl`. The unit file is still written to `--unit-dir`
    /// so tests can read it back.
    #[arg(long)]
    pub dry_run: bool,
}

/// Outcome of an `install_presenced` call. The CLI dispatch prints a one-
/// liner per variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallPresencedOutcome {
    /// Live install: binary copied, unit written, systemctl invocations
    /// fired.
    Installed {
        /// Path of the unit file after the write.
        unit_path: PathBuf,
        /// Destination of the daemon binary after the copy.
        binary_path: PathBuf,
    },
    /// Dry-run: unit file written to `--unit-dir`, `systemctl` invocations
    /// printed but not executed.
    DryRun {
        /// Path of the unit file after the write.
        unit_path: PathBuf,
        /// Source binary path (the one `ExecStart=` references).
        source_binary: PathBuf,
    },
}

/// Typed error surface for `install_presenced`.
#[derive(Debug, Error)]
pub enum InstallPresencedError {
    /// `--from` was omitted and the auto-detect found no sibling binary.
    #[error("could not locate syauth-presenced binary; pass --from <path>")]
    SourceMissing,

    /// `current_exe()` failed; cannot probe for the sibling binary.
    #[error("failed to resolve current executable path: {0}")]
    CurrentExe(#[source] std::io::Error),

    /// `--unit-dir` was omitted and neither `XDG_CONFIG_HOME` nor `HOME`
    /// were set.
    #[error("cannot resolve user-unit directory: neither XDG_CONFIG_HOME nor HOME is set")]
    UnitDirUnresolvable,

    /// Generic I/O failure (read, write, copy, mkdir).
    #[error("I/O error on {path}: {source}")]
    Io {
        /// Path the I/O was attempted on.
        path: PathBuf,
        /// Originating I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A `systemctl` invocation returned a non-zero exit code.
    #[error("`systemctl --user {verb}` failed (exit {code:?}): {stderr}")]
    SystemctlFailed {
        /// Which systemctl verb was being invoked.
        verb: String,
        /// Exit code, if the child terminated normally.
        code: Option<i32>,
        /// Captured stderr of the failed call.
        stderr: String,
    },

    /// `systemctl` could not be spawned at all (e.g. not in PATH).
    #[error("failed to spawn `systemctl`: {0}")]
    SystemctlSpawn(#[source] std::io::Error),
}

/// Resolve the destination directory of the systemd user unit. Honors
/// `--unit-dir` first, then `$XDG_CONFIG_HOME/systemd/user`, then
/// `$HOME/.config/systemd/user`.
fn resolve_unit_dir(unit_dir_override: Option<&Path>) -> Result<PathBuf, InstallPresencedError> {
    if let Some(p) = unit_dir_override {
        return Ok(p.to_path_buf());
    }
    if let Ok(xdg) = env::var(XDG_CONFIG_HOME_ENV)
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg).join(SYSTEMD_USER_UNIT_SUBDIR));
    }
    if let Ok(home) = env::var("HOME")
        && !home.is_empty()
    {
        return Ok(PathBuf::from(home)
            .join(XDG_CONFIG_HOME_FALLBACK_SUBDIR)
            .join(SYSTEMD_USER_UNIT_SUBDIR));
    }
    Err(InstallPresencedError::UnitDirUnresolvable)
}

/// Resolve the source binary path. Honors `--from` first, then probes the
/// sibling of `current_exe()`.
fn resolve_source_binary(from_override: Option<&Path>) -> Result<PathBuf, InstallPresencedError> {
    if let Some(p) = from_override {
        return Ok(p.to_path_buf());
    }
    let me = env::current_exe().map_err(InstallPresencedError::CurrentExe)?;
    let sibling = me.parent().map(|d| d.join(DAEMON_BIN_NAME));
    match sibling {
        Some(s) if s.exists() => Ok(s),
        _ => Err(InstallPresencedError::SourceMissing),
    }
}

/// Write `body` to `target` atomically, creating parent dirs as needed.
fn atomic_write_text(target: &Path, body: &str) -> Result<(), InstallPresencedError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|source| InstallPresencedError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let parent = target.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|source| InstallPresencedError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    tmp.write_all(body.as_bytes()).map_err(|source| InstallPresencedError::Io {
        path: tmp.path().to_path_buf(),
        source,
    })?;
    tmp.as_file().sync_all().map_err(|source| InstallPresencedError::Io {
        path: tmp.path().to_path_buf(),
        source,
    })?;
    tmp.persist(target).map_err(|err| InstallPresencedError::Io {
        path: target.to_path_buf(),
        source: err.error,
    })?;
    Ok(())
}

/// Substitute the `ExecStart=` directive in the bundled unit body so it
/// points at the actual `--from` path. The bundled template references
/// `/usr/local/libexec/syauth-presenced` (the production install path); when
/// the operator passes `--from <elsewhere>`, the live and dry-run unit text
/// must name that elsewhere.
fn rewrite_exec_start(body: &str, binary_path: &Path) -> String {
    let mut out = String::with_capacity(body.len());
    let mut replaced = false;
    for line in body.lines() {
        if !replaced && line.starts_with("ExecStart=") {
            out.push_str("ExecStart=");
            out.push_str(&binary_path.display().to_string());
            out.push('\n');
            replaced = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Execute `systemctl --user <verb>...` and surface a typed error on
/// failure. Skipped in dry-run; this function is only called by the live
/// path.
fn run_systemctl(args: &[&str]) -> Result<(), InstallPresencedError> {
    let mut cmd = Command::new("systemctl");
    cmd.arg("--user");
    for a in args {
        cmd.arg(a);
    }
    let output = cmd.output().map_err(InstallPresencedError::SystemctlSpawn)?;
    if !output.status.success() {
        return Err(InstallPresencedError::SystemctlFailed {
            verb: args.join(" "),
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}

/// Drives the install workflow per the S-009 DoD.
///
/// # Errors
///
/// Returns [`InstallPresencedError`] when the source binary cannot be
/// located, the unit-file destination cannot be resolved, an I/O call
/// fails, or `systemctl` exits non-zero.
pub fn install_presenced<W: Write>(opts: &InstallPresencedOpts, stdout: &mut W) -> Result<InstallPresencedOutcome, InstallPresencedError> {
    let unit_dir = resolve_unit_dir(opts.unit_dir.as_deref())?;
    let unit_path = unit_dir.join(SYSTEMD_USER_UNIT_NAME);
    let source_binary = resolve_source_binary(opts.from.as_deref())?;
    // In live mode the unit's `ExecStart=` references the canonical
    // /usr/local/libexec path (where we are about to copy the binary). In
    // dry-run / `--from` mode it must reference the actual source path so
    // operators inspecting the unit can correlate it with what they passed.
    let binary_path: PathBuf = if opts.dry_run || opts.from.is_some() {
        source_binary.clone()
    } else {
        PathBuf::from(DEFAULT_DAEMON_BIN_PATH)
    };
    let unit_body = rewrite_exec_start(SYSTEMD_USER_UNIT_BUNDLED, &binary_path);
    atomic_write_text(&unit_path, &unit_body)?;

    if opts.dry_run {
        writeln!(stdout, "{WOULD_RUN_PREFIX}systemctl --user daemon-reload").map_err(|source| InstallPresencedError::Io {
            path: PathBuf::from("<stdout>"),
            source,
        })?;
        writeln!(stdout, "{WOULD_RUN_PREFIX}systemctl --user enable --now {SYSTEMD_USER_UNIT_NAME}").map_err(|source| {
            InstallPresencedError::Io {
                path: PathBuf::from("<stdout>"),
                source,
            }
        })?;
        return Ok(InstallPresencedOutcome::DryRun { unit_path, source_binary });
    }

    // Live path: copy the binary into place, then reload + enable.
    if let Some(parent) = Path::new(DEFAULT_DAEMON_BIN_PATH).parent() {
        fs::create_dir_all(parent).map_err(|source| InstallPresencedError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::copy(&source_binary, DEFAULT_DAEMON_BIN_PATH).map_err(|source| InstallPresencedError::Io {
        path: PathBuf::from(DEFAULT_DAEMON_BIN_PATH),
        source,
    })?;
    run_systemctl(&["daemon-reload"])?;
    run_systemctl(&["enable", "--now", SYSTEMD_USER_UNIT_NAME])?;
    Ok(InstallPresencedOutcome::Installed {
        unit_path,
        binary_path: PathBuf::from(DEFAULT_DAEMON_BIN_PATH),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_exec_start_substitutes_first_match() {
        let body = "[Service]\nExecStart=/usr/local/libexec/syauth-presenced\nRestart=on-failure\n";
        let got = rewrite_exec_start(body, Path::new("/tmp/fake"));
        assert!(got.contains("ExecStart=/tmp/fake\n"), "rewrite missing: {got}");
        assert!(got.contains("Restart=on-failure"), "tail preserved: {got}");
    }

    #[test]
    fn rewrite_exec_start_leaves_body_alone_when_no_match() {
        let body = "[Service]\nRestart=on-failure\n";
        let got = rewrite_exec_start(body, Path::new("/tmp/fake"));
        assert!(!got.contains("ExecStart="), "no exec_start to rewrite: {got}");
    }

    #[test]
    fn resolve_unit_dir_honors_override() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let got = resolve_unit_dir(Some(dir.path())).expect("resolve");
        assert_eq!(got, dir.path());
    }

    #[test]
    fn dry_run_writes_unit_and_prints_would_run_lines() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let fake = dir.path().join("fake-daemon-binary");
        fs::write(&fake, b"").expect("touch fake");
        let opts = InstallPresencedOpts {
            from: Some(fake.clone()),
            unit_dir: Some(dir.path().to_path_buf()),
            dry_run: true,
        };
        let mut buf: Vec<u8> = Vec::new();
        let outcome = install_presenced(&opts, &mut buf).expect("dry-run ok");
        match outcome {
            InstallPresencedOutcome::DryRun { unit_path, source_binary } => {
                assert_eq!(source_binary, fake);
                let body = fs::read_to_string(&unit_path).expect("read unit");
                let expected_exec_start = format!("ExecStart={}", fake.display());
                assert!(body.contains(&expected_exec_start), "ExecStart pin: {body}");
            }
            other => panic!("expected DryRun, got {other:?}"),
        }
        let stdout = String::from_utf8(buf).expect("utf8 stdout");
        assert!(
            stdout.contains("would-run: systemctl --user daemon-reload"),
            "daemon-reload would-run line: {stdout:?}"
        );
        assert!(
            stdout.contains("would-run: systemctl --user enable --now syauth-presenced.service"),
            "enable would-run line: {stdout:?}"
        );
    }
}
