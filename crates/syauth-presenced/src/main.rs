//! `syauth-presenced` — long-running desktop user-service that owns
//! the BlueZ adapter for the unlock channel.
//!
//! Roadmap: `specs/unlock-proximity/ROADMAP.md` Step S-001.
//! Journey: `specs/journeys/JOURNEY-S-001-scaffold-syauth-presenced.md`.
//! SPEC anchor: `specs/unlock-proximity/SPEC.md` §3 Approach + §4
//! Architecture + §6 Rehydration step 1 (pidfile lock).
//!
//! S-001 only ships the process skeleton: CLI parsing, syslog-tagged
//! tracing, single-instance pidfile lock at
//! `${XDG_RUNTIME_DIR}/syauth/presenced.pid`, an empty tokio loop, and
//! a SIGINT/SIGTERM clean-shutdown path. Later S-NNN rows layer the
//! Unix-socket RPC, BLE peripheral, rotation, multi-peer, challenge
//! flow, etc. on top of the same `runtime::run` entrypoint.

use std::{
    io::{self, Write as _},
    path::PathBuf,
    process::ExitCode,
};

use anyhow::{Context as _, Result};
use clap::Parser;
use syauth_presenced::{
    Config, DEFAULT_AUDIT_LOG_PATH, DEFAULT_BONDS_FILE, DEFAULT_KEYS_DIR, DEFAULT_SOCKET_BASENAME, InjectedResponse, PIDFILE_BASENAME,
    PeripheralMode, RUNTIME_SUBDIR, run,
};
use tracing_subscriber::EnvFilter;

/// Syslog tag for every line emitted by this binary, per
/// `specs/unlock-proximity/SPEC.md` §3 Approach.
const SYSLOG_TAG: &str = "syauth-presenced";

/// Environment variable holding the per-user runtime directory.
/// Falls back to `/run/user/$UID` if unset, per the SPEC §8 Risks
/// row on "XDG_RUNTIME_DIR not set, e.g. from a serial console".
const XDG_RUNTIME_DIR_ENV: &str = "XDG_RUNTIME_DIR";

/// Default log level when `--log-level` is not specified.
const DEFAULT_LOG_LEVEL: &str = "info";

#[derive(Debug, Parser)]
#[command(
    name = "syauth-presenced",
    version,
    about = "Long-running desktop user-service for the syauth unlock channel",
    long_about = "Loads bonded peers, owns the BlueZ adapter for the unlock channel, \
                  and serves PAM challenge transactions over a Unix socket. \
                  See specs/unlock-proximity/SPEC.md for the design contract."
)]
struct Cli {
    /// Unix socket path the daemon will bind for PAM challenge RPC.
    /// Default: `${XDG_RUNTIME_DIR}/syauth/auth.sock`.
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,
    /// TOML bonds file path. Default: `/var/lib/syauth/bonds.toml`.
    #[arg(long, value_name = "PATH", default_value = DEFAULT_BONDS_FILE)]
    bonds_file: PathBuf,
    /// Per-peer keys directory. Default: `/var/lib/syauth/keys/`.
    #[arg(long, value_name = "PATH", default_value = DEFAULT_KEYS_DIR)]
    keys_dir: PathBuf,
    /// Append-only audit log path (SPEC §3 scope item #8).
    /// Default: `/var/lib/syauth/last.log`.
    #[arg(long, value_name = "PATH", default_value = DEFAULT_AUDIT_LOG_PATH)]
    audit_log: PathBuf,
    /// Tracing-subscriber log level (e.g. `info`, `debug`,
    /// `syauth_presenced=debug,info`).
    #[arg(long, value_name = "FILTER", default_value = DEFAULT_LOG_LEVEL)]
    log_level: String,
    /// Pidfile path override (test-only). Defaults to
    /// `${XDG_RUNTIME_DIR}/syauth/presenced.pid`.
    #[arg(long, value_name = "PATH", hide = true)]
    pidfile: Option<PathBuf>,
    /// Peripheral backend (test-only). `real` (default) opens the
    /// BlueZ adapter; `fake` wires `FakePeripheral` so the daemon
    /// binary can run on a CI machine without a radio. Used by
    /// `crates/syauth-pam/tests/pam_daemon_integration.rs`.
    #[arg(long, value_name = "MODE", hide = true, default_value = PERIPHERAL_REAL)]
    peripheral: String,
    /// Pre-seed the fake peripheral's response queue (test-only).
    /// Format: `<peer_id>:<hex-bytes>`. May be repeated. Honored
    /// only when `--peripheral=fake` is set.
    #[arg(long = "inject-response", value_name = "PEER_ID:HEX", hide = true)]
    inject_response: Vec<String>,
    /// Force the orchestrator's challenge nonce to a deterministic
    /// hex value (test-only). 32 hex chars → 16 bytes. Lets the
    /// integration test sign a response whose nonce matches the
    /// orchestrator's challenge frame so the daemon's verifier
    /// returns `ok`.
    #[arg(long = "test-fixed-nonce", value_name = "HEX", hide = true)]
    test_fixed_nonce: Option<String>,
}

