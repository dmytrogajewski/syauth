//! `syauth-cli` — `status` subcommand.
//!
//! Read-only day-2 diagnostic. Prints, in order, five labeled lines:
//!
//! ```text
//! adapter:           <name>
//! adapter-state:     <Powered | Down | Missing>
//! advertising:       <true|false>
//! bonds-count:       <N>
//! last-unlock:       <timestamp>  <outcome>  <peer-id>     (or "(no entries)")
//! ```
//!
//! `status` is read-only by contract: it never creates, truncates, or
//! rotates `last.log` or `bonds.toml`. Missing adapter / missing log file
//! / empty log file are all soft-fail paths (no error, no exit code !=
//! 0) so the operator can run `syauth status` anywhere — including a CI
//! host without a BT dongle — and get a useful diagnostic.
//!
//! The PAM module (S-009) is the writer of `last.log`. The format pinned
//! here is `<RFC3339 timestamp> <success|failure> <peer-id>` per
//! whitespace-separated line; the writer is expected to keep the file
//! bounded — [`LAST_UNLOCK_LOG_MAX_LINES`] is this reader's defensive
//! cap.
//!
//! Roadmap: specs/syauth/ROADMAP.md item S-012.
//! Journey: specs/journeys/JOURNEY-S-012-day2-cli.md

use std::{
    fs,
    io::{self, BufRead, Write},
    path::PathBuf,
};

use clap::Parser;
use syauth_core::BondStore;
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::pair::{DEFAULT_ADAPTER_NAME, DEFAULT_BOND_DIR, bonds_path};

/// File name (within `--bond-dir`) where the PAM module writes
/// per-unlock outcome lines. Defined here so a future test in
/// `syauth-pam` can re-import the same const instead of hard-coding the
/// path.
pub const LAST_UNLOCK_LOG_FILENAME: &str = "last.log";

/// Defensive read cap. Even if the on-disk file is unexpectedly
/// unbounded, we read at most this many lines and only parse the last
/// one. The PAM writer is expected to keep the file under this cap; the
/// cap exists so a `cat /dev/random >> last.log` accident does not turn
/// `syauth status` into an OOM.
pub const LAST_UNLOCK_LOG_MAX_LINES: usize = 64;

/// In v0.1 the advertising lifecycle lives in S-018. Until then,
/// `status` reports a constant `false`. The labeled line is still
/// printed so the operator-facing surface (and our help snapshot) is
/// stable across the S-018 transition.
pub const ADVERTISING_STATE_V01: bool = false;

/// Token printed when `last.log` is missing or empty.
pub const LAST_UNLOCK_NO_ENTRIES: &str = "(no entries)";

/// Outcome token printed when the last log line cannot be parsed into
/// `<timestamp> <outcome> <peer-id>`. Surfaces the offending line
/// verbatim so the operator can fix it without re-greppping.
pub const LAST_UNLOCK_UNPARSEABLE_PREFIX: &str = "(unparseable: ";

/// CLI options for the `status` subcommand.
#[derive(Debug, Parser, Clone)]
pub struct StatusOpts {
    /// BlueZ adapter id (e.g. `hci0`).
    #[arg(long, default_value = DEFAULT_ADAPTER_NAME)]
    pub adapter: String,

    /// Directory holding the bonds file. Defaults to SPEC's
    /// `/var/lib/syauth/`. Tests inject a tempdir.
    #[arg(long, default_value = DEFAULT_BOND_DIR)]
    pub bond_dir: PathBuf,

    /// Path to the rolling unlock log. Defaults to
    /// `<bond-dir>/last.log`. Tests inject a fixture path.
    #[arg(long)]
    pub last_log: Option<PathBuf>,
}

/// Typed errors produced by [`run_status`].
#[derive(Debug, Error)]
pub enum StatusError {
    /// Bond store I/O or contract failure.
    #[error("bond store error: {0}")]
    Bond(#[from] syauth_core::BondError),

    /// Stdio I/O error.
    #[error("status i/o error: {0}")]
    Io(#[from] io::Error),
}

/// Three-valued adapter state. `Missing` is the soft-fail path when
/// BlueZ does not know the requested adapter id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterState {
    /// Adapter is known to BlueZ and is powered on.
    Powered,
    /// Adapter is known to BlueZ but is currently powered down.
    Down,
    /// Adapter is unknown to BlueZ (typical on CI / containers / hosts
    /// without a BT dongle).
    Missing,
}

impl AdapterState {
    /// Render the variant as the exact token printed on the
    /// `adapter-state:` line. Pinned by the integration test.
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Powered => "Powered",
            Self::Down => "Down",
            Self::Missing => "Missing",
        }
    }
}

