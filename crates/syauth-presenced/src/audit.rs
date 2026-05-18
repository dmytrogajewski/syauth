//! `AuditLog` — append-only audit appender for the daemon's
//! challenge transaction flow (SPEC §3 scope item #8 + §7 Audit +
//! §8 Risks row "Daemon writes audit log faster than disk flushes").
//!
//! SPEC anchor: `specs/unlock-proximity/SPEC.md` §3 scope item #8
//! ("Audit: every challenge transaction writes one structured line to
//! `/var/lib/syauth/last.log` with `peer_id, nonce_hex, outcome,
//! elapsed_ms`"), §7 Audit ("`/var/lib/syauth/last.log` (append-only):
//! one line per challenge tx"), §8 Risks row ("the daemon `O_APPEND`s
//! and fsync()s every 32 transactions").
//!
//! Roadmap row: `specs/unlock-proximity/ROADMAP.md` Step S-006.
//! Journey: `specs/journeys/JOURNEY-S-006-challenge-transaction-flow.md`.
//!
//! Field layout on disk (comma-separated, one line per record):
//!
//! ```text
//! <peer_id>,<nonce_hex>,<t_start_epoch_ms>,<t_end_epoch_ms>,<outcome>,<reason>\n
//! ```
//!
//! The comma is the documented field separator
//! ([`AUDIT_FIELD_SEPARATOR`]); none of the fields written by the
//! orchestrator contain a comma. `nonce_hex` is exactly
//! `2 * syauth_core::NONCE_LEN = 32` lowercase hex chars; `outcome`
//! and `reason` are the typed reason strings declared in
//! `orchestrator.rs`.

use std::{
    fs::{File, OpenOptions},
    io::{self, Write as _},
    os::unix::fs::OpenOptionsExt as _,
    path::{Path, PathBuf},
};

/// Filesystem mode applied to the audit log on create. The audit
/// log is operator-readable, world-unreadable per SPEC §7 Audit +
/// §8 Risks. `0o600` matches `bonds.toml` and
/// `keys/<peer_id>.bin`.
pub const AUDIT_LOG_FILE_MODE: u32 = 0o600;

/// Number of appends between `file.sync_all()` calls. SPEC §8 Risks
/// row "the daemon `O_APPEND`s and fsync()s every 32 transactions" —
/// the constant is named so the SPEC anchor is grep-able from the
/// source.
pub const AUDIT_FSYNC_EVERY: u64 = 32;

/// Field separator written between columns of one audit line.
/// Comma chosen because none of the fields the orchestrator writes
/// (`peer_id` is hex, `nonce_hex` is hex, the two timestamps are
/// decimal integers, `outcome` and `reason` are typed enum tags
/// drawn from a fixed kebab-case set) contains a comma; the
/// separator is therefore unambiguous without quoting.
pub const AUDIT_FIELD_SEPARATOR: &str = ",";

/// Per-transaction record appended to the audit log. Borrowed
/// strings because the orchestrator owns the `String` allocations
/// for `outcome` / `reason` (typed constants); a borrowed view
/// avoids one allocation per audit append on the hot path.
#[derive(Debug, Clone, Copy)]
pub struct AuditRecord<'a> {
    /// Bond identifier the challenge targeted.
    pub peer_id: &'a str,
    /// Lowercase hex render of the per-challenge 16-byte nonce.
    /// Exactly `2 * syauth_core::NONCE_LEN = 32` chars.
    pub nonce_hex: &'a str,
    /// Wall-clock time the orchestrator generated the nonce
    /// (`epoch_ms`). Audit columns are decimal so a human can
    /// `awk -F, '{print $4 - $3}'` to derive `elapsed_ms`.
    pub t_start_ms: u128,
    /// Wall-clock time the orchestrator returned from
    /// `issue_challenge` (`epoch_ms`). May equal `t_start_ms` if the
    /// outcome short-circuited (e.g., `UnknownPeer` before the
    /// notify round-trip).
    pub t_end_ms: u128,
    /// Typed enum tag for the outcome (e.g., `"ok"`, `"denied"`,
    /// `"response-timeout"`, `"bad-signature"`).
    pub outcome: &'a str,
    /// Operator-facing reason string. Today this duplicates
    /// `outcome` (the typed reason ⇔ outcome mapping is 1:1 for
    /// S-006), but the column stays so the wire-level
    /// `Response::Challenge { reason }` field has a 1:1 audit
    /// column for a future row where the two diverge (e.g.,
    /// `outcome=denied, reason=biometric-fail`).
    pub reason: &'a str,
}

