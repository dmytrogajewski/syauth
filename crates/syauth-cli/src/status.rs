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
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use serde::Serialize;
use syauth_core::BondStore;
use syauth_presenced::{PeerStatus, Request, Response, read_frame_blocking, write_frame_blocking};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::pair::{DEFAULT_ADAPTER_NAME, DEFAULT_BOND_DIR, bonds_path};

/// Poll cadence applied to `--watch`. Pinned per S-017 prompt
/// ("polls every 1 s and redraws"). One second is short enough to
/// surface a fresh challenge within one redraw, long enough that a
/// loaded daemon does not see >1 Hz status traffic.
pub const WATCH_INTERVAL: Duration = Duration::from_secs(1);

/// ANSI escape sequence to clear the screen and move the cursor home.
/// Emitted at the top of every `--watch` redraw so the table is
/// always rendered in the same position.
pub const WATCH_CLEAR_SCREEN: &str = "\x1b[2J\x1b[H";

/// `connect()` + Status RPC timeout. Mirrors the S-016
/// `doctor::DAEMON_CONNECT_TIMEOUT` so the two subcommands share an
/// SLA on the daemon-down latency budget.
pub const DAEMON_CONNECT_TIMEOUT: Duration = Duration::from_millis(50);

/// Read budget on the daemon's `Response::Status` reply. Same SLA
/// as the S-016 doctor.
pub const DAEMON_STATUS_READ_TIMEOUT: Duration = Duration::from_millis(200);

/// Width in characters of the short rotating-UUID prefix the text
/// renderer prints in the `uuid` column. Matches
/// `syauth_presenced::SHORT_UUID_HEX_LEN = 8` so a
/// `journalctl -t syauth-presenced | grep uuid=<short>` lookup
/// works end-to-end.
pub const SHORT_UUID_HEX_LEN: usize = 8;

/// Default Unix-socket basename under `${XDG_RUNTIME_DIR}/syauth/`.
/// Matches `syauth_presenced::DEFAULT_SOCKET_BASENAME` so the CLI
/// and the daemon agree on the default socket path.
const DEFAULT_SOCKET_BASENAME: &str = "syauth/auth.sock";

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

    /// Override the daemon Unix-socket path. If unset, defaults to
    /// `${XDG_RUNTIME_DIR}/syauth/auth.sock` (or
    /// `/run/user/<uid>/syauth/auth.sock` when env is missing).
    #[arg(long)]
    pub socket: Option<PathBuf>,

    /// Poll the daemon every `WATCH_INTERVAL` and redraw the
    /// status table. Exits cleanly on SIGINT.
    #[arg(long)]
    pub watch: bool,

    /// Emit the daemon section as a typed JSON object via
    /// `serde_json::to_string_pretty`. Same data, different surface
    /// for tooling (waybar pill etc).
    #[arg(long)]
    pub json: bool,
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

    /// `--json` encode failure. Effectively unreachable — the
    /// schema is total — but kept as a typed variant for symmetry.
    #[error("status json encode error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Daemon-reachability outcome. Mirrors `doctor::DaemonState` so the
/// two subcommands share a reason vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum DaemonProbeState {
    /// `connect()` + `Request::Status` succeeded within the
    /// timeout; the daemon's per-peer rows + boot wall-clock time
    /// are carried in.
    Up {
        /// Boot wall-clock time the daemon reported. Serialized as
        /// epoch seconds on the JSON path for non-Rust consumers.
        #[serde(with = "epoch_seconds")]
        started_at: SystemTime,
        /// Per-peer liveness rows the orchestrator emitted.
        peers: Vec<PeerStatus>,
    },
    /// Probe failed. `reason` is a short greppable token
    /// (`socket-missing`, `connect-refused`, `frame-error`,
    /// `timeout`).
    Down {
        /// One-word reason token; never includes whitespace.
        reason: String,
    },
}

/// Typed JSON report shape consumed by the waybar pill in the `sy`
/// repo's roadmap. Field renames are a breaking change requiring a
/// snapshot accept + a changelog row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CliStatusReport {
    /// Resolved daemon socket path.
    pub daemon_socket: PathBuf,
    /// Daemon-reachability outcome (includes per-peer rows on Up).
    pub daemon: DaemonProbeState,
}