/// One parsed line from `last.log` plus its raw timestamp string. The
/// printer renders the raw timestamp verbatim (RFC3339 round-trip) so
/// the operator sees exactly what the PAM writer recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastUnlockEntry {
    /// Raw RFC3339 timestamp as it appears in the file.
    pub timestamp: String,
    /// `success` or `failure`. Anything else is rejected by [`parse_last_log_line`].
    pub outcome: String,
    /// Peer id (hex string). Opaque to this module.
    pub peer_id: String,
}

/// Strict line parser for `last.log` lines.
///
/// Format: `<RFC3339 timestamp> <success|failure> <peer-id>` separated
/// by single ASCII spaces. Returns `None` for any deviation; the caller
/// surfaces the deviation via [`LAST_UNLOCK_UNPARSEABLE_PREFIX`].
pub fn parse_last_log_line(line: &str) -> Option<LastUnlockEntry> {
    let line = line.trim_end_matches(['\n', '\r']);
    let mut parts = line.split_ascii_whitespace();
    let ts = parts.next()?;
    let outcome = parts.next()?;
    let peer_id = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if outcome != "success" && outcome != "failure" {
        return None;
    }
    // Round-trip validation: timestamp must be parseable as RFC3339.
    OffsetDateTime::parse(ts, &Rfc3339).ok()?;
    Some(LastUnlockEntry {
        timestamp: ts.to_owned(),
        outcome: outcome.to_owned(),
        peer_id: peer_id.to_owned(),
    })
}

/// Three-valued last-unlock view, ready for printing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LastUnlockView {
    /// `last.log` is missing or contains zero parseable lines.
    NoEntries,
    /// Last line parsed cleanly.
    Entry(LastUnlockEntry),
    /// `last.log` exists with at least one line but the last line is
    /// malformed. The raw bytes are surfaced so the operator can fix.
    Unparseable {
        /// The offending raw line (trimmed of trailing newlines).
        raw: String,
    },
}

/// Read the last (at most [`LAST_UNLOCK_LOG_MAX_LINES`]) lines from
/// `path` and reduce them to a [`LastUnlockView`].
///
/// `path` missing → `NoEntries`. `path` empty → `NoEntries`. Last line
/// non-empty but unparseable → `Unparseable { raw }`. Last line
/// parseable → `Entry { ... }`.
///
/// Never writes to or truncates the file.
///
/// # Errors
///
/// Returns [`StatusError::Io`] only for I/O errors other than
/// `NotFound` (e.g. permission denied).
pub fn read_last_unlock(path: &std::path::Path) -> Result<LastUnlockView, StatusError> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(LastUnlockView::NoEntries),
        Err(e) => return Err(StatusError::Io(e)),
    };
    let mut last_non_empty: Option<String> = None;
    let mut lines_read: usize = 0;
    for line_res in io::BufReader::new(file).lines() {
        if lines_read >= LAST_UNLOCK_LOG_MAX_LINES {
            break;
        }
        lines_read = lines_read.saturating_add(1);
        let line = line_res?;
        if !line.trim().is_empty() {
            last_non_empty = Some(line);
        }
    }
    match last_non_empty {
        None => Ok(LastUnlockView::NoEntries),
        Some(raw) => match parse_last_log_line(&raw) {
            Some(entry) => Ok(LastUnlockView::Entry(entry)),
            None => Ok(LastUnlockView::Unparseable { raw }),
        },
    }
}

/// Adapter introspection seam. The production wiring (`bluer::Session`)
/// lives in [`probe_adapter_state`]; the integration test injects a
/// closure-shaped probe so it never touches the host's BlueZ.
pub trait AdapterProbe: Send + Sync {
    /// Return the adapter state for `adapter_id`. `Missing` is the
    /// expected return on hosts without that adapter (NOT an error).
    fn probe(&self, adapter_id: &str) -> AdapterState;
}

/// Default production probe: opens a `bluer::Session` and asks BlueZ
/// for the adapter, mapping every error to `Missing`. This is the
/// "diagnostic everywhere" decision from the journey — we'd rather
/// print `Missing` and let the operator move on than panic.
pub struct BluerAdapterProbe;

