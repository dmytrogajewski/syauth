//! `syauth uninstall-pam` — restore a PAM service file from its `.bak` and
//! remove the bak.
//!
//! Contract (S-013):
//! * If no recognizable syauth line is present, exit 0 with a WARN; never
//!   delete a bak we did not write.
//! * If a syauth line is present and a `.bak` exists, atomically replace
//!   the service file with the bak's bytes, then delete the bak.
//! * If a syauth line is present but `.bak` is missing, refuse with a
//!   non-zero exit and an actionable error.
//!
//! Journey: specs/journeys/JOURNEY-S-013-pam-install-helper.md

use std::{
    fs,
    io::Write as _,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use clap::Parser;
use thiserror::Error;

use crate::install_pam::{BACKUP_SUFFIX, DEFAULT_PAM_DIR};

/// CLI options for `uninstall-pam`.
#[derive(Debug, Parser)]
pub struct UninstallOpts {
    /// PAM service to uninstall from.
    #[arg(long)]
    pub service: String,

    /// PAM service directory. Defaults to `/etc/pam.d`. Tests inject a tempdir.
    #[arg(long, default_value = DEFAULT_PAM_DIR)]
    pub pam_dir: PathBuf,

    /// Skip the interactive confirmation prompt.
    #[arg(long)]
    pub yes: bool,
}

/// Typed error surface for uninstall.
#[derive(Debug, Error)]
pub enum UninstallError {
    /// The service file does not exist under `--pam-dir`.
    #[error("PAM service file not found: {0}")]
    ServiceNotFound(PathBuf),

    /// The service file references syauth but no backup is on disk.
    /// Refusing to remove the syauth line without a known-good restore.
    #[error(
        "no backup found at {0}; refusing to remove the syauth line without restoring known-good state. Restore the affected {1} manually or rerun install-pam to recreate the bond."
    )]
    BackupMissing(PathBuf, #[source] BackupMissingService),

    /// The service file is not valid UTF-8.
    #[error("PAM service file at {path} is not valid UTF-8: {source}")]
    NotUtf8 {
        /// Path read.
        path: PathBuf,
        /// Originating decode error.
        #[source]
        source: std::string::FromUtf8Error,
    },

    /// Generic I/O failure.
    #[error("I/O error on {path}: {source}")]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Originating I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Atomic persist of the restored file failed.
    #[error("atomic persist of {path} failed: {source}")]
    Persist {
        /// Destination path.
        path: PathBuf,
        /// Originating persist error.
        #[source]
        source: tempfile::PersistError,
    },
}

/// Carrier so `Display` can name both the missing bak and the affected
/// service. `BackupMissing(bak, BackupMissingService(service))` keeps a
/// single typed surface.
#[derive(Debug)]
pub struct BackupMissingService(pub PathBuf);

impl std::fmt::Display for BackupMissingService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "service file: {}", self.0.display())
    }
}

impl std::error::Error for BackupMissingService {}

/// Outcome of an `uninstall` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UninstallOutcome {
    /// No recognizable syauth line in the service file; no changes made.
    NotInstalled {
        /// Path inspected.
        path: PathBuf,
    },
    /// Service file was restored from the backup; the backup was removed.
    Restored {
        /// Service path (post-restore).
        service: PathBuf,
        /// Backup path (now removed).
        backup: PathBuf,
    },
}

fn service_path(pam_dir: &Path, service: &str) -> PathBuf {
    pam_dir.join(service)
}

fn backup_path(pam_dir: &Path, service: &str) -> PathBuf {
    pam_dir.join(format!("{service}{BACKUP_SUFFIX}"))
}

