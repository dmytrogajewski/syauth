//! `syauth-cli` — `doctor` subcommand.
//!
//! Operator-facing read-only diagnostic. Emits one greppable
//! `key=value` line per probe, plus a final `doctor=ok|warn|fail`
//! summary line, so `sy syauth doctor | grep daemon=` lights up
//! dashboards without parsing prose. `--json` emits the same data as
//! a typed JSON object via [`serde_json::to_string_pretty`] for
//! tooling consumers.
//!
//! Roadmap: `specs/unlock-proximity/ROADMAP.md` Step S-016.
//! Journey: `specs/journeys/JOURNEY-S-016-syauth-doctor.md`.
//!
//! The probe sequence (one `key=value` line each):
//!
//! 1. `daemon_socket = <path>` — resolved Unix-socket path.
//! 2. `daemon = up | down: <reason>` — `connect()` + `Request::Status`
//!    with a [`DAEMON_CONNECT_TIMEOUT`] budget.
//! 3. `bonds_file = <path>` and `bonds_count = N`.
//! 4. `keys_dir = <path>`, `keys_files = N`, and one
//!    `keys_<peer_id>_mode = <octal> [(expected 0600)]` per file.
//! 5. `bluez_adapter = powered | unpowered | absent | unknown`.
//! 6. `systemctl_user_unit = active | inactive | unknown`.
//! 7. `last_log_tail_<i>` for i in 1..=[`DOCTOR_LAST_LOG_TAIL`].
//! 8. `xdg_runtime_dir = <env-or-fallback>` — flags the SSH-session
//!    caveat from SPEC §8.
//! 9. `doctor = ok | warn | fail`.
//!
//! The doctor never writes to the host; every probe is best-effort
//! and folds known-CI-unfriendly failures (`systemctl` missing, BlueZ
//! absent) into `unknown` so the doctor itself never breaks because
//! the host lacks a probe target.