#[async_trait::async_trait]
impl AsyncAdapterProbe for BluerAdapterProbe {
    async fn probe_async(&self, adapter_id: &str) -> AdapterState {
        bluer_probe(adapter_id).await.unwrap_or(AdapterState::Missing)
    }
}

/// Async query of BlueZ. Any failure (BlueZ absent, dbus error, adapter
/// not in the listed set) folds into `Missing` upstream.
async fn bluer_probe(adapter_id: &str) -> Result<AdapterState, ()> {
    let session = bluer::Session::new().await.map_err(|_| ())?;
    let names = session.adapter_names().await.map_err(|_| ())?;
    if !names.iter().any(|n| n == adapter_id) {
        return Ok(AdapterState::Missing);
    }
    let adapter = session.adapter(adapter_id).map_err(|_| ())?;
    match adapter.is_powered().await {
        Ok(true) => Ok(AdapterState::Powered),
        Ok(false) => Ok(AdapterState::Down),
        Err(_) => Ok(AdapterState::Missing),
    }
}

/// Async sibling of [`AdapterProbe`] — used by the production async
/// dispatcher to query BlueZ without spinning up a nested runtime.
#[async_trait::async_trait]
pub trait AsyncAdapterProbe: Send + Sync {
    /// Return the adapter state for `adapter_id`. `Missing` is the
    /// expected return on hosts without that adapter.
    async fn probe_async(&self, adapter_id: &str) -> AdapterState;
}

/// Snapshot of every field [`render_status_to`] needs. Built by
/// [`gather_status`] (production) or by tests directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSnapshot {
    /// Adapter id (e.g. `hci0`).
    pub adapter: String,
    /// Adapter state (Powered/Down/Missing).
    pub adapter_state: AdapterState,
    /// Whether the host is currently advertising. Hard-coded to
    /// [`ADVERTISING_STATE_V01`] in v0.1.
    pub advertising: bool,
    /// Count of bonds in the bond store.
    pub bonds_count: usize,
    /// Last unlock outcome, ready for printing.
    pub last_unlock: LastUnlockView,
}

/// Render `snapshot` to `writer` as the five labeled lines documented
/// at the module level.
pub fn render_status_to(writer: &mut dyn Write, snapshot: &StatusSnapshot) -> Result<(), StatusError> {
    writeln!(writer, "adapter:           {}", snapshot.adapter)?;
    writeln!(writer, "adapter-state:     {}", snapshot.adapter_state.as_token())?;
    writeln!(writer, "advertising:       {}", snapshot.advertising)?;
    writeln!(writer, "bonds-count:       {}", snapshot.bonds_count)?;
    write!(writer, "last-unlock:       ")?;
    match &snapshot.last_unlock {
        LastUnlockView::NoEntries => writeln!(writer, "{LAST_UNLOCK_NO_ENTRIES}")?,
        LastUnlockView::Entry(entry) => writeln!(writer, "{}  {}  {}", entry.timestamp, entry.outcome, entry.peer_id)?,
        LastUnlockView::Unparseable { raw } => writeln!(writer, "{LAST_UNLOCK_UNPARSEABLE_PREFIX}{raw})")?,
    }
    Ok(())
}

/// Resolve the effective `last.log` path: `--last-log` if set, otherwise
/// `<bond-dir>/<LAST_UNLOCK_LOG_FILENAME>`.
pub fn effective_last_log_path(opts: &StatusOpts) -> PathBuf {
    opts.last_log
        .clone()
        .unwrap_or_else(|| opts.bond_dir.join(LAST_UNLOCK_LOG_FILENAME))
}

/// Gather the [`StatusSnapshot`] against `probe`. Reads `bonds.toml`
/// and `last.log` from disk; calls `probe.probe(adapter)` for the
/// adapter state. Does not write anywhere.
///
/// # Errors
///
/// Returns [`StatusError::Bond`] if the bonds file is malformed.
/// Returns [`StatusError::Io`] for unexpected I/O failure on
/// `last.log` (a `NotFound` is *not* an error — it folds into
/// `NoEntries`).
pub fn gather_status(opts: &StatusOpts, probe: &dyn AdapterProbe) -> Result<StatusSnapshot, StatusError> {
    let path = bonds_path(&opts.bond_dir);
    let store = BondStore::load(&path)?;
    let last_unlock = read_last_unlock(&effective_last_log_path(opts))?;
    Ok(StatusSnapshot {
        adapter: opts.adapter.clone(),
        adapter_state: probe.probe(&opts.adapter),
        advertising: ADVERTISING_STATE_V01,
        bonds_count: store.list().len(),
        last_unlock,
    })
}

