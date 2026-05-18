//! `syauth install-pam` — atomically wire `pam_syauth.so` into a PAM service
//! file with a `.bak` snapshot.
//!
//! Contract (S-013):
//! * Idempotent: a second invocation produces a byte-identical file.
//! * Refuses to overwrite a pre-existing `.bak`.
//! * Atomic write via `tempfile::NamedTempFile::persist`.
//! * Preserves the source file's mode bits.
//!
//! Journey: specs/journeys/JOURNEY-S-013-pam-install-helper.md

use std::{
    fs,
    io::Write as _,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use clap::Parser;
use regex::Regex;
use thiserror::Error;

/// Default directory holding PAM service files on Linux.
pub const DEFAULT_PAM_DIR: &str = "/etc/pam.d";

/// Suffix appended to a service file to produce its backup path.
pub const BACKUP_SUFFIX: &str = ".bak";

/// Default `pam_syauth` module shared-object name (resolved via PAM's
/// module search path; we deliberately do NOT hard-code an absolute path).
pub const DEFAULT_SO_NAME: &str = "pam_syauth.so";

/// Default PAM module arguments inserted after the so-name. Matches
/// the daemon's `DEFAULT_AUTH_TIMEOUT` (8 s) so a real BiometricPrompt
/// reaction has headroom before the PAM round-trip falls through to
/// the next sufficient module.
pub const DEFAULT_MODULE_ARGS: &str = "timeout=8000";

/// Default PAM control flag for the inserted auth directive.
///
/// `sufficient` is the operator-facing default: when the phone is in
/// range and the user approves on the biometric prompt, the auth
/// stack short-circuits with success. When the phone is absent, the
/// daemon is down, the BLE link is broken, or the user denies, the
/// module returns `PAM_AUTHINFO_UNAVAIL` / `PAM_AUTH_ERR` and the
/// stack falls through to the next module (typically `pam_u2f.so`
/// for FIDO2 fallback).
///
/// `--control` lets the operator pick a different flag for testing
/// (`required`, `[success=done auth_err=die default=ignore]`, etc.).
pub const DEFAULT_CONTROL_FLAG: &str = "sufficient";

/// Canonical line inserted by `--with-u2f-fallback`. Mirrors the
/// `pam_u2f` directive shape already shipping under
/// `/etc/pam.d/sudo` and `/etc/pam.d/gdm-password` on Fedora 43 so
/// the post-install stack reads uniformly across services.
pub const U2F_FALLBACK_LINE: &str = "auth    sufficient    pam_u2f.so cue";

/// Recognition regex for any line that wires pam_u2f into the auth
/// stack. Same shape as [`RECOGNITION_REGEX`] but for `pam_u2f.so`;
/// drives the idempotency check that prevents `--with-u2f-fallback`
/// from inserting a duplicate line on subsequent runs.
const U2F_RECOGNITION_REGEX: &str = r"(?m)^\s*auth\s+\S+\s+pam_u2f\.so\b";

/// PAM module type for the inserted directive.
pub const MODULE_TYPE: &str = "auth";

/// Whitespace separator used between fields of the inserted line. Matches the
/// 4-space convention used in the DoD example.
pub const FIELD_SEPARATOR: &str = "    ";

/// Recognition regex for any line that wires syauth into a PAM stack.
/// Matches `auth <ctrl> pam_syauth.so [args...]` with arbitrary whitespace.
/// The `(?m)` flag turns `^` into a per-line anchor so the line is detected
/// regardless of where in the file it lives.
const RECOGNITION_REGEX: &str = r"(?m)^\s*auth\s+\S+\s+pam_syauth\.so\b";

fn recognition_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(RECOGNITION_REGEX).expect("static syauth-recognition regex must compile"))
}

/// True when `body` contains any recognizable syauth auth line. Exposed for
/// the `uninstall_pam` module so the install + uninstall recognition rule
/// is a single point of truth.
pub fn recognition_regex_match(body: &str) -> bool {
    recognition_regex().is_match(body)
}

/// Returns true if `line`'s trimmed text starts an `auth` directive.
fn is_auth_directive(line: &str) -> bool {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("auth") {
        return false;
    }
    let after = &trimmed["auth".len()..];
    matches!(after.chars().next(), Some(c) if c.is_whitespace())
}