use std::{
    fs,
    io::{self, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

use clap::Parser;
use serde::Serialize;
use syauth_core::BondStore;
use syauth_presenced::{Request, Response, read_frame_blocking, write_frame_blocking};
use thiserror::Error;

// =============================================================================
// Named constants — every literal that touches the operator surface or the
// SPEC default-paths is named here so a single grep finds the canonical
// definition.
// =============================================================================

/// Default `--bonds-file`. SPEC §3 Approach + SPEC §7 Audit anchor.
pub const DEFAULT_BONDS_FILE: &str = "/var/lib/syauth/bonds.toml";

/// Default `--keys-dir`. SPEC §7 Data Classification anchor (
/// "`/var/lib/syauth/keys/<peer_id>.bin` (0600 root-owned)").
pub const DEFAULT_KEYS_DIR: &str = "/var/lib/syauth/keys";

/// Default `--audit-log`. SPEC §7 Audit anchor.
pub const DEFAULT_AUDIT_LOG_FILE: &str = "/var/lib/syauth/last.log";

/// Expected mode (octal) on every `<peer_id>.bin` keys file. SPEC §7
/// Data Classification: "0600 root-owned". The doctor surfaces a
/// warning whenever the effective mode (symlinks followed) deviates.
pub const EXPECTED_KEYS_FILE_MODE: u32 = 0o600;

/// Tail length applied to the audit log. Ten lines is the SPEC §3
/// scope item #24 contract ("last 10 lines of `/var/lib/syauth/last.log`").
pub const DOCTOR_LAST_LOG_TAIL: usize = 10;

/// Hard ceiling on the number of lines the audit-log probe will read
/// before truncating, even if the on-disk file is unexpectedly
/// unbounded. The `status` subcommand uses the same defensive cap
/// (`status::LAST_UNLOCK_LOG_MAX_LINES = 64`); the doctor's cap is
/// looser to surface up to 10 lines without a `Lines::rev` requirement.
const DOCTOR_LOG_MAX_READ_LINES: usize = 4_096;

/// Hard cap on the number of `<peer_id>.bin` files the doctor will
/// inspect. Protects against accidental world-readable dumps under
/// `keys_dir` blowing up the output.
const DOCTOR_KEYS_FILE_CAP: usize = 1_024;

/// `connect()` + Status RPC timeout budget. Mirrors SPEC §4.3
/// "daemon-down latency ≤ 50 ms"; the constant is re-stated here
/// (not imported from `syauth-pam`) because the `syauth-cli` crate
/// must not depend on the PAM cdylib. The value is in sync with
/// `pam_syauth::auth::DAEMON_CONNECT_TIMEOUT` by SPEC anchor.
pub const DAEMON_CONNECT_TIMEOUT: Duration = Duration::from_millis(50);

/// Read budget on the daemon's `Response::Status` reply. The daemon
/// answers Status from in-memory state (no BLE round-trip), so a
/// short timeout is fine; we use 200 ms to absorb scheduler jitter
/// on a loaded CI runner.
const DAEMON_STATUS_READ_TIMEOUT: Duration = Duration::from_millis(200);

/// File extension applied to per-peer bond-key files. Matches
/// `syauth_presenced::BOND_KEY_FILE_EXT`.
const BOND_KEY_FILE_EXT: &str = ".bin";

/// Token printed when `XDG_RUNTIME_DIR` is unset and the doctor falls
/// back to `/run/user/<uid>`. The label is its own probe (separate
/// from `daemon=down`) so dashboards can alert on the SPEC §8
/// SSH-session caveat in isolation.
const XDG_RUNTIME_DIR_UNSET_PREFIX: &str = "unset (fallback ";

/// Token printed when `XDG_RUNTIME_DIR` is set.
const XDG_RUNTIME_DIR_SET_PREFIX: &str = "set ";

/// Summary token: every probe green.
const SUMMARY_OK: &str = "ok";

/// Summary token: at least one probe surfaced a non-fatal warning
/// (e.g. keys mode != 0600, XDG fallback used).
const SUMMARY_WARN: &str = "warn";

/// Summary token: a load-bearing probe failed (`daemon=down`).
const SUMMARY_FAIL: &str = "fail";

/// `systemctl --user is-active` argv. Pulled into a constant so the
/// integration test reads identical to the production wiring.
const SYSTEMCTL_BIN: &str = "systemctl";
const SYSTEMCTL_USER_FLAG: &str = "--user";
const SYSTEMCTL_IS_ACTIVE: &str = "is-active";
const SYSTEMCTL_UNIT_NAME: &str = "syauth-presenced.service";

// =============================================================================
// Clap options.
// =============================================================================

/// CLI options for the `doctor` subcommand.
#[derive(Debug, Parser, Clone)]
pub struct DoctorOpts {
    /// Override the resolved Unix-socket path. If unset, defaults to
    /// `${XDG_RUNTIME_DIR}/syauth/auth.sock` (or
    /// `/run/user/<uid>/syauth/auth.sock` when the env is missing,
    /// per SPEC §8 SSH-session caveat).
    #[arg(long)]
    pub socket: Option<PathBuf>,

    /// Bonds file path. Defaults to SPEC's `/var/lib/syauth/bonds.toml`.
    #[arg(long, default_value = DEFAULT_BONDS_FILE)]
    pub bonds_file: PathBuf,

    /// Keys directory. Defaults to SPEC's `/var/lib/syauth/keys`.
    #[arg(long, default_value = DEFAULT_KEYS_DIR)]
    pub keys_dir: PathBuf,

    /// Audit log file. Defaults to SPEC's `/var/lib/syauth/last.log`.
    #[arg(long, default_value = DEFAULT_AUDIT_LOG_FILE)]
    pub audit_log: PathBuf,

    /// Emit the probe data as a typed JSON object via
    /// `serde_json::to_string_pretty`. Same schema as the greppable
    /// output, different surface for tooling.
    #[arg(long)]
    pub json: bool,

    /// Skip the BlueZ DBus probe. CI hosts without bluer-on-DBus
    /// would otherwise see `unknown`; the flag makes the skip
    /// explicit and keeps test output deterministic.
    #[arg(long)]
    pub skip_bluez: bool,

    /// Skip the `systemctl --user is-active` shell-out. Same
    /// motivation as `--skip-bluez`.
    #[arg(long)]
    pub skip_systemctl: bool,

    /// Skip the daemon socket probe entirely (record `unknown`).
    /// Useful for keys-only audit runs from CI.
    #[arg(long)]
    pub skip_daemon: bool,
}

// =============================================================================
// Typed error surface.
// =============================================================================

/// Errors [`run_doctor`] can surface. The doctor itself never panics
/// on a probe failure (probes fold their failures into a label); the
/// only `Err` paths are stdout I/O and JSON encoding.
#[derive(Debug, Error)]
pub enum DoctorError {
    /// Stdout / stderr I/O failure.
    #[error("doctor i/o error: {0}")]
    Io(#[from] io::Error),

    /// `serde_json` encode failure on the `--json` path. The schema
    /// is total over its variants, so this is effectively unreachable
    /// — kept as a typed variant for symmetry.
    #[error("doctor json encode error: {0}")]
    Json(#[from] serde_json::Error),
}

// =============================================================================
// Typed report shape (drives both the key=value renderer and the JSON
// renderer). Each field maps 1:1 to one probe line.
// =============================================================================

/// Daemon-socket reachability outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum DaemonState {
    /// `connect()` + `Request::Status` succeeded within the timeout.
    Up,
    /// Probe failed. `reason` is a short greppable token
    /// (`socket-missing`, `connect-refused`, `timeout`, `frame-error`,
    /// `skipped`).
    Down {
        /// One-word reason token; never includes whitespace.
        reason: String,
    },
}

/// Bonds-file probe outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BondsReport {
    /// Resolved bonds-file path.
    pub path: PathBuf,
    /// Whether the file exists at `path`.
    pub exists: bool,
    /// Number of bonds parsed; `0` when the file is absent.
    pub count: usize,
    /// Whether the file parsed cleanly (always `true` when `exists`
    /// is `false`).
    pub parseable: bool,
}

/// Per-key file probe row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyFileReport {
    /// Peer id (file stem with the `.bin` suffix stripped).
    pub peer_id: String,
    /// Effective mode rendered as a four-character octal string
    /// (e.g. `"0600"`, `"0644"`). Symlinks are followed.
    pub mode: String,
    /// `true` iff `mode == "0600"`.
    pub ok: bool,
}

/// Keys-directory probe outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeysReport {
    /// Resolved keys directory path.
    pub dir: PathBuf,
    /// Per-key file rows. Sorted by `peer_id` for deterministic
    /// output.
    pub files: Vec<KeyFileReport>,
}