fn read_utf8(path: &Path) -> Result<String, UninstallError> {
    let bytes = fs::read(path).map_err(|source| UninstallError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    String::from_utf8(bytes).map_err(|source| UninstallError::NotUtf8 {
        path: path.to_path_buf(),
        source,
    })
}

fn atomic_restore(target: &Path, bytes: &[u8], mode: u32) -> Result<(), UninstallError> {
    let parent = target.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|source| UninstallError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    tmp.write_all(bytes).map_err(|source| UninstallError::Io {
        path: tmp.path().to_path_buf(),
        source,
    })?;
    tmp.as_file().sync_all().map_err(|source| UninstallError::Io {
        path: tmp.path().to_path_buf(),
        source,
    })?;
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(mode)).map_err(|source| UninstallError::Io {
        path: tmp.path().to_path_buf(),
        source,
    })?;
    tmp.persist(target).map_err(|source| UninstallError::Persist {
        path: target.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn file_mode(path: &Path) -> Result<u32, UninstallError> {
    let meta = fs::metadata(path).map_err(|source| UninstallError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(meta.permissions().mode() & 0o7777)
}

/// Drives the uninstall workflow.
///
/// # Errors
///
/// Returns [`UninstallError`] when the service file is missing, the bak is
/// absent while the syauth line is present, the file is not UTF-8, or an
/// I/O / persist call fails.
pub fn uninstall(opts: &UninstallOpts) -> Result<UninstallOutcome, UninstallError> {
    let service = service_path(&opts.pam_dir, &opts.service);
    if !service.exists() {
        return Err(UninstallError::ServiceNotFound(service));
    }
    let body = read_utf8(&service)?;
    if !crate::install_pam::recognition_regex_match(&body) {
        return Ok(UninstallOutcome::NotInstalled { path: service });
    }
    let backup = backup_path(&opts.pam_dir, &opts.service);
    if !backup.exists() {
        return Err(UninstallError::BackupMissing(backup, BackupMissingService(service)));
    }
    let bak_bytes = fs::read(&backup).map_err(|source| UninstallError::Io {
        path: backup.clone(),
        source,
    })?;
    // Preserve the *current* file's mode; PAM expects stable perms across
    // edits, and the bak's mode was already aligned to the original.
    let mode = file_mode(&service)?;
    atomic_restore(&service, &bak_bytes, mode)?;
    fs::remove_file(&backup).map_err(|source| UninstallError::Io {
        path: backup.clone(),
        source,
    })?;
    Ok(UninstallOutcome::Restored { service, backup })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANONICAL: &str = "auth    required    pam_syauth.so timeout=1200\n";
    const ORIGINAL: &str = "#%PAM-1.0\nauth       include      system-auth\n";

    fn opts(dir: &Path) -> UninstallOpts {
        UninstallOpts {
            service: "demo".to_string(),
            pam_dir: dir.to_path_buf(),
            yes: true,
        }
    }

    #[test]
    fn noop_when_no_syauth_line() {
        let dir = tempfile::tempdir().expect("tmpdir");
        fs::write(dir.path().join("demo"), ORIGINAL).expect("svc");
        // Pre-existing unrelated bak — must NOT be touched.
        fs::write(dir.path().join("demo.bak"), b"other tool").expect("bak");
        let out = uninstall(&opts(dir.path())).expect("noop");
        assert!(matches!(out, UninstallOutcome::NotInstalled { .. }));
        assert_eq!(fs::read(dir.path().join("demo.bak")).expect("re-read"), b"other tool");
    }

    #[test]
    fn refuses_when_bak_missing_but_line_present() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let body = format!("{CANONICAL}{ORIGINAL}");
        fs::write(dir.path().join("demo"), body).expect("svc");
        let err = uninstall(&opts(dir.path())).expect_err("must refuse");
        assert!(matches!(err, UninstallError::BackupMissing(_, _)));
    }

    #[test]
    fn restores_byte_equality_and_removes_bak() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let svc = dir.path().join("demo");
        let bak = dir.path().join("demo.bak");
        let body = format!("{CANONICAL}{ORIGINAL}");
        fs::write(&svc, body).expect("svc");
        fs::write(&bak, ORIGINAL).expect("bak");
        let out = uninstall(&opts(dir.path())).expect("ok");
        assert!(matches!(out, UninstallOutcome::Restored { .. }));
        assert_eq!(fs::read(&svc).expect("read svc"), ORIGINAL.as_bytes());
        assert!(!bak.exists(), "bak must be removed");
    }
}