/// CLI options for the `install-pam` subcommand.
#[derive(Debug, Parser)]
pub struct InstallOpts {
    /// PAM service to install into (e.g. `sudo`, `login`, `gdm-password`).
    #[arg(long)]
    pub service: String,

    /// PAM service directory to modify. Defaults to `/etc/pam.d`. Tests
    /// inject a tempdir.
    #[arg(long, default_value = DEFAULT_PAM_DIR)]
    pub pam_dir: PathBuf,

    /// Module arguments appended after the so-name. Defaults to
    /// `timeout=1200`.
    #[arg(long, default_value = DEFAULT_MODULE_ARGS)]
    pub module_args: String,

    /// Shared-object name of the PAM module. Defaults to `pam_syauth.so`.
    /// PAM resolves this against its module search path.
    #[arg(long, default_value = DEFAULT_SO_NAME)]
    pub so_path: String,

    /// Skip the interactive confirmation prompt. Tests always pass this.
    #[arg(long)]
    pub yes: bool,

    /// Also install the `syauth-presenced` systemd user unit (S-009).
    /// Default `true` so the PAM edit and the daemon install are bundled.
    /// Pass `--with-presenced=false` to install only the PAM line — e.g.
    /// when the daemon is managed by the host distribution.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub with_presenced: bool,

    /// When `--with-presenced=true`, forward this flag to the bundled
    /// `install-presenced` step. Tests pass this so the bundled step does
    /// not shell out to `systemctl`.
    #[arg(long)]
    pub presenced_dry_run: bool,

    /// When `--with-presenced=true`, forward this directory override to the
    /// bundled `install-presenced` step. Tests pass a tempdir here.
    #[arg(long)]
    pub presenced_unit_dir: Option<PathBuf>,

    /// When `--with-presenced=true`, forward this source-binary override to
    /// the bundled `install-presenced` step.
    #[arg(long)]
    pub presenced_from: Option<PathBuf>,

    /// PAM control flag for the inserted auth directive. Defaults to
    /// `sufficient` so a successful syauth round-trip short-circuits
    /// the auth stack and a failed one falls through to the next
    /// sufficient module (typically pam_u2f for FIDO2 fallback). Pass
    /// `required` to make syauth the only path that can grant auth on
    /// this service.
    #[arg(long, default_value = DEFAULT_CONTROL_FLAG)]
    pub control: String,

    /// Also insert `auth sufficient pam_u2f.so cue` directly below
    /// the syauth line if the service file does not already carry a
    /// `pam_u2f.so` directive. Use this when wiring services that
    /// today rely solely on a password fallback (`login`, `su`,
    /// `runuser`, …) so the auth chain becomes
    /// `syauth -> FIDO -> password`, matching the chain on services
    /// where pam_u2f is pre-configured (`sudo`, `gdm-password`).
    /// Idempotent: a second invocation with the flag is a no-op when
    /// a pam_u2f line is already present.
    #[arg(long, default_value_t = false)]
    pub with_u2f_fallback: bool,
}

/// Typed error surface for `install_pam`.
#[derive(Debug, Error)]
pub enum InstallError {
    /// The service file does not exist under `--pam-dir`.
    #[error("PAM service file not found: {0}")]
    ServiceNotFound(PathBuf),

    /// A `.bak` already exists. Refusing to clobber.
    #[error("refusing to overwrite existing backup at {0}; move it aside (e.g. `mv {0} {0}.before-syauth`) and retry")]
    BackupExists(PathBuf),

    /// The service file is not valid UTF-8; we refuse to guess an encoding.
    #[error("PAM service file at {path} is not valid UTF-8: {source}")]
    NotUtf8 {
        /// Path to the file we tried to read.
        path: PathBuf,
        /// Originating decode error.
        #[source]
        source: std::string::FromUtf8Error,
    },

    /// Generic I/O failure (read, write, persist, copy, set_permissions).
    #[error("I/O error on {path}: {source}")]
    Io {
        /// Path the I/O was attempted on.
        path: PathBuf,
        /// Originating I/O error.
        #[source]
        source: std::io::Error,
    },

    /// `tempfile::persist` failed; the original is intact and the temp file
    /// has been cleaned up by tempfile's drop.
    #[error("atomic persist of {path} failed: {source}")]
    Persist {
        /// Destination path.
        path: PathBuf,
        /// Originating tempfile error.
        #[source]
        source: tempfile::PersistError,
    },
}