/// `XDG_RUNTIME_DIR` env probe outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct XdgRuntimeDirReport {
    /// `true` iff `XDG_RUNTIME_DIR` is set in the doctor's
    /// environment.
    pub set: bool,
    /// The resolved value (env-or-fallback).
    pub value: PathBuf,
}

/// Full doctor report — the JSON shape, and the same data the
/// greppable renderer formats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    /// Resolved daemon socket path.
    pub daemon_socket: PathBuf,
    /// Daemon-reachability outcome.
    pub daemon: DaemonState,
    /// Bonds-file probe.
    pub bonds_file: BondsReport,
    /// Keys-directory probe.
    pub keys: KeysReport,
    /// BlueZ adapter probe: `"powered" | "unpowered" | "absent" | "unknown"`.
    pub bluez_adapter: String,
    /// `systemctl --user` probe: `"active" | "inactive" | "unknown"`.
    pub systemctl: String,
    /// Audit-log tail, most-recent-last, capped at
    /// [`DOCTOR_LAST_LOG_TAIL`] lines.
    pub last_log_tail: Vec<String>,
    /// XDG_RUNTIME_DIR env probe.
    pub xdg_runtime_dir: XdgRuntimeDirReport,
    /// Summary token: `"ok" | "warn" | "fail"`.
    pub summary: String,
}

// =============================================================================
// Public entry point.
// =============================================================================