/// Async sibling of [`gather_status`] — used by the production
/// dispatcher with an [`AsyncAdapterProbe`] so the bluer query lives on
/// the existing tokio runtime instead of spinning up a nested one.
///
/// # Errors
///
/// See [`gather_status`].
pub async fn gather_status_async(opts: &StatusOpts, probe: &dyn AsyncAdapterProbe) -> Result<StatusSnapshot, StatusError> {
    let path = bonds_path(&opts.bond_dir);
    let store = BondStore::load(&path)?;
    let last_unlock = read_last_unlock(&effective_last_log_path(opts))?;
    let adapter_state = probe.probe_async(&opts.adapter).await;
    Ok(StatusSnapshot {
        adapter: opts.adapter.clone(),
        adapter_state,
        advertising: ADVERTISING_STATE_V01,
        bonds_count: store.list().len(),
        last_unlock,
    })
}

/// Drive `syauth status` end-to-end against the production
/// [`BluerAdapterProbe`].
///
/// # Errors
///
/// Returns [`StatusError`] on bond-store load failure or unexpected I/O.
/// Missing adapter / missing `last.log` are NOT errors.
pub async fn run_status(opts: &StatusOpts) -> Result<(), StatusError> {
    let probe = BluerAdapterProbe;
    let snapshot = gather_status_async(opts, &probe).await?;
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    render_status_to(&mut writer, &snapshot)
}

