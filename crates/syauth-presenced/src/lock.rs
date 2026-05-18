//! Single-instance pidfile lock.
//!
//! SPEC anchor: `specs/unlock-proximity/SPEC.md` §3 Approach
//! (single-instance per user, locks
//! `${XDG_RUNTIME_DIR}/syauth/presenced.pid`).
//! Roadmap row: S-001 DoR / DoD.
//! Journey: `specs/journeys/JOURNEY-S-001-scaffold-syauth-presenced.md`.
//!
//! The lock is an open-file-description exclusive `fcntl(F_OFD_SETLK)`
//! lock held on an open file descriptor pointing at the pidfile. The
//! kernel releases the lock automatically when the descriptor closes
//! (process exit, crash, kill -9), so there is no "stale lockfile
//! blocks restart" failure mode. Drop semantics also `unlink(2)` the
//! pidfile path so a clean shutdown leaves no debris behind.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write as _},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt as _},
    path::{Path, PathBuf},
};

use nix::{
    errno::Errno,
    fcntl::{FcntlArg, fcntl},
};

/// Filesystem mode for the pidfile. Matches SPEC §7 Security:
/// "Unix socket: ACL via 0600 mode" — same posture for the pidfile so
/// only the owning UID can read/write it.
const PIDFILE_MODE: u32 = 0o600;

/// Directory mode for the parent `${XDG_RUNTIME_DIR}/syauth/`.
/// Matches the SPEC's per-user-tmpfs assumption.
const PIDFILE_DIR_MODE: u32 = 0o700;

/// Typed errors for the pidfile lock acquisition path. The crate's
/// `anyhow::Error` boundary in `runtime::run` wraps these so the
/// process exit path can branch on the kind without string matching.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another `syauth-presenced` instance is already holding the lock
    /// on this pidfile. Smoke test TC-02 asserts this is the failure
    /// path for the second instance.
    #[error("another syauth-presenced instance is already running (pidfile: {path})")]
    AlreadyRunning {
        /// Pidfile whose lock could not be acquired.
        path: PathBuf,
    },
    /// Could not create the pidfile's parent directory.
    #[error("failed to create pidfile parent directory {path}: {source}")]
    ParentDir {
        /// Parent directory path the daemon tried to create.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// Could not open the pidfile for writing.
    #[error("failed to open pidfile {path}: {source}")]
    Open {
        /// Pidfile path the daemon tried to open.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// `fcntl(F_OFD_SETLK)` failed for a reason other than
    /// `EAGAIN`/`EACCES` (those map to `AlreadyRunning`).
    #[error("failed to lock pidfile {path}: {source}")]
    Fcntl {
        /// Pidfile path whose lock attempt failed.
        path: PathBuf,
        /// Underlying nix errno.
        #[source]
        source: Errno,
    },
    /// Could not write the daemon's PID into the pidfile body.
    #[error("failed to write PID into pidfile {path}: {source}")]
    Write {
        /// Pidfile path the daemon tried to write to.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
}

/// RAII guard wrapping the pidfile's open descriptor + on-disk path.
/// Holding the guard keeps the lock; dropping it releases the lock
/// (kernel-side) and unlinks the pidfile path.
#[derive(Debug)]
pub struct PidFileLock {
    /// Owned file descriptor. Drop closes it; closing the FD releases
    /// the F_OFD_SETLK lock per POSIX.
    file: File,
    /// Filesystem path so Drop can unlink it.
    path: PathBuf,
}

impl PidFileLock {
    /// Acquire the single-instance lock at `path`. Creates parent
    /// directories with `0700` if needed, opens the pidfile with mode
    /// `0600`, takes an exclusive `fcntl(F_OFD_SETLK)` lock, and writes
    /// the caller's PID as decimal bytes for audit/debugging.
    ///
    /// Returns `LockError::AlreadyRunning` if another process already
    /// holds the lock (TC-02 contract).
    pub fn acquire(path: &Path) -> Result<Self, LockError> {
        ensure_parent_dir(path)?;
        let mut file = open_pidfile(path)?;
        try_lock_exclusive(&file, path)?;
        write_pid(&mut file, path)?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    /// Pidfile path the guard is responsible for. Used by tests + by
    /// the runtime layer's `Drop` cleanup logging.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PidFileLock {
    fn drop(&mut self) {
        // Best-effort unlink. If unlink fails (already gone, permission
        // change, fs unmounted), there is no useful recovery; we close
        // the FD anyway when `file` drops, which releases the
        // kernel-side lock. Log at debug to keep the shutdown path
        // observable without being noisy.
        if let Err(err) = fs::remove_file(&self.path) {
            tracing::debug!(
                path = %self.path.display(),
                error = %err,
                "pidfile unlink failed during PidFileLock drop"
            );
        }
        // Touch `self.file` so the dead-code lint sees the field as
        // observed. The substantive Drop work is the close that
        // happens implicitly when `self.file` falls out of scope at
        // the end of this method — that is what releases the
        // kernel-side F_OFD_SETLK lock.
        let _ = self.file.metadata();
    }
}

fn ensure_parent_dir(path: &Path) -> Result<(), LockError> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => return Ok(()),
    };
    fs::create_dir_all(parent).map_err(|source| LockError::ParentDir {
        path: parent.to_path_buf(),
        source,
    })?;
    // Tighten perms to `0700` for newly-created dirs; ignore failures
    // for pre-existing dirs the operator owns with different (looser)
    // perms — the lock semantic does not depend on dir mode.
    if let Ok(meta) = fs::metadata(parent) {
        use std::os::unix::fs::PermissionsExt as _;
        if meta.permissions().mode() & 0o777 != PIDFILE_DIR_MODE {
            let mut perms = meta.permissions();
            perms.set_mode(PIDFILE_DIR_MODE);
            let _ = fs::set_permissions(parent, perms);
        }
    }
    Ok(())
}

fn open_pidfile(path: &Path) -> Result<File, LockError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(PIDFILE_MODE)
        .open(path)
        .map_err(|source| LockError::Open {
            path: path.to_path_buf(),
            source,
        })
}

fn try_lock_exclusive(file: &File, path: &Path) -> Result<(), LockError> {
    let spec = exclusive_lock_spec();
    match fcntl(file.as_raw_fd(), FcntlArg::F_OFD_SETLK(&spec)) {
        Ok(_) => Ok(()),
        Err(Errno::EAGAIN | Errno::EACCES) => Err(LockError::AlreadyRunning { path: path.to_path_buf() }),
        Err(source) => Err(LockError::Fcntl {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Construct the `flock` argument for an exclusive whole-file lock.
/// `l_start = 0` + `l_len = 0` means "lock the entire file" per
/// `fcntl(2)`; defined out-of-line so the magic-zero arithmetic is
/// named and reviewable.
fn exclusive_lock_spec() -> libc::flock {
    libc::flock {
        l_type: libc::F_WRLCK as i16,
        l_whence: libc::SEEK_SET as i16,
        l_start: 0,
        l_len: 0,
        l_pid: 0,
    }
}

fn write_pid(file: &mut File, path: &Path) -> Result<(), LockError> {
    // Truncate any prior content (could be a stale unlocked pidfile
    // that the kernel cleaned up the lock for) and write the current
    // PID in decimal followed by a newline so `cat` / journalctl
    // output is readable.
    file.set_len(0).map_err(|source| LockError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    let pid_line = format!("{}\n", std::process::id());
    file.write_all(pid_line.as_bytes()).map_err(|source| LockError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}