/// Drive the doctor against `opts` and emit either the key=value
/// renderer (default) or the JSON renderer (`--json`).
///
/// # Errors
///
/// Returns [`DoctorError::Io`] on stdout I/O failure;
/// [`DoctorError::Json`] on the `--json` encode path (effectively
/// unreachable, schema is total).
pub fn run_doctor(opts: &DoctorOpts) -> Result<(), DoctorError> {
    let report = build_report(opts);
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    if opts.json {
        write_json(&mut writer, &report)?;
    } else {
        write_keyvalue(&mut writer, &report)?;
    }
    Ok(())
}

// =============================================================================
// Report construction.
// =============================================================================

/// Build the full [`DoctorReport`] by running every probe in order.
/// Pure with respect to the host except for the probes themselves —
/// no probe writes anywhere.
pub fn build_report(opts: &DoctorOpts) -> DoctorReport {
    let xdg = probe_xdg_runtime_dir();
    let daemon_socket = opts.socket.clone().unwrap_or_else(|| default_socket_path(&xdg.value));
    let daemon = if opts.skip_daemon {
        DaemonState::Down {
            reason: "skipped".to_owned(),
        }
    } else {
        probe_daemon(&daemon_socket)
    };
    let bonds_file = probe_bonds(&opts.bonds_file);
    let keys = probe_keys(&opts.keys_dir);
    let bluez_adapter = if opts.skip_bluez {
        "unknown".to_owned()
    } else {
        probe_bluez_adapter()
    };
    let systemctl = if opts.skip_systemctl {
        "unknown".to_owned()
    } else {
        probe_systemctl()
    };
    let last_log_tail = probe_audit_log_tail(&opts.audit_log);
    let summary = compute_summary(&daemon, &bonds_file, &keys, &xdg);
    DoctorReport {
        daemon_socket,
        daemon,
        bonds_file,
        keys,
        bluez_adapter,
        systemctl,
        last_log_tail,
        xdg_runtime_dir: xdg,
        summary,
    }
}

// =============================================================================
// Probe: XDG_RUNTIME_DIR. Runs first because the daemon-socket default
// is derived from it.
// =============================================================================

/// Read `XDG_RUNTIME_DIR` from the environment; fall back to
/// `/run/user/<uid>` when unset. The doctor never `stat`s the path
/// (we report what the daemon would have used, not whether it
/// exists).
fn probe_xdg_runtime_dir() -> XdgRuntimeDirReport {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(v) if !v.is_empty() => XdgRuntimeDirReport {
            set: true,
            value: PathBuf::from(v),
        },
        _ => {
            let uid = nix_geteuid();
            XdgRuntimeDirReport {
                set: false,
                value: PathBuf::from(format!("/run/user/{uid}")),
            }
        }
    }
}

/// Wrapper around the libc-level `geteuid()` that returns the result
/// as a `u32` (the `nix::unistd::Uid` type drops to `u32` cleanly).
fn nix_geteuid() -> u32 {
    nix::unistd::geteuid().as_raw()
}

/// Compose the default socket path: `<xdg>/syauth/auth.sock`. Matches
/// SPEC §3 + `syauth_presenced::DEFAULT_SOCKET_BASENAME`.
fn default_socket_path(xdg_runtime_dir: &Path) -> PathBuf {
    xdg_runtime_dir.join("syauth").join("auth.sock")
}

// =============================================================================
// Probe: daemon socket. `connect()` + `Request::Status` round-trip with
// a hard timeout.
// =============================================================================

/// Probe the daemon by connecting to `socket` and exchanging one
/// `Request::Status` / `Response::Status` pair. Every failure path
/// collapses into a `DaemonState::Down { reason }` so the doctor
/// itself never returns an error.
fn probe_daemon(socket: &Path) -> DaemonState {
    if !socket.exists() {
        return DaemonState::Down {
            reason: "socket-missing".to_owned(),
        };
    }
    let mut stream = match UnixStream::connect(socket) {
        Ok(s) => s,
        Err(err) => {
            return DaemonState::Down {
                reason: connect_error_reason(&err),
            };
        }
    };
    if let Err(err) = stream.set_read_timeout(Some(DAEMON_STATUS_READ_TIMEOUT)) {
        return DaemonState::Down {
            reason: format!("read-timeout-setup-failed: {}", err.kind()),
        };
    }
    if let Err(err) = stream.set_write_timeout(Some(DAEMON_CONNECT_TIMEOUT)) {
        return DaemonState::Down {
            reason: format!("write-timeout-setup-failed: {}", err.kind()),
        };
    }
    if let Err(err) = write_frame_blocking(&mut stream, &Request::Status) {
        return DaemonState::Down {
            reason: format!("frame-error: {err}"),
        };
    }
    match read_frame_blocking::<_, Response>(&mut stream) {
        Ok(Response::Status { .. }) => DaemonState::Up,
        Ok(other) => DaemonState::Down {
            reason: format!("unexpected-response: {}", response_kind(&other)),
        },
        Err(err) => DaemonState::Down {
            reason: format!("frame-error: {err}"),
        },
    }
}