/// Append-only audit appender. Holds the open file plus a
/// fsync-cadence counter. `Send` because all fields are `Send`.
#[derive(Debug)]
pub struct AuditLog {
    /// The on-disk path. Held for diagnostics in error rendering.
    path: PathBuf,
    /// The append-only file handle.
    file: File,
    /// Number of records appended since the last fsync. Wraps via
    /// `% AUDIT_FSYNC_EVERY`.
    appended_since_fsync: u64,
}

impl AuditLog {
    /// Open `path` with `O_APPEND | O_CREATE` and mode
    /// [`AUDIT_LOG_FILE_MODE`]. The append flag is the kernel-level
    /// atomic-position-write primitive; concurrent writers cannot
    /// interleave partial lines.
    ///
    /// # Errors
    ///
    /// Returns the underlying `std::io::Error` if the parent
    /// directory is missing or unwritable.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).mode(AUDIT_LOG_FILE_MODE).open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            appended_since_fsync: 0,
        })
    }

    /// Append one record to the log. Calls `file.sync_all()` every
    /// [`AUDIT_FSYNC_EVERY`] appends.
    ///
    /// # Errors
    ///
    /// Returns the underlying `std::io::Error` if the write or the
    /// (periodic) fsync fails.
    pub fn append(&mut self, record: &AuditRecord<'_>) -> io::Result<()> {
        let line = format_record(record);
        self.file.write_all(line.as_bytes())?;
        self.appended_since_fsync = self.appended_since_fsync.wrapping_add(1);
        if self.appended_since_fsync % AUDIT_FSYNC_EVERY == 0 {
            self.file.sync_all()?;
        }
        Ok(())
    }

    /// Return the on-disk path the appender was opened against.
    /// Useful for the orchestrator's tracing breadcrumbs.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Format `record` as one trailing-newline-terminated comma-separated
/// audit line. Free function so the unit tests pin the on-disk shape
/// without touching the file system.
fn format_record(record: &AuditRecord<'_>) -> String {
    format!(
        "{peer}{sep}{nonce}{sep}{ts}{sep}{te}{sep}{outcome}{sep}{reason}\n",
        peer = record.peer_id,
        nonce = record.nonce_hex,
        ts = record.t_start_ms,
        te = record.t_end_ms,
        outcome = record.outcome,
        reason = record.reason,
        sep = AUDIT_FIELD_SEPARATOR,
    )
}

#[cfg(test)]
mod tests {
    // Journey: specs/journeys/JOURNEY-S-006-challenge-transaction-flow.md
    use std::os::unix::fs::PermissionsExt as _;

    use tempfile::tempdir;

    use super::*;

    /// Pinned audit record fixture for the format pin-down tests.
    fn fixture_record() -> AuditRecord<'static> {
        AuditRecord {
            peer_id: "abc123",
            nonce_hex: "0011223344556677",
            t_start_ms: 1_700_000_000_000,
            t_end_ms: 1_700_000_000_500,
            outcome: "ok",
            reason: "ok",
        }
    }

    #[test]
    fn format_record_emits_comma_separated_line_with_newline() {
        let line = format_record(&fixture_record());
        assert_eq!(line, "abc123,0011223344556677,1700000000000,1700000000500,ok,ok\n");
    }

    #[test]
    fn open_creates_file_with_audit_mode() {
        let td = tempdir().expect("tempdir");
        let path = td.path().join("audit.log");
        let _log = AuditLog::open(&path).expect("open succeeds");
        let meta = std::fs::metadata(&path).expect("metadata");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, AUDIT_LOG_FILE_MODE);
    }

    #[test]
    fn append_grows_file_by_one_line_per_record() {
        let td = tempdir().expect("tempdir");
        let path = td.path().join("audit.log");
        let mut log = AuditLog::open(&path).expect("open succeeds");
        log.append(&fixture_record()).expect("append 1");
        log.append(&fixture_record()).expect("append 2");
        log.append(&fixture_record()).expect("append 3");
        let contents = std::fs::read_to_string(&path).expect("read");
        assert_eq!(contents.lines().count(), 3);
    }

    #[test]
    fn open_appends_when_file_already_exists() {
        let td = tempdir().expect("tempdir");
        let path = td.path().join("audit.log");
        {
            let mut log = AuditLog::open(&path).expect("first open");
            log.append(&fixture_record()).expect("first append");
        }
        {
            let mut log = AuditLog::open(&path).expect("second open");
            log.append(&fixture_record()).expect("second append");
        }
        let contents = std::fs::read_to_string(&path).expect("read");
        assert_eq!(
            contents.lines().count(),
            2,
            "second open must append, not truncate; got {contents:?}"
        );
    }
}