// ---------------------------------------------------------------------------
// Tests — library-level. Integration test lives in tests/cli.rs.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use tempfile::TempDir;

    use super::*;

    const SAMPLE_TS: &str = "2026-05-15T12:00:00Z";
    const SAMPLE_PEER: &str = "0123456789abcdef0123456789abcdef";

    struct FixedProbe(AdapterState);
    impl AdapterProbe for FixedProbe {
        fn probe(&self, _adapter_id: &str) -> AdapterState {
            self.0
        }
    }

    #[test]
    fn parse_last_log_line_accepts_success() {
        let line = format!("{SAMPLE_TS} success {SAMPLE_PEER}");
        let got = parse_last_log_line(&line).expect("parse");
        assert_eq!(got.timestamp, SAMPLE_TS);
        assert_eq!(got.outcome, "success");
        assert_eq!(got.peer_id, SAMPLE_PEER);
    }

    #[test]
    fn parse_last_log_line_accepts_failure() {
        let line = format!("{SAMPLE_TS} failure {SAMPLE_PEER}\n");
        let got = parse_last_log_line(&line).expect("parse");
        assert_eq!(got.outcome, "failure");
    }

    #[test]
    fn parse_last_log_line_rejects_other_outcome_tokens() {
        let line = format!("{SAMPLE_TS} maybe {SAMPLE_PEER}");
        assert!(parse_last_log_line(&line).is_none());
    }

    #[test]
    fn parse_last_log_line_rejects_extra_fields() {
        let line = format!("{SAMPLE_TS} success {SAMPLE_PEER} extra");
        assert!(parse_last_log_line(&line).is_none());
    }

    #[test]
    fn parse_last_log_line_rejects_bad_timestamp() {
        let line = format!("not-a-timestamp success {SAMPLE_PEER}");
        assert!(parse_last_log_line(&line).is_none());
    }

    #[test]
    fn read_last_unlock_returns_no_entries_for_missing_file() {
        let td = TempDir::new().expect("tempdir");
        let path = td.path().join("last.log");
        let got = read_last_unlock(&path).expect("read");
        assert_eq!(got, LastUnlockView::NoEntries);
    }

    #[test]
    fn read_last_unlock_returns_no_entries_for_empty_file() {
        let td = TempDir::new().expect("tempdir");
        let path = td.path().join("last.log");
        std::fs::write(&path, b"").expect("write");
        let got = read_last_unlock(&path).expect("read");
        assert_eq!(got, LastUnlockView::NoEntries);
    }

    #[test]
    fn read_last_unlock_parses_last_line_of_multi_line_log() {
        let td = TempDir::new().expect("tempdir");
        let path = td.path().join("last.log");
        let body = format!("{SAMPLE_TS} failure {SAMPLE_PEER}\n2026-05-15T12:01:00Z success {SAMPLE_PEER}\n");
        std::fs::write(&path, body).expect("write");
        let got = read_last_unlock(&path).expect("read");
        match got {
            LastUnlockView::Entry(e) => {
                assert_eq!(e.timestamp, "2026-05-15T12:01:00Z");
                assert_eq!(e.outcome, "success");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn read_last_unlock_surfaces_unparseable_last_line() {
        let td = TempDir::new().expect("tempdir");
        let path = td.path().join("last.log");
        std::fs::write(&path, b"this is garbage\n").expect("write");
        let got = read_last_unlock(&path).expect("read");
        match got {
            LastUnlockView::Unparseable { raw } => assert_eq!(raw, "this is garbage"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn render_status_to_includes_all_documented_labels() {
        let snap = StatusSnapshot {
            adapter: "hci0".to_owned(),
            adapter_state: AdapterState::Missing,
            advertising: false,
            bonds_count: 0,
            last_unlock: LastUnlockView::NoEntries,
        };
        let mut buf: Vec<u8> = Vec::new();
        let mut cur = Cursor::new(&mut buf);
        render_status_to(&mut cur, &snap).expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        for label in ["adapter:", "adapter-state:", "advertising:", "bonds-count:", "last-unlock:"] {
            assert!(s.contains(label), "missing {label} in:\n{s}");
        }
        assert!(s.contains("Missing"));
        assert!(s.contains(LAST_UNLOCK_NO_ENTRIES));
    }

    #[test]
    fn render_status_to_prints_parsed_entry() {
        let snap = StatusSnapshot {
            adapter: "hci0".to_owned(),
            adapter_state: AdapterState::Powered,
            advertising: false,
            bonds_count: 2,
            last_unlock: LastUnlockView::Entry(LastUnlockEntry {
                timestamp: SAMPLE_TS.to_owned(),
                outcome: "success".to_owned(),
                peer_id: SAMPLE_PEER.to_owned(),
            }),
        };
        let mut buf: Vec<u8> = Vec::new();
        let mut cur = Cursor::new(&mut buf);
        render_status_to(&mut cur, &snap).expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains(SAMPLE_TS));
        assert!(s.contains("success"));
        assert!(s.contains(SAMPLE_PEER));
        assert!(s.contains("Powered"));
        assert!(s.contains("bonds-count:       2"));
    }

    #[test]
    fn gather_status_uses_probe_result_and_counts_bonds() {
        let td = TempDir::new().expect("tempdir");
        let bond_dir = td.path().to_path_buf();
        let opts = StatusOpts {
            adapter: "test-adapter".to_owned(),
            bond_dir,
            last_log: None,
        };
        let probe = FixedProbe(AdapterState::Down);
        let snap = gather_status(&opts, &probe).expect("gather");
        assert_eq!(snap.adapter, "test-adapter");
        assert_eq!(snap.adapter_state, AdapterState::Down);
        assert_eq!(snap.advertising, ADVERTISING_STATE_V01);
        assert_eq!(snap.bonds_count, 0);
        assert_eq!(snap.last_unlock, LastUnlockView::NoEntries);
    }

    #[test]
    fn effective_last_log_path_falls_back_to_bond_dir_default() {
        let opts = StatusOpts {
            adapter: "hci0".to_owned(),
            bond_dir: PathBuf::from("/tmp/foo"),
            last_log: None,
        };
        assert_eq!(
            effective_last_log_path(&opts),
            PathBuf::from("/tmp/foo").join(LAST_UNLOCK_LOG_FILENAME)
        );
    }

    #[test]
    fn effective_last_log_path_honors_explicit_flag() {
        let opts = StatusOpts {
            adapter: "hci0".to_owned(),
            bond_dir: PathBuf::from("/tmp/foo"),
            last_log: Some(PathBuf::from("/tmp/elsewhere/last.log")),
        };
        assert_eq!(effective_last_log_path(&opts), PathBuf::from("/tmp/elsewhere/last.log"));
    }

    #[test]
    fn adapter_state_as_token_is_stable() {
        assert_eq!(AdapterState::Powered.as_token(), "Powered");
        assert_eq!(AdapterState::Down.as_token(), "Down");
        assert_eq!(AdapterState::Missing.as_token(), "Missing");
    }
}