/// Map a `std::io::Error` from the `connect()` call to a short
/// greppable reason token.
fn connect_error_reason(err: &io::Error) -> String {
    match err.kind() {
        io::ErrorKind::NotFound => "socket-missing".to_owned(),
        io::ErrorKind::ConnectionRefused => "connect-refused".to_owned(),
        io::ErrorKind::PermissionDenied => "permission-denied".to_owned(),
        io::ErrorKind::TimedOut => "timeout".to_owned(),
        other => format!("connect-error: {other:?}"),
    }
}

/// One-word render of a `Response` variant, used in the
/// `unexpected-response` reason token.
fn response_kind(resp: &Response) -> &'static str {
    match resp {
        Response::Challenge { .. } => "challenge",
        Response::Reload { .. } => "reload",
        Response::Status { .. } => "status",
    }
}

// =============================================================================
// Probe: bonds file.
// =============================================================================

/// Read the bonds file (if present) and count its entries. A missing
/// file is NOT an error — the doctor reports `exists=false, count=0`.
fn probe_bonds(path: &Path) -> BondsReport {
    if !path.exists() {
        return BondsReport {
            path: path.to_path_buf(),
            exists: false,
            count: 0,
            parseable: true,
        };
    }
    match BondStore::load(path) {
        Ok(store) => BondsReport {
            path: path.to_path_buf(),
            exists: true,
            count: store.list().len(),
            parseable: true,
        },
        Err(_) => BondsReport {
            path: path.to_path_buf(),
            exists: true,
            count: 0,
            parseable: false,
        },
    }
}

// =============================================================================
// Probe: keys directory. Per-file mode check; never reads bytes.
// =============================================================================

/// Walk `dir`, collect every `<peer_id>.bin`, and stat its effective
/// mode (symlinks followed). Files are sorted by peer_id so the
/// output is deterministic.
fn probe_keys(dir: &Path) -> KeysReport {
    let mut files: Vec<KeyFileReport> = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => {
            return KeysReport {
                dir: dir.to_path_buf(),
                files,
            };
        }
    };
    for entry in entries.flatten().take(DOCTOR_KEYS_FILE_CAP) {
        let p = entry.path();
        let Some(peer_id) = key_file_peer_id(&p) else {
            continue;
        };
        let mode = effective_file_mode(&p);
        let mode_str = format!("{mode:04o}");
        let ok = mode == EXPECTED_KEYS_FILE_MODE;
        files.push(KeyFileReport {
            peer_id,
            mode: mode_str,
            ok,
        });
    }
    files.sort_by(|a, b| a.peer_id.cmp(&b.peer_id));
    KeysReport {
        dir: dir.to_path_buf(),
        files,
    }
}

/// Extract the peer id from a `<peer_id>.bin` file name, or return
/// `None` if the entry does not match the layout.
fn key_file_peer_id(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(BOND_KEY_FILE_EXT)?;
    if stem.is_empty() {
        return None;
    }
    Some(stem.to_owned())
}

/// Return the effective file mode (symlinks followed) as a `u32`,
/// masked to the low 12 bits so the formatter renders four octal
/// digits.
fn effective_file_mode(path: &Path) -> u32 {
    match fs::metadata(path) {
        Ok(meta) => meta.permissions().mode() & 0o7777,
        Err(_) => 0,
    }
}

// =============================================================================
// Probe: BlueZ adapter. Best-effort, never blocks the doctor.
// =============================================================================