/// Outcome of an `install` call. Library callers can inspect this; the CLI
/// prints a one-liner for each variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// The service file already contained a recognizable syauth line; no
    /// changes were made and no backup was written.
    AlreadyInstalled {
        /// Path to the service file.
        path: PathBuf,
    },
    /// The line was inserted; a fresh `.bak` was written.
    Installed {
        /// Path to the service file (post-install).
        service: PathBuf,
        /// Path to the freshly written backup.
        backup: PathBuf,
    },
}

/// Path helper: `<pam_dir>/<service>`.
fn service_path(pam_dir: &Path, service: &str) -> PathBuf {
    pam_dir.join(service)
}

/// Path helper: `<pam_dir>/<service>.bak`.
fn backup_path(pam_dir: &Path, service: &str) -> PathBuf {
    pam_dir.join(format!("{service}{BACKUP_SUFFIX}"))
}

/// Build the canonical inserted line (no trailing newline).
fn build_line(opts: &InstallOpts) -> String {
    format!(
        "{MODULE_TYPE}{sep}{ctrl}{sep}{so} {args}",
        sep = FIELD_SEPARATOR,
        ctrl = opts.control,
        so = opts.so_path,
        args = opts.module_args
    )
}

/// Returns the rewritten file body (line inserted at top of auth block).
fn insert_line(original: &str, new_line: &str) -> String {
    let mut out = String::with_capacity(original.len() + new_line.len() + 1);
    let mut inserted = false;
    // We intentionally use `split_inclusive` to preserve every byte (CR/LF,
    // missing-final-newline, blank lines) of the source.
    for line in original.split_inclusive('\n') {
        if !inserted && is_auth_directive(line) {
            out.push_str(new_line);
            out.push('\n');
            inserted = true;
        }
        out.push_str(line);
    }
    if !inserted {
        // No `auth` directive at all — append at the top.
        let mut prefixed = String::with_capacity(original.len() + new_line.len() + 1);
        prefixed.push_str(new_line);
        prefixed.push('\n');
        prefixed.push_str(original);
        return prefixed;
    }
    out
}