/// `--peripheral=real` selects production BlueZ.
const PERIPHERAL_REAL: &str = "real";
/// `--peripheral=fake` selects `FakePeripheral` (test seam).
const PERIPHERAL_FAKE: &str = "fake";

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Err(err) = init_tracing(&cli.log_level) {
        let mut stderr = io::stderr().lock();
        let _ = writeln!(stderr, "error: failed to initialize tracing: {err:#}");
        return ExitCode::FAILURE;
    }
    let config = match build_config(&cli) {
        Ok(cfg) => cfg,
        Err(err) => {
            tracing::error!(error = %err, "failed to assemble runtime config");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(err) => {
            tracing::error!(error = %err, "failed to start tokio runtime");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(config)) {
        Ok(reason) => {
            tracing::info!(?reason, "exit");
            ExitCode::SUCCESS
        }
        Err(err) => {
            tracing::error!(error = %err, "daemon exited with error");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing(filter: &str) -> Result<()> {
    let env_filter = EnvFilter::try_new(filter).with_context(|| format!("invalid --log-level filter: {filter}"))?;
    // The `fmt` layer prefixes every line with the `target` value
    // (defaults to the emitting module path). We want a fixed syslog
    // tag instead so `journalctl -t syauth-presenced` filters
    // cleanly when the daemon's stdout is captured by systemd-
    // journald. `with_target(false)` drops the module path, then a
    // custom event formatter prepends the tag. Until S-009 wires a
    // real `libsystemd` sink, this constant-prefix layout matches
    // the SPEC's syslog-tag contract.
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .with_writer(TaggedWriter::new)
        .try_init()
        .map_err(|err| anyhow::anyhow!("failed to install tracing subscriber: {err}"))
}

/// `io::Write` wrapper that prefixes every line with the syauth-
/// presenced syslog tag so `journalctl -t syauth-presenced` filters
/// match when stdout is captured.
struct TaggedWriter {
    inner: io::Stderr,
}

impl TaggedWriter {
    fn new() -> Self {
        Self { inner: io::stderr() }
    }
}

impl io::Write for TaggedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut handle = self.inner.lock();
        handle.write_all(SYSLOG_TAG.as_bytes())?;
        handle.write_all(b": ")?;
        handle.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.lock().flush()
    }
}

fn build_config(cli: &Cli) -> Result<Config> {
    let runtime_dir = resolve_runtime_dir()?;
    let socket_default = runtime_dir.join(RUNTIME_SUBDIR).join(DEFAULT_SOCKET_BASENAME);
    let pidfile_default = runtime_dir.join(RUNTIME_SUBDIR).join(PIDFILE_BASENAME);
    let peripheral_mode = match cli.peripheral.as_str() {
        PERIPHERAL_REAL => PeripheralMode::Real,
        PERIPHERAL_FAKE => PeripheralMode::Fake,
        other => {
            return Err(anyhow::anyhow!(
                "--peripheral must be `{PERIPHERAL_REAL}` or `{PERIPHERAL_FAKE}`, got: {other}",
            ));
        }
    };
    let mut inject_responses = Vec::with_capacity(cli.inject_response.len());
    for spec in &cli.inject_response {
        let (peer_id, hex_part) = spec
            .split_once(':')
            .with_context(|| format!("--inject-response must be `<peer_id>:<hex>`, got: {spec}"))?;
        let bytes = hex::decode(hex_part).with_context(|| format!("--inject-response hex decode failed for: {spec}"))?;
        inject_responses.push(InjectedResponse {
            peer_id: peer_id.to_owned(),
            bytes,
        });
    }
    let test_fixed_nonce = match cli.test_fixed_nonce.as_deref() {
        Some(hex_str) => {
            let bytes = hex::decode(hex_str).with_context(|| format!("--test-fixed-nonce hex decode failed: {hex_str}"))?;
            let arr: [u8; syauth_core::NONCE_LEN] = bytes.as_slice().try_into().map_err(|_| {
                anyhow::anyhow!(
                    "--test-fixed-nonce must decode to {} bytes, got {}",
                    syauth_core::NONCE_LEN,
                    bytes.len()
                )
            })?;
            Some(arr)
        }
        None => None,
    };
    Ok(Config {
        socket: cli.socket.clone().unwrap_or(socket_default),
        bonds_file: cli.bonds_file.clone(),
        keys_dir: cli.keys_dir.clone(),
        audit_log_path: cli.audit_log.clone(),
        pidfile: cli.pidfile.clone().unwrap_or(pidfile_default),
        // Production default: enforce the daemon's own UID. Tests
        // construct `Config` directly with a deliberately-unreachable
        // value to exercise the `SO_PEERCRED` ACL.
        expected_uid: None,
        peripheral_mode,
        inject_responses,
        test_fixed_nonce,
    })
}

fn resolve_runtime_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var(XDG_RUNTIME_DIR_ENV)
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    // SPEC §8 Risks: "Fall back to `/run/user/$UID/syauth/auth.sock`
    // if XDG_RUNTIME_DIR is unset". `nix::unistd::geteuid` is a safe
    // wrapper around `geteuid(2)` — workspace lints deny `unsafe`
    // code so we route through nix even for a one-call lookup.
    let uid = nix::unistd::geteuid().as_raw();
    Ok(PathBuf::from(format!("/run/user/{uid}")))
}