/// Best-effort BlueZ probe. The `syauth-cli` crate already pulls
/// `bluer`, but we do not spin up a tokio runtime here — that would
/// be heavy and the doctor's contract is "fast, best-effort, never
/// block on DBus". Until S-017 wires a proper async probe, the
/// production path returns `"unknown"`. Anchored in SPEC §3 scope
/// item #24 (per-peer metrics arrive in S-017).
fn probe_bluez_adapter() -> String {
    "unknown".to_owned()
}

// =============================================================================
// Probe: systemctl --user is-active. Best-effort.
// =============================================================================

/// Shell out to `systemctl --user is-active syauth-presenced.service`,
/// suppress stderr, and parse the trimmed stdout. Any failure
/// (binary missing, runtime error) folds into `"unknown"`.
fn probe_systemctl() -> String {
    let out = Command::new(SYSTEMCTL_BIN)
        .args([SYSTEMCTL_USER_FLAG, SYSTEMCTL_IS_ACTIVE, SYSTEMCTL_UNIT_NAME])
        .stderr(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            match s.as_str() {
                "active" => "active".to_owned(),
                "inactive" | "failed" | "unknown" => "inactive".to_owned(),
                _ => "unknown".to_owned(),
            }
        }
        Err(_) => "unknown".to_owned(),
    }
}

// =============================================================================
// Probe: audit log tail.
// =============================================================================

/// Read the last [`DOCTOR_LAST_LOG_TAIL`] non-empty lines of `path`
/// (most-recent-last). A missing or unreadable file collapses to an
/// empty `Vec`.
fn probe_audit_log_tail(path: &Path) -> Vec<String> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let mut tail: std::collections::VecDeque<String> = std::collections::VecDeque::with_capacity(DOCTOR_LAST_LOG_TAIL);
    let reader = io::BufReader::new(file);
    let mut read = 0usize;
    for line in io::BufRead::lines(reader) {
        if read >= DOCTOR_LOG_MAX_READ_LINES {
            break;
        }
        read = read.saturating_add(1);
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        if tail.len() == DOCTOR_LAST_LOG_TAIL {
            tail.pop_front();
        }
        tail.push_back(line);
    }
    tail.into_iter().collect()
}

// =============================================================================
// Summary.
// =============================================================================

/// Compute the final `doctor=ok|warn|fail` token from the probe
/// outcomes. `daemon=down` is `fail`; any other deviation is `warn`.
fn compute_summary(daemon: &DaemonState, bonds: &BondsReport, keys: &KeysReport, xdg: &XdgRuntimeDirReport) -> String {
    if let DaemonState::Down { reason } = daemon {
        if reason != "skipped" {
            return SUMMARY_FAIL.to_owned();
        }
    }
    let mut warn = false;
    if bonds.exists && !bonds.parseable {
        warn = true;
    }
    if keys.files.iter().any(|f| !f.ok) {
        warn = true;
    }
    if !xdg.set {
        warn = true;
    }
    if warn { SUMMARY_WARN.to_owned() } else { SUMMARY_OK.to_owned() }
}

// =============================================================================
// Renderers.
// =============================================================================

/// Greppable `key=value` renderer. One line per probe; the final
/// line is `doctor=<summary>`.
pub fn write_keyvalue(writer: &mut dyn Write, report: &DoctorReport) -> Result<(), DoctorError> {
    writeln!(writer, "daemon_socket={}", report.daemon_socket.display())?;
    match &report.daemon {
        DaemonState::Up => writeln!(writer, "daemon=up")?,
        DaemonState::Down { reason } => writeln!(writer, "daemon=down: {reason}")?,
    }
    writeln!(writer, "bonds_file={}", report.bonds_file.path.display())?;
    writeln!(writer, "bonds_count={}", report.bonds_file.count)?;
    writeln!(writer, "bonds_parseable={}", report.bonds_file.parseable)?;
    writeln!(writer, "keys_dir={}", report.keys.dir.display())?;
    writeln!(writer, "keys_files={}", report.keys.files.len())?;
    for file in &report.keys.files {
        if file.ok {
            writeln!(writer, "keys_{}_mode={}", file.peer_id, file.mode)?;
        } else {
            writeln!(writer, "keys_{}_mode={} (expected 0600)", file.peer_id, file.mode)?;
        }
    }
    writeln!(writer, "bluez_adapter={}", report.bluez_adapter)?;
    writeln!(writer, "systemctl_user_unit={}", report.systemctl)?;
    for (idx, line) in report.last_log_tail.iter().enumerate() {
        let i = idx.saturating_add(1);
        writeln!(writer, "last_log_tail_{i}={line}")?;
    }
    write_xdg_runtime_dir(writer, &report.xdg_runtime_dir)?;
    writeln!(writer, "doctor={}", report.summary)?;
    Ok(())
}