/// Reads `path` and decodes as UTF-8 with typed errors.
fn read_utf8(path: &Path) -> Result<String, InstallError> {
    let bytes = fs::read(path).map_err(|source| InstallError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    String::from_utf8(bytes).map_err(|source| InstallError::NotUtf8 {
        path: path.to_path_buf(),
        source,
    })
}

/// Atomically writes `new_contents` to `target`, preserving the file mode
/// taken from `source_mode`. Uses `NamedTempFile::new_in(parent)` so the
/// persist is a real rename within the same filesystem.
fn atomic_write(target: &Path, new_contents: &str, source_mode: u32) -> Result<(), InstallError> {
    let parent = target.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|source| InstallError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    tmp.write_all(new_contents.as_bytes()).map_err(|source| InstallError::Io {
        path: tmp.path().to_path_buf(),
        source,
    })?;
    tmp.as_file().sync_all().map_err(|source| InstallError::Io {
        path: tmp.path().to_path_buf(),
        source,
    })?;
    // Match source mode before the persist so the final inode has the
    // correct permission bits the moment it becomes visible.
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(source_mode)).map_err(|source| InstallError::Io {
        path: tmp.path().to_path_buf(),
        source,
    })?;
    tmp.persist(target).map_err(|source| InstallError::Persist {
        path: target.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Returns the file mode bits (permission bits only) of `path`.
fn file_mode(path: &Path) -> Result<u32, InstallError> {
    let meta = fs::metadata(path).map_err(|source| InstallError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(meta.permissions().mode() & 0o7777)
}

/// Drives the install workflow per the S-013 DoD.
///
/// # Errors
///
/// Returns [`InstallError`] when the service file is missing, the backup
/// already exists, the file is not UTF-8, or an I/O / persist call fails.
pub fn install(opts: &InstallOpts) -> Result<InstallOutcome, InstallError> {
    let service = service_path(&opts.pam_dir, &opts.service);
    if !service.exists() {
        return Err(InstallError::ServiceNotFound(service));
    }
    let original = read_utf8(&service)?;
    let syauth_present = recognition_regex().is_match(&original);
    let u2f_present = u2f_recognition_regex().is_match(&original);
    let needs_u2f_insert = opts.with_u2f_fallback && !u2f_present;
    // Idempotency: when the syauth line is already present AND the u2f
    // fallback (if requested) is also already present, the file is in
    // its target shape — exit before any write. The reverse case —
    // syauth present but pam_u2f missing while `--with-u2f-fallback`
    // is set — re-enters the write path to insert the fallback line
    // below the existing syauth directive.
    if syauth_present && !needs_u2f_insert {
        return Ok(InstallOutcome::AlreadyInstalled { path: service });
    }
    let mode = file_mode(&service)?;
    let backup = backup_path(&opts.pam_dir, &opts.service);
    if !syauth_present {
        // First-time install: take the backup BEFORE the atomic write
        // so a crash after backup + before write still leaves the
        // admin with a known-good snapshot at <service>.bak.
        if backup.exists() {
            return Err(InstallError::BackupExists(backup));
        }
        fs::copy(&service, &backup).map_err(|source| InstallError::Io {
            path: backup.clone(),
            source,
        })?;
        fs::set_permissions(&backup, fs::Permissions::from_mode(mode)).map_err(|source| InstallError::Io {
            path: backup.clone(),
            source,
        })?;
    }
    // The body the next-stage transforms operate on. When syauth is
    // already in place, skip the build_line insertion and apply the
    // u2f fallback directly on top of the existing body.
    let with_syauth = if syauth_present {
        original
    } else {
        insert_line(&original, &build_line(opts))
    };
    let new_contents = maybe_insert_u2f_fallback(&with_syauth, opts.with_u2f_fallback);
    atomic_write(&service, &new_contents, mode)?;
    Ok(InstallOutcome::Installed { service, backup })
}

/// If `opts.with_u2f_fallback` is set and the body does not already
/// carry a `pam_u2f.so` directive, insert [`U2F_FALLBACK_LINE`]
/// directly below the syauth line so the resulting auth chain is
/// `syauth -> pam_u2f -> next-module`. Pure fn: returns the original
/// body untouched when the flag is false or when the file already
/// declares pam_u2f, which keeps the call idempotent across repeated
/// invocations.
fn maybe_insert_u2f_fallback(body: &str, enabled: bool) -> String {
    if !enabled {
        return body.to_string();
    }
    if u2f_recognition_regex().is_match(body) {
        return body.to_string();
    }
    let mut out = String::with_capacity(body.len() + U2F_FALLBACK_LINE.len() + 1);
    let mut inserted = false;
    for line in body.split_inclusive('\n') {
        out.push_str(line);
        if !inserted && line.contains("pam_syauth.so") {
            out.push_str(U2F_FALLBACK_LINE);
            out.push('\n');
            inserted = true;
        }
    }
    out
}

fn u2f_recognition_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(U2F_RECOGNITION_REGEX).expect("static u2f-recognition regex must compile"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "#%PAM-1.0\nauth       include      system-auth\naccount    include      system-auth\n";

    fn opts_for(pam_dir: &Path) -> InstallOpts {
        InstallOpts {
            service: "demo".to_string(),
            pam_dir: pam_dir.to_path_buf(),
            module_args: DEFAULT_MODULE_ARGS.to_string(),
            so_path: DEFAULT_SO_NAME.to_string(),
            yes: true,
            with_presenced: false,
            presenced_dry_run: false,
            presenced_unit_dir: None,
            presenced_from: None,
            control: DEFAULT_CONTROL_FLAG.to_string(),
            with_u2f_fallback: false,
        }
    }

    #[test]
    fn recognition_regex_matches_canonical_line() {
        assert!(recognition_regex().is_match("auth    required    pam_syauth.so timeout=1200"));
    }

    #[test]
    fn recognition_regex_matches_indented_variant() {
        assert!(recognition_regex().is_match("  auth required pam_syauth.so debug"));
    }

    #[test]
    fn recognition_regex_does_not_false_positive_on_lookalike_module() {
        assert!(!recognition_regex().is_match("auth required pam_syauth_legacy.so"));
    }

    #[test]
    fn is_auth_directive_recognizes_typical_lines() {
        assert!(is_auth_directive("auth required pam_unix.so\n"));
        assert!(is_auth_directive("  auth    sufficient pam_x.so\n"));
        assert!(!is_auth_directive("account required pam_unix.so\n"));
        assert!(!is_auth_directive("#auth required pam_unix.so\n"));
        assert!(!is_auth_directive("auth_helper\n"));
    }

    #[test]
    fn insert_line_places_before_first_auth() {
        let new_line = "auth    required    pam_syauth.so timeout=1200";
        let got = insert_line(SAMPLE, new_line);
        let canonical_idx = got.find(new_line).expect("inserted line present");
        let first_auth = got.find("auth       include      system-auth").expect("original auth preserved");
        assert!(canonical_idx < first_auth);
    }

    #[test]
    fn insert_line_no_auth_directive_prepends() {
        let body = "#%PAM-1.0\naccount required pam_permit.so\n";
        let got = insert_line(body, "auth    required    pam_syauth.so timeout=1200");
        assert!(got.starts_with("auth    required    pam_syauth.so timeout=1200\n"));
    }

    #[test]
    fn build_line_uses_documented_defaults() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let line = build_line(&opts_for(dir.path()));
        assert_eq!(line, "auth    sufficient    pam_syauth.so timeout=8000");
    }

    #[test]
    fn build_line_honours_custom_control_flag() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let mut opts = opts_for(dir.path());
        opts.control = "required".to_string();
        let line = build_line(&opts);
        assert_eq!(line, "auth    required    pam_syauth.so timeout=8000");
    }

    #[test]
    fn u2f_fallback_inserted_when_flag_set_and_no_existing_u2f() {
        let body = "#%PAM-1.0\nauth    sufficient    pam_syauth.so timeout=8000\nauth       substack     system-auth\n";
        let got = maybe_insert_u2f_fallback(body, true);
        assert!(got.contains("auth    sufficient    pam_u2f.so cue"));
        let syauth_idx = got.find("pam_syauth.so").expect("syauth present");
        let u2f_idx = got.find("pam_u2f.so").expect("u2f inserted");
        let substack_idx = got.find("substack").expect("substack present");
        assert!(syauth_idx < u2f_idx);
        assert!(u2f_idx < substack_idx);
    }

    #[test]
    fn u2f_fallback_is_noop_when_flag_unset() {
        let body = "#%PAM-1.0\nauth    sufficient    pam_syauth.so timeout=8000\nauth       substack     system-auth\n";
        let got = maybe_insert_u2f_fallback(body, false);
        assert_eq!(got, body);
    }

    #[test]
    fn u2f_fallback_is_noop_when_already_present() {
        let body = "#%PAM-1.0\nauth    sufficient    pam_syauth.so timeout=8000\nauth        sufficient    pam_u2f.so cue\nauth       substack     system-auth\n";
        let got = maybe_insert_u2f_fallback(body, true);
        assert_eq!(got, body);
    }

    #[test]
    fn install_then_second_install_is_noop() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let svc = dir.path().join("demo");
        fs::write(&svc, SAMPLE).expect("write");
        let out1 = install(&opts_for(dir.path())).expect("install ok");
        match out1 {
            InstallOutcome::Installed { .. } => {}
            other => panic!("expected Installed, got {other:?}"),
        }
        let after = fs::read(&svc).expect("read after");
        let out2 = install(&opts_for(dir.path())).expect("install ok 2");
        match out2 {
            InstallOutcome::AlreadyInstalled { .. } => {}
            other => panic!("expected AlreadyInstalled, got {other:?}"),
        }
        assert_eq!(fs::read(&svc).expect("read after 2"), after);
    }

    #[test]
    fn install_refuses_existing_bak() {
        let dir = tempfile::tempdir().expect("tmpdir");
        fs::write(dir.path().join("demo"), SAMPLE).expect("write svc");
        fs::write(dir.path().join("demo.bak"), b"other").expect("write bak");
        let err = install(&opts_for(dir.path())).expect_err("must refuse");
        assert!(matches!(err, InstallError::BackupExists(_)));
    }

    #[test]
    fn install_errors_on_missing_service() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let err = install(&opts_for(dir.path())).expect_err("missing");
        assert!(matches!(err, InstallError::ServiceNotFound(_)));
    }
}