mod epoch_seconds {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde::{Serialize, Serializer};

    /// Serialize a `SystemTime` as whole epoch seconds. Times before
    /// the epoch encode as `0` — unreachable on a healthy clock.
    pub(super) fn serialize<S: Serializer>(value: &SystemTime, serializer: S) -> Result<S::Ok, S::Error> {
        let secs = value.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_secs();
        secs.serialize(serializer)
    }
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
/// [`BluerAdapterProbe`]. Honors `--watch`, `--json`, and the
/// daemon-socket probe.
///
/// # Errors
///
/// Returns [`StatusError`] on bond-store load failure or unexpected I/O.
/// Missing adapter / missing `last.log` are NOT errors. A
/// daemon-down probe is NOT an error (folds into a `daemon=down:
/// <reason>` line).
pub async fn run_status(opts: &StatusOpts) -> Result<(), StatusError> {
    if opts.watch && !opts.json {
        return run_status_watch_loop(opts).await;
    }
    let probe = BluerAdapterProbe;
    render_one_shot(opts, &probe).await
}

/// Render a single `status` snapshot. Used by the one-shot path and
/// by each `--watch` iteration.
async fn render_one_shot(opts: &StatusOpts, probe: &dyn AsyncAdapterProbe) -> Result<(), StatusError> {
    let cli_report = build_cli_report(opts);
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    if opts.json {
        write_json_report(&mut writer, &cli_report)?;
        return Ok(());
    }
    write_daemon_section(&mut writer, &cli_report)?;
    let snapshot = gather_status_async(opts, probe).await?;
    render_status_to(&mut writer, &snapshot)
}

/// Build the typed CLI status report (daemon section + per-peer
/// rows). Probes the daemon socket synchronously via the same
/// `read_frame_blocking` / `write_frame_blocking` helpers the PAM
/// module uses; the call is fenced by [`DAEMON_CONNECT_TIMEOUT`].
pub fn build_cli_report(opts: &StatusOpts) -> CliStatusReport {
    let socket = opts.socket.clone().unwrap_or_else(default_socket_path);
    let daemon = probe_daemon(&socket);
    CliStatusReport {
        daemon_socket: socket,
        daemon,
    }
}

/// Compose the default daemon socket path:
/// `${XDG_RUNTIME_DIR}/syauth/auth.sock`, with the
/// `/run/user/<uid>/` fallback when env is unset.
fn default_socket_path() -> PathBuf {
    let base = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(format!("/run/user/{}", nix::unistd::geteuid().as_raw())),
    };
    base.join(DEFAULT_SOCKET_BASENAME)
}

/// Probe the daemon by connecting to `socket` and exchanging one
/// `Request::Status` / `Response::Status` pair. Every failure path
/// collapses into a `DaemonProbeState::Down { reason }` so the
/// status command itself never errors out on daemon-down.
fn probe_daemon(socket: &Path) -> DaemonProbeState {
    if !socket.exists() {
        return DaemonProbeState::Down {
            reason: "socket-missing".to_owned(),
        };
    }
    let mut stream = match UnixStream::connect(socket) {
        Ok(s) => s,
        Err(err) => {
            return DaemonProbeState::Down {
                reason: connect_error_reason(&err),
            };
        }
    };
    if let Err(err) = stream.set_read_timeout(Some(DAEMON_STATUS_READ_TIMEOUT)) {
        return DaemonProbeState::Down {
            reason: format!("read-timeout-setup-failed: {}", err.kind()),
        };
    }
    if let Err(err) = stream.set_write_timeout(Some(DAEMON_CONNECT_TIMEOUT)) {
        return DaemonProbeState::Down {
            reason: format!("write-timeout-setup-failed: {}", err.kind()),
        };
    }
    if let Err(err) = write_frame_blocking(&mut stream, &Request::Status) {
        return DaemonProbeState::Down {
            reason: format!("frame-error: {err}"),
        };
    }
    match read_frame_blocking::<_, Response>(&mut stream) {
        Ok(Response::Status { peers, started_at }) => DaemonProbeState::Up { started_at, peers },
        Ok(_) => DaemonProbeState::Down {
            reason: "unexpected-response".to_owned(),
        },
        Err(err) => DaemonProbeState::Down {
            reason: format!("frame-error: {err}"),
        },
    }
}