/// Render the `xdg_runtime_dir` line, distinguishing set / unset
/// (fallback) explicitly per the SPEC §8 SSH-session caveat
/// breadcrumb.
fn write_xdg_runtime_dir(writer: &mut dyn Write, xdg: &XdgRuntimeDirReport) -> Result<(), DoctorError> {
    if xdg.set {
        writeln!(writer, "xdg_runtime_dir={}{}", XDG_RUNTIME_DIR_SET_PREFIX, xdg.value.display())?;
    } else {
        writeln!(writer, "xdg_runtime_dir={}{})", XDG_RUNTIME_DIR_UNSET_PREFIX, xdg.value.display())?;
    }
    Ok(())
}

/// JSON renderer. Emits `serde_json::to_string_pretty(report)`
/// followed by a single newline.
pub fn write_json(writer: &mut dyn Write, report: &DoctorReport) -> Result<(), DoctorError> {
    let s = serde_json::to_string_pretty(report)?;
    writer.write_all(s.as_bytes())?;
    writer.write_all(b"\n")?;
    Ok(())
}

// =============================================================================
// SystemTime helper — kept here so the doctor binary does not pull a
// transitive dep on `time` for this one render. Unused today; reserved
// for the S-017 expansion that prints the daemon's `started_at`.
// =============================================================================

#[allow(dead_code)]
fn approx_unix_seconds(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// =============================================================================
// Tests — library-level. Integration tests live in tests/doctor_flow.rs.
// =============================================================================

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use tempfile::TempDir;

    use super::*;

    const TEST_PEER: &str = "deadbeefcafebabe1234567890abcdef";

    #[test]
    fn key_file_peer_id_strips_bin_suffix() {
        let got = key_file_peer_id(Path::new("/keys/abcd.bin"));
        assert_eq!(got, Some("abcd".to_owned()));
    }

    #[test]
    fn key_file_peer_id_rejects_non_bin() {
        assert_eq!(key_file_peer_id(Path::new("/keys/abcd.txt")), None);
        assert_eq!(key_file_peer_id(Path::new("/keys/.bin")), None);
    }

    #[test]
    fn probe_keys_flags_0644_file_as_not_ok() {
        let td = TempDir::new().expect("tempdir");
        let dir = td.path().to_path_buf();
        let p = dir.join(format!("{TEST_PEER}.bin"));
        fs::write(&p, [0u8; 4]).expect("write");
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).expect("chmod");
        let got = probe_keys(&dir);
        assert_eq!(got.files.len(), 1);
        assert_eq!(got.files[0].peer_id, TEST_PEER);
        assert_eq!(got.files[0].mode, "0644");
        assert!(!got.files[0].ok);
    }

    #[test]
    fn probe_keys_sorts_files_by_peer_id() {
        let td = TempDir::new().expect("tempdir");
        let dir = td.path().to_path_buf();
        for stem in ["zzzz", "aaaa", "mmmm"] {
            let p = dir.join(format!("{stem}.bin"));
            fs::write(&p, [0u8; 4]).expect("write");
            fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).expect("chmod");
        }
        let got = probe_keys(&dir);
        let ids: Vec<_> = got.files.iter().map(|f| f.peer_id.clone()).collect();
        assert_eq!(ids, vec!["aaaa".to_owned(), "mmmm".to_owned(), "zzzz".to_owned()]);
    }

    #[test]
    fn default_socket_path_appends_syauth_auth_sock() {
        let got = default_socket_path(Path::new("/tmp/runtime"));
        assert_eq!(got, PathBuf::from("/tmp/runtime/syauth/auth.sock"));
    }

    #[test]
    fn compute_summary_is_fail_when_daemon_down() {
        let daemon = DaemonState::Down {
            reason: "socket-missing".to_owned(),
        };
        let bonds = BondsReport {
            path: PathBuf::from("/tmp/x"),
            exists: false,
            count: 0,
            parseable: true,
        };
        let keys = KeysReport {
            dir: PathBuf::from("/tmp/y"),
            files: vec![],
        };
        let xdg = XdgRuntimeDirReport {
            set: true,
            value: PathBuf::from("/run/user/1000"),
        };
        assert_eq!(compute_summary(&daemon, &bonds, &keys, &xdg), SUMMARY_FAIL);
    }

    #[test]
    fn compute_summary_is_warn_when_keys_mode_bad() {
        let daemon = DaemonState::Up;
        let bonds = BondsReport {
            path: PathBuf::from("/tmp/x"),
            exists: true,
            count: 1,
            parseable: true,
        };
        let keys = KeysReport {
            dir: PathBuf::from("/tmp/y"),
            files: vec![KeyFileReport {
                peer_id: TEST_PEER.to_owned(),
                mode: "0644".to_owned(),
                ok: false,
            }],
        };
        let xdg = XdgRuntimeDirReport {
            set: true,
            value: PathBuf::from("/run/user/1000"),
        };
        assert_eq!(compute_summary(&daemon, &bonds, &keys, &xdg), SUMMARY_WARN);
    }

    #[test]
    fn compute_summary_is_ok_when_all_green() {
        let daemon = DaemonState::Up;
        let bonds = BondsReport {
            path: PathBuf::from("/tmp/x"),
            exists: true,
            count: 1,
            parseable: true,
        };
        let keys = KeysReport {
            dir: PathBuf::from("/tmp/y"),
            files: vec![KeyFileReport {
                peer_id: TEST_PEER.to_owned(),
                mode: "0600".to_owned(),
                ok: true,
            }],
        };
        let xdg = XdgRuntimeDirReport {
            set: true,
            value: PathBuf::from("/run/user/1000"),
        };
        assert_eq!(compute_summary(&daemon, &bonds, &keys, &xdg), SUMMARY_OK);
    }

    #[test]
    fn probe_audit_log_tail_caps_at_ten() {
        let td = TempDir::new().expect("tempdir");
        let path = td.path().join("last.log");
        let mut body = String::new();
        for i in 1..=25 {
            body.push_str(&format!("line-{i}\n"));
        }
        fs::write(&path, body).expect("write");
        let got = probe_audit_log_tail(&path);
        assert_eq!(got.len(), DOCTOR_LAST_LOG_TAIL);
        assert_eq!(got.first().map(String::as_str), Some("line-16"));
        assert_eq!(got.last().map(String::as_str), Some("line-25"));
    }

    #[test]
    fn write_keyvalue_emits_summary_token() {
        let report = DoctorReport {
            daemon_socket: PathBuf::from("/tmp/x.sock"),
            daemon: DaemonState::Down {
                reason: "socket-missing".to_owned(),
            },
            bonds_file: BondsReport {
                path: PathBuf::from("/tmp/bonds.toml"),
                exists: false,
                count: 0,
                parseable: true,
            },
            keys: KeysReport {
                dir: PathBuf::from("/tmp/keys"),
                files: vec![],
            },
            bluez_adapter: "unknown".to_owned(),
            systemctl: "unknown".to_owned(),
            last_log_tail: vec![],
            xdg_runtime_dir: XdgRuntimeDirReport {
                set: true,
                value: PathBuf::from("/run/user/1000"),
            },
            summary: SUMMARY_FAIL.to_owned(),
        };
        let mut buf = Vec::new();
        let mut cur = Cursor::new(&mut buf);
        write_keyvalue(&mut cur, &report).expect("write");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("daemon=down: socket-missing"));
        assert!(s.ends_with("doctor=fail\n"));
    }
}