/// Map a `std::io::Error` from the `connect()` call to the same
/// reason-token vocabulary the S-016 doctor uses.
fn connect_error_reason(err: &io::Error) -> String {
    match err.kind() {
        io::ErrorKind::NotFound => "socket-missing".to_owned(),
        io::ErrorKind::ConnectionRefused => "connect-refused".to_owned(),
        io::ErrorKind::PermissionDenied => "permission-denied".to_owned(),
        io::ErrorKind::TimedOut => "timeout".to_owned(),
        other => format!("connect-error: {other:?}"),
    }
}

/// Render the daemon section (header + per-peer table) of the
/// status output. Always emits at least the `daemon=` header line
/// so a daemon-down case is never a silent empty section.
pub fn write_daemon_section(writer: &mut dyn Write, report: &CliStatusReport) -> Result<(), StatusError> {
    match &report.daemon {
        DaemonProbeState::Up { started_at, peers } => {
            let ts = format_rfc3339(*started_at);
            writeln!(writer, "daemon=up started_at={ts}")?;
            writeln!(
                writer,
                "peer_id                                                              last_challenge  last_connect    uuid      in_flight"
            )?;
            for peer in peers {
                writeln!(
                    writer,
                    "{:<68}  {:<14}  {:<14}  {:<8}  {}",
                    peer.peer_id,
                    format_ms_ago(peer.last_challenge_ms_ago),
                    format_ms_ago(peer.last_connect_ms_ago),
                    short_uuid_hex(&peer.current_session_uuid),
                    peer.in_flight_challenges,
                )?;
            }
        }
        DaemonProbeState::Down { reason } => {
            writeln!(writer, "daemon=down: {reason}")?;
        }
    }
    Ok(())
}

/// JSON renderer for the daemon section. Used by `--json` mode.
pub fn write_json_report(writer: &mut dyn Write, report: &CliStatusReport) -> Result<(), StatusError> {
    let s = serde_json::to_string_pretty(report)?;
    writer.write_all(s.as_bytes())?;
    writer.write_all(b"\n")?;
    Ok(())
}

/// Render a `SystemTime` as an RFC3339 wall-clock token. Falls back
/// to `<epoch>+<seconds>` if the value pre-dates the unix epoch
/// (unreachable on a healthy clock).
fn format_rfc3339(t: SystemTime) -> String {
    let secs = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(_) => return "epoch+0".to_owned(),
    };
    let secs_i64 = i64::try_from(secs).unwrap_or(i64::MAX);
    match OffsetDateTime::from_unix_timestamp(secs_i64) {
        Ok(dt) => dt.format(&Rfc3339).unwrap_or_else(|_| format!("epoch+{secs}")),
        Err(_) => format!("epoch+{secs}"),
    }
}

/// Render an `Option<u64>` millisecond age as `<X.Ys ago>` or
/// `never`. Seconds resolution to one decimal so a sub-second age
/// surfaces as `0.3s ago`.
fn format_ms_ago(ms: Option<u64>) -> String {
    match ms {
        Some(m) => {
            let secs = m as f64 / 1000.0;
            format!("{secs:.1}s ago")
        }
        None => "never".to_owned(),
    }
}

/// Render the leading [`SHORT_UUID_HEX_LEN`] hex chars of `uuid`,
/// matching the orchestrator's `short_hex` rotation-audit format.
fn short_uuid_hex(uuid: &uuid::Uuid) -> String {
    let s = uuid.simple().to_string();
    s.chars().take(SHORT_UUID_HEX_LEN).collect()
}

/// `--watch` polling loop. Polls every `WATCH_INTERVAL`, redrawing
/// via `WATCH_CLEAR_SCREEN`. Exits cleanly on SIGINT (via an
/// `AtomicBool` toggled by a one-shot signal handler).
async fn run_status_watch_loop(opts: &StatusOpts) -> Result<(), StatusError> {
    let stop = Arc::new(AtomicBool::new(false));
    install_sigint_handler(Arc::clone(&stop));
    let probe = BluerAdapterProbe;
    while !stop.load(Ordering::SeqCst) {
        {
            let mut writer = io::stdout().lock();
            writer.write_all(WATCH_CLEAR_SCREEN.as_bytes())?;
        }
        render_one_shot(opts, &probe).await?;
        if wait_or_break(&stop, WATCH_INTERVAL) {
            break;
        }
    }
    Ok(())
}

/// Install a one-shot SIGINT handler that flips `stop` to `true`.
/// A no-op on a second invocation per process (the `ctrlc` crate
/// docs do not exist; we use a tokio signal stream instead).
fn install_sigint_handler(stop: Arc<AtomicBool>) {
    thread::spawn(move || {
        if let Ok(mut signals) = signal_hook::iterator::Signals::new([signal_hook::consts::SIGINT])
            && signals.forever().next().is_some()
        {
            stop.store(true, Ordering::SeqCst);
        }
    });
}

/// Sleep for `dur` (in `WATCH_SLEEP_TICK` slices) so a SIGINT
/// flipping `stop` interrupts the wait within one tick. Returns
/// `true` if `stop` flipped during the wait.
fn wait_or_break(stop: &AtomicBool, dur: Duration) -> bool {
    let deadline = std::time::Instant::now() + dur;
    while std::time::Instant::now() < deadline {
        if stop.load(Ordering::SeqCst) {
            return true;
        }
        thread::sleep(WATCH_SLEEP_TICK);
    }
    stop.load(Ordering::SeqCst)
}

/// Granularity of the `--watch` SIGINT poll. 50 ms means a SIGINT
/// flipping the `AtomicBool` interrupts a sleeping watch within one
/// tick, while keeping the wakeup rate well under any plausible
/// terminal redraw budget.
const WATCH_SLEEP_TICK: Duration = Duration::from_millis(50);

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
            socket: None,
            watch: false,
            json: false,
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
            socket: None,
            watch: false,
            json: false,
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
            socket: None,
            watch: false,
            json: false,
        };
        assert_eq!(effective_last_log_path(&opts), PathBuf::from("/tmp/elsewhere/last.log"));
    }

    #[test]
    fn adapter_state_as_token_is_stable() {
        assert_eq!(AdapterState::Powered.as_token(), "Powered");
        assert_eq!(AdapterState::Down.as_token(), "Down");
        assert_eq!(AdapterState::Missing.as_token(), "Missing");
    }

    #[test]
    fn watch_interval_is_one_second() {
        // S-017 prompt: "polls every 1 s and redraws". The named
        // constant is the canonical source of truth.
        assert_eq!(WATCH_INTERVAL, Duration::from_secs(1));
    }

    #[test]
    fn format_ms_ago_renders_never_for_none() {
        assert_eq!(format_ms_ago(None), "never");
    }

    #[test]
    fn format_ms_ago_renders_one_decimal_seconds() {
        assert_eq!(format_ms_ago(Some(3_200)), "3.2s ago");
        assert_eq!(format_ms_ago(Some(0)), "0.0s ago");
    }

    #[test]
    fn short_uuid_hex_truncates_to_eight_chars() {
        let uuid = uuid::Uuid::from_bytes([0xab; 16]);
        let s = short_uuid_hex(&uuid);
        assert_eq!(s.len(), SHORT_UUID_HEX_LEN);
        assert_eq!(s, "abababab");
    }

    #[test]
    fn write_daemon_section_emits_down_token_on_daemon_down() {
        let report = CliStatusReport {
            daemon_socket: PathBuf::from("/tmp/x.sock"),
            daemon: DaemonProbeState::Down {
                reason: "socket-missing".to_owned(),
            },
        };
        let mut buf: Vec<u8> = Vec::new();
        let mut cur = Cursor::new(&mut buf);
        write_daemon_section(&mut cur, &report).expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("daemon=down: socket-missing"));
    }

    #[test]
    fn connect_error_reason_maps_not_found_to_socket_missing() {
        let err = io::Error::new(io::ErrorKind::NotFound, "x");
        assert_eq!(connect_error_reason(&err), "socket-missing");
    }
}
