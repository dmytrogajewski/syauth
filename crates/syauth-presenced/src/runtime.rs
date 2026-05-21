//! Daemon runtime: acquire the single-instance lock, install the
//! signal handler, run the empty event loop, clean up.
//!
//! SPEC anchor: `specs/unlock-proximity/SPEC.md` §6 Rehydration cold-
//! start sequence step 1 ("open `${XDG_RUNTIME_DIR}/syauth/auth.sock`
//! (file lock + bind)"). S-001 implements only the *lock* half of step
//! 1; the bind arrives in S-002.
//! Roadmap row: S-001 DoR / DoD.
//! Journey: `specs/journeys/JOURNEY-S-001-scaffold-syauth-presenced.md`.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use syauth_core::{Bond, BondStatus, BondStore, bond_key_from_pubkeys, peer_id_from_pubkey};
use syauth_transport::{BOND_KEY_BYTES, DEFAULT_ADAPTER_NAME, FakePeripheral, Peripheral, PersistentPeripheral};
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::mpsc,
    time::Instant,
};

use crate::{
    lock::{LockError, PidFileLock},
    orchestrator::{Orchestrator, RELOAD_DEBOUNCE, ROTATION_LOG_TARGET, ReloadCommand, ReloadTrigger, align_to_next_minute},
    server::{self, ServeConfig, ServeError},
};

/// Default `--socket` value. Anchored in
/// `specs/unlock-proximity/SPEC.md` §3 Approach.
pub const DEFAULT_SOCKET_BASENAME: &str = "auth.sock";

/// Default `--bonds-file` value. Anchored in SPEC §3 Approach.
pub const DEFAULT_BONDS_FILE: &str = "/var/lib/syauth/bonds.toml";

/// Default `--keys-dir` value. Anchored in SPEC §3 Approach.
pub const DEFAULT_KEYS_DIR: &str = "/var/lib/syauth/keys/";

/// Default `--audit-log` value. Anchored in SPEC §3 scope item #8 +
/// §7 Audit ("`/var/lib/syauth/last.log` (append-only)").
pub const DEFAULT_AUDIT_LOG_PATH: &str = "/var/lib/syauth/last.log";

/// Per-peer bond-key file extension under `<keys_dir>/<peer_id>.bin`.
/// Matches the layout written by `syauth pair` and read by
/// `crates/syauth-pam/src/auth.rs::load_bond_key_from_file`.
pub const BOND_KEY_FILE_EXT: &str = ".bin";

/// Basename of the single-instance pidfile under
/// `${XDG_RUNTIME_DIR}/syauth/`. Anchored in SPEC §3 scope item #1.
pub const PIDFILE_BASENAME: &str = "presenced.pid";

/// Name of the syauth subdirectory under `${XDG_RUNTIME_DIR}` that
/// holds both the socket (S-002+) and the pidfile (S-001).
pub const RUNTIME_SUBDIR: &str = "syauth";

/// Top-level error surface for `run()`. The binary's `main` wraps
/// these into `anyhow::Error` so the user-visible message at exit is
/// consistent across the lock path and the socket path.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The pidfile lock could not be acquired (typed via `LockError`
    /// so the second-instance case is distinguishable from disk-full
    /// or permission denials).
    #[error(transparent)]
    Lock(#[from] LockError),
    /// Failed to install a Unix signal handler for SIGINT or SIGTERM
    /// (e.g., signal-disposition table exhausted).
    #[error("failed to install signal handler for {signal}: {source}")]
    Signal {
        /// Human-readable signal name (`SIGTERM` / `SIGINT`).
        signal: &'static str,
        /// Underlying I/O error from `tokio::signal::unix::signal`.
        #[source]
        source: std::io::Error,
    },
    /// The Unix-socket accept loop bind / chmod / accept layer
    /// failed (S-002 `ServeError`).
    #[error(transparent)]
    Serve(#[from] ServeError),
}

/// Reason the daemon's main loop exited. Useful for tests that want
/// to assert "the daemon stopped because of SIGTERM, not because of an
/// internal panic".
#[derive(Debug, PartialEq, Eq)]
pub enum ShutdownReason {
    /// `SIGTERM` received (systemd `stop`, manual `kill <pid>`).
    Sigterm,
    /// `SIGINT` received (operator Ctrl+C).
    Sigint,
}

/// Which `Peripheral` implementation the daemon should wire into the
/// orchestrator. Production uses `PersistentPeripheral` over BlueZ;
/// the S-008 PAM↔daemon integration test sets [`PeripheralMode::Fake`]
/// via the hidden `--peripheral=fake` flag so CI can drive a full
/// challenge round-trip without a real radio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PeripheralMode {
    /// Production path: open the BlueZ adapter and host a real GATT
    /// peripheral.
    #[default]
    Real,
    /// Test seam: use [`syauth_transport::FakePeripheral`] so the
    /// daemon binary can be exercised on a CI runner with no radio.
    Fake,
}

/// One pre-seeded response injection. Carries the `peer_id` to inject
/// against and the raw response bytes that
/// [`syauth_transport::FakePeripheral::inject_response`] will return on
/// the next `wait_for_response(peer_id, _)` call.
#[derive(Debug, Clone)]
pub struct InjectedResponse {
    /// Bond identifier (matches `Request::Challenge::peer_id`).
    pub peer_id: String,
    /// Raw response bytes (typically a 64-byte Ed25519 signature
    /// over the challenge body).
    pub bytes: Vec<u8>,
}

/// Runtime configuration assembled from the CLI args by `main`. The
/// fields' default values are encoded in `clap` derive attributes in
/// `cli::Cli` and reproduced as named constants above so tests can
/// reference them without parsing `--help` output.
#[derive(Debug, Clone)]
pub struct Config {
    /// Path to the Unix socket the daemon will bind (S-002).
    pub socket: PathBuf,
    /// Path to the bonds TOML file. Recorded for log lines.
    pub bonds_file: PathBuf,
    /// Path to the per-peer keys directory. Recorded for log lines.
    pub keys_dir: PathBuf,
    /// Path to the append-only audit log (SPEC §3 scope item #8 +
    /// §7 Audit). Production defaults to
    /// `/var/lib/syauth/last.log`; tests override.
    pub audit_log_path: PathBuf,
    /// Pidfile path. Defaults to
    /// `${XDG_RUNTIME_DIR}/syauth/presenced.pid` in `main`; tests
    /// override it.
    pub pidfile: PathBuf,
    /// UID the per-connection `SO_PEERCRED` ACL must match. `None`
    /// means "use `geteuid()` of the running daemon" — the
    /// production default. Tests inject `Some(0)` (or any
    /// deliberately-unreachable UID) to exercise the SPEC §7
    /// T-Local-Privilege-Escalation defense without forking a
    /// different-uid child.
    pub expected_uid: Option<u32>,
    /// Which `Peripheral` impl to wire (S-008 test seam).
    pub peripheral_mode: PeripheralMode,
    /// Optional pre-seeded responses (S-008 test seam) — only honored
    /// when [`Config::peripheral_mode`] is [`PeripheralMode::Fake`].
    pub inject_responses: Vec<InjectedResponse>,
    /// Optional fixed challenge nonce (S-008 test seam) — when set,
    /// the dispatcher calls
    /// `Orchestrator::issue_challenge_with_nonce` instead of the
    /// random-nonce production entry point. Production always
    /// `None`.
    pub test_fixed_nonce: Option<[u8; syauth_core::NONCE_LEN]>,
}

impl Config {
    /// Construct the canonical runtime layout under `runtime_dir`:
    /// socket + pidfile go under `runtime_dir/syauth/`. Tests pass a
    /// tempdir; production passes `${XDG_RUNTIME_DIR}`.
    pub fn with_runtime_dir(runtime_dir: &Path) -> Self {
        let subdir = runtime_dir.join(RUNTIME_SUBDIR);
        Self {
            socket: subdir.join(DEFAULT_SOCKET_BASENAME),
            bonds_file: PathBuf::from(DEFAULT_BONDS_FILE),
            keys_dir: PathBuf::from(DEFAULT_KEYS_DIR),
            audit_log_path: PathBuf::from(DEFAULT_AUDIT_LOG_PATH),
            pidfile: subdir.join(PIDFILE_BASENAME),
            expected_uid: None,
            peripheral_mode: PeripheralMode::Real,
            inject_responses: Vec::new(),
            test_fixed_nonce: None,
        }
    }
}

/// Run the daemon. Acquires the single-instance lock, installs the
/// SIGINT/SIGTERM handler, binds the Unix-socket RPC server (S-002),
/// and blocks the calling task until a signal arrives. Returns the
/// shutdown reason so the caller can log it.
///
/// Contract: this function never `unwrap`s, never panics, and never
/// lets `Drop(PidFileLock)` or `Drop(SocketGuard)` skip — the `?`
/// operator in the signal-handler / lock-acquire setup runs BEFORE
/// the accept loop spawns, so any error there drops the lock guard
/// and unlinks the pidfile + socket on the way out.
pub async fn run(config: Config) -> Result<ShutdownReason, RunError> {
    tracing::info!(
        socket = %config.socket.display(),
        bonds_file = %config.bonds_file.display(),
        keys_dir = %config.keys_dir.display(),
        pidfile = %config.pidfile.display(),
        "syauth-presenced started"
    );

    // Install signal handlers BEFORE acquiring the lock so a signal
    // delivered between lock acquisition and handler install can't
    // bypass the cleanup path.
    let mut sigterm = signal(SignalKind::terminate()).map_err(|source| RunError::Signal { signal: "SIGTERM", source })?;
    let mut sigint = signal(SignalKind::interrupt()).map_err(|source| RunError::Signal { signal: "SIGINT", source })?;
    let mut sighup = signal(SignalKind::hangup()).map_err(|source| RunError::Signal { signal: "SIGHUP", source })?;

    let lock = PidFileLock::acquire(&config.pidfile)?;
    tracing::info!(
        pidfile = %lock.path().display(),
        "single-instance pidfile lock acquired"
    );

    let (orch_shutdown_tx, orch_shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let (orch_task, reload_tx, orch_handle) = maybe_spawn_orchestrator(&config, orch_shutdown_rx).await;

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let serve_config = ServeConfig {
        socket_path: config.socket.clone(),
        expected_uid: config.expected_uid,
        reload_tx: reload_tx.clone(),
        orchestrator: orch_handle,
        test_fixed_nonce: config.test_fixed_nonce,
        started_at: Some(std::time::SystemTime::now()),
    };
    let serve_task = tokio::spawn(async move {
        server::serve(serve_config, async move {
            let _ = shutdown_rx.await;
        })
        .await
    });

    let inotify_task = reload_tx.as_ref().map(|tx| spawn_inotify_watcher(&config.bonds_file, tx.clone()));

    let reason = wait_for_reason(&mut sigterm, &mut sigint, &mut sighup, reload_tx.as_ref()).await;
    tracing::info!(?reason, "shutdown signal received, draining");
    let _ = shutdown_tx.send(());
    let _ = orch_shutdown_tx.send(());
    if let Some(handle) = orch_task {
        let _ = handle.await;
    }
    if let Some(handle) = inotify_task {
        handle.abort();
    }
    match serve_task.await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::error!(error = %err, "accept loop returned error");
            drop(lock);
            return Err(RunError::from(err));
        }
        Err(join_err) => {
            tracing::error!(error = %join_err, "accept-loop task join failed");
        }
    }

    // Drop runs here (function returns); the kernel releases the
    // F_OFD_SETLK lock and the pidfile is unlinked on the way out.
    drop(lock);
    tracing::info!("syauth-presenced stopped cleanly");
    Ok(reason)
}

async fn wait_for_reason(
    sigterm: &mut tokio::signal::unix::Signal,
    sigint: &mut tokio::signal::unix::Signal,
    sighup: &mut tokio::signal::unix::Signal,
    reload_tx: Option<&mpsc::Sender<ReloadCommand>>,
) -> ShutdownReason {
    loop {
        tokio::select! {
            biased;
            _ = sigterm.recv() => return ShutdownReason::Sigterm,
            _ = sigint.recv() => return ShutdownReason::Sigint,
            _ = sighup.recv() => {
                if let Some(tx) = reload_tx {
                    if let Err(err) = tx.send(ReloadCommand { trigger: ReloadTrigger::Sighup }).await {
                        tracing::warn!(target: ROTATION_LOG_TARGET, error = %err, "SIGHUP reload push failed");
                    }
                } else {
                    tracing::warn!(target: ROTATION_LOG_TARGET, "SIGHUP received but no orchestrator running; ignoring");
                }
            }
        }
    }
}

/// Try to construct an [`Orchestrator`] over a [`PersistentPeripheral`]
/// and spawn it. Returns `(None, None)` (and logs a `warn`) if no
/// non-revoked bond exists, the key file is missing or malformed, or
/// the BlueZ adapter cannot be opened — in any of those cases the
/// daemon stays up with only the S-002 Unix-socket loop.
///
/// On success the orchestrator is constructed over the cold-start
/// peer (a single bond for now; multi-peer hydration on cold start
/// is built on top of this single-bond seed by the reload pipeline
/// re-reading `bonds.toml` on the first SIGHUP or inotify event after
/// startup). The returned `mpsc::Sender<ReloadCommand>` clone is
/// wired into `ServeConfig::reload_tx` and the SIGHUP / inotify
/// dispatchers.
///
/// This split keeps the S-001 `lifecycle_smoke` tests green: those
/// tests run with a non-existent `bonds.toml`, which `BondStore::load`
/// renders as an empty store, which routes through the `warn`
/// short-circuit before any BlueZ DBus call is made.
async fn maybe_spawn_orchestrator(
    config: &Config,
    shutdown: tokio::sync::oneshot::Receiver<()>,
) -> (
    Option<tokio::task::JoinHandle<()>>,
    Option<mpsc::Sender<ReloadCommand>>,
    Option<Arc<Orchestrator>>,
) {
    let store = match BondStore::load(&config.bonds_file) {
        Ok(s) => s,
        Err(err) => {
            warn_no_orchestrator(&format!("bonds.toml load failed: {err}"));
            return (None, None, None);
        }
    };
    let first_active = store.list().iter().find(|b| matches!(b.status, BondStatus::Bonded)).cloned();
    // Bring up the BlueZ peripheral + pair-event consumer EVEN WHEN
    // no bond exists yet. This is the load-bearing change for the
    // daemon-owned pair flow: a fresh-install daemon must be
    // discoverable so a phone can complete its first pair, after
    // which the consumer adds the bond and the orchestrator can
    // spin up on the next reload.
    type PairEventRx = tokio::sync::mpsc::Receiver<([u8; 32], [u8; 32])>;
    let (peripheral, pair_events): (Arc<dyn Peripheral + Send + Sync>, Option<PairEventRx>) = match config.peripheral_mode {
        PeripheralMode::Real => match PersistentPeripheral::new(DEFAULT_ADAPTER_NAME).await {
            Ok((p, rx)) => (p, Some(rx)),
            Err(err) => {
                warn_no_orchestrator(&format!("BlueZ adapter open failed: {err}"));
                return (None, None, None);
            }
        },
        PeripheralMode::Fake => (seed_fake_peripheral(config), None),
    };
    // Load any pre-existing non-revoked bond into the orchestrator's
    // seed peer set. With zero bonds the seed is empty — the
    // orchestrator still runs (it advertises the pair-mode UUID every
    // minute so a phone can complete its first pair through the
    // daemon-owned pair flow).
    let mut seed_peers: Vec<(Bond, [u8; BOND_KEY_BYTES])> = Vec::new();
    if let Some(b) = first_active {
        match load_bond_key(&config.keys_dir, &b.peer_id) {
            Ok(k) => {
                if let Err(err) = peripheral.add_peer(&b.peer_id, &k).await {
                    warn_no_orchestrator(&format!("peripheral add_peer failed: {err}"));
                    return (None, None, None);
                }
                seed_peers.push((b, k));
            }
            Err(reason) => {
                tracing::warn!(
                    target: "syauth_presenced",
                    reason,
                    "seed bond key load failed; orchestrator will run with empty peer set (pair mode only)"
                );
            }
        }
    }
    let audit_log = match crate::audit::AuditLog::open(&config.audit_log_path) {
        Ok(l) => Some(l),
        Err(err) => {
            tracing::warn!(
                path = %config.audit_log_path.display(),
                error = %err,
                "audit log open failed; orchestrator will run without audit appends"
            );
            None
        }
    };
    let start = Instant::now() + align_to_next_minute(std::time::SystemTime::now());
    let orchestrator = Arc::new(Orchestrator::with_peers_and_audit(
        peripheral,
        seed_peers,
        config.bonds_file.clone(),
        config.keys_dir.clone(),
        start,
        audit_log,
    ));
    let reload_tx = orchestrator.reload_sender();
    // Spawn the pair-event consumer now that we have a reload_tx
    // handle. The consumer writes bonds.toml + key file then signals
    // the orchestrator to reload; the orchestrator handles
    // peripheral.add_peer + internal peer set update atomically so
    // the pair listener and the rotation loop stay in sync.
    if let Some(rx) = pair_events {
        let bonds_path = config.bonds_file.clone();
        let keys_dir = config.keys_dir.clone();
        let reload_tx_for_pair = reload_tx.clone();
        tokio::spawn(pair_event_consumer(rx, bonds_path, keys_dir, reload_tx_for_pair));
    }
    let orchestrator_handle = Arc::clone(&orchestrator);
    let handle = tokio::spawn(Arc::clone(&orchestrator).run(shutdown));
    (Some(handle), Some(reload_tx), Some(orchestrator_handle))
}

/// Spawn a tokio task that hosts a `notify::recommended_watcher`
/// rooted at the parent directory of `bonds_file`, debouncing bursts
/// of `Modify` / `Create` / `Remove` events into one reload command
/// (SPEC §8 Risks row, belt-and-suspenders for SIGHUP delivery loss).
/// Watcher init failure logs a `warn` and the daemon falls back to
/// SIGHUP-only.
fn spawn_inotify_watcher(bonds_file: &Path, reload_tx: mpsc::Sender<ReloadCommand>) -> tokio::task::JoinHandle<()> {
    let bonds_file = bonds_file.to_path_buf();
    tokio::spawn(async move {
        let parent = match bonds_file.parent() {
            Some(p) => p.to_path_buf(),
            None => {
                tracing::warn!(target: ROTATION_LOG_TARGET, "bonds_file has no parent dir; inotify watcher disabled");
                return;
            }
        };
        let (event_tx, mut event_rx) = mpsc::channel::<()>(32);
        let bonds_file_owned = bonds_file.to_owned();
        let watcher_handle = match notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res
                && matches!(
                    event.kind,
                    notify::EventKind::Create(_) | notify::EventKind::Modify(_) | notify::EventKind::Remove(_)
                )
                && event.paths.iter().any(|p| p == &bonds_file_owned)
            {
                let _ = event_tx.blocking_send(());
            }
        }) {
            Ok(w) => w,
            Err(err) => {
                tracing::warn!(target: ROTATION_LOG_TARGET, error = %err, "inotify watcher init failed; daemon falling back to SIGHUP-only");
                return;
            }
        };
        let mut watcher = watcher_handle;
        if let Err(err) = notify::Watcher::watch(&mut watcher, &parent, notify::RecursiveMode::NonRecursive) {
            tracing::warn!(target: ROTATION_LOG_TARGET, error = %err, "inotify watch root failed; daemon falling back to SIGHUP-only");
            return;
        }
        loop {
            match event_rx.recv().await {
                Some(()) => {
                    // Debounce window: drain any additional events
                    // that arrive during the sleep so a burst of
                    // CLOSE_WRITE / MOVED_TO from `tempfile::persist`
                    // collapses into one reload.
                    tokio::time::sleep(Duration::from_millis(RELOAD_DEBOUNCE.as_millis() as u64)).await;
                    while event_rx.try_recv().is_ok() {}
                    if let Err(err) = reload_tx
                        .send(ReloadCommand {
                            trigger: ReloadTrigger::Inotify,
                        })
                        .await
                    {
                        tracing::warn!(target: ROTATION_LOG_TARGET, error = %err, "inotify reload push failed; watcher exiting");
                        return;
                    }
                }
                None => return,
            }
        }
    })
}

/// Build a [`FakePeripheral`], pre-seed its response queue with the
/// configured [`InjectedResponse`]s, and return it as a `Peripheral`
/// trait object. S-008 test seam — only reached when
/// [`Config::peripheral_mode`] is [`PeripheralMode::Fake`].
fn seed_fake_peripheral(config: &Config) -> Arc<dyn Peripheral + Send + Sync> {
    let fake = FakePeripheral::new();
    for injection in &config.inject_responses {
        fake.inject_response(&injection.peer_id, injection.bytes.clone());
    }
    fake
}

/// Read `<keys_dir>/<peer_id>.bin`, validate length, return the
/// 32-byte bond key. Mirrors `crates/syauth-pam/src/auth.rs`'s
/// `load_bond_key_from_file` shape (without the mode check — the
/// daemon's threat model trusts the keys directory's permissions
/// rather than re-validating per call).
fn load_bond_key(keys_dir: &Path, peer_id: &str) -> Result<[u8; BOND_KEY_BYTES], String> {
    let path = keys_dir.join(format!("{peer_id}{BOND_KEY_FILE_EXT}"));
    let bytes = std::fs::read(&path).map_err(|err| format!("read {}: {err}", path.display()))?;
    if bytes.len() != BOND_KEY_BYTES {
        return Err(format!(
            "{} has wrong length: expected {BOND_KEY_BYTES} bytes, got {}",
            path.display(),
            bytes.len()
        ));
    }
    let mut out = [0u8; BOND_KEY_BYTES];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Log the "orchestrator not started" warn line. Factored out so the
/// short-circuits in [`maybe_spawn_orchestrator`] read uniformly.
fn warn_no_orchestrator(reason: &str) {
    tracing::warn!(reason, "orchestrator not started; daemon will serve socket only");
}

/// Consume pair events from the peripheral's pair watcher. Each event
/// is a `(host_pubkey, phone_pubkey)` pair the phone wrote to the
/// pair-mode characteristic after completing LESC bonding. The
/// consumer derives the canonical bond_key, persists the bond record
/// and per-peer key file, then asks the orchestrator to reload —
/// `peripheral.add_peer` happens inside the orchestrator's reload
/// path so the rotation set, internal peer map, and peripheral state
/// stay in lockstep.
///
/// Best-effort: a failure on any step logs a warn and continues — the
/// phone will retry the write if the link stays alive, or the user
/// will rerun pair if the link dropped. We never panic the daemon on
/// a malformed pair attempt.
async fn pair_event_consumer(
    mut rx: tokio::sync::mpsc::Receiver<([u8; 32], [u8; 32])>,
    bonds_path: std::path::PathBuf,
    keys_dir: std::path::PathBuf,
    reload_tx: mpsc::Sender<ReloadCommand>,
) {
    while let Some((host_pubkey, phone_pubkey)) = rx.recv().await {
        let peer_id = peer_id_from_pubkey(&phone_pubkey);
        let bond_key = bond_key_from_pubkeys(&host_pubkey, &phone_pubkey);
        tracing::info!(
            target: "syauth_presenced",
            peer_id = %peer_id,
            "pair: deriving bond and persisting"
        );
        // 1. Write the per-peer key file at <keys_dir>/<peer_id>.bin.
        //    Permissions match the established convention used by
        //    `syauth pair`: 0600, parent dir created on first run.
        if let Err(err) = persist_pair_bond_key(&keys_dir, &peer_id, &bond_key) {
            tracing::warn!(
                target: "syauth_presenced",
                peer_id = %peer_id,
                error = %err,
                "pair: bond key write failed"
            );
            continue;
        }
        // 2. Add the bond record to bonds.toml. We use `--force`-style
        //    semantics: any existing record for this peer_id is
        //    replaced (the phone explicitly chose to pair again).
        let bond = Bond {
            peer_id: peer_id.clone(),
            pubkey: phone_pubkey,
            name: PAIR_DEFAULT_BOND_NAME.to_string(),
            created_at: ::time::OffsetDateTime::now_utc(),
            status: BondStatus::Bonded,
        };
        if let Err(err) = persist_pair_bond_record(&bonds_path, bond) {
            tracing::warn!(
                target: "syauth_presenced",
                peer_id = %peer_id,
                error = %err,
                "pair: bond record write failed"
            );
            continue;
        }
        // 3. Signal the orchestrator to reload. It will re-read
        //    bonds.toml, diff against its live peer set, and call
        //    `peripheral.add_peer` itself so the rotation loop and
        //    the peripheral state stay coherent.
        if let Err(err) = reload_tx.send(ReloadCommand { trigger: ReloadTrigger::Pair }).await {
            tracing::warn!(
                target: "syauth_presenced",
                peer_id = %peer_id,
                error = %err,
                "pair: orchestrator reload signal failed; daemon will pick the bond up on next inotify event"
            );
            continue;
        }
        tracing::info!(
            target: "syauth_presenced",
            peer_id = %peer_id,
            "pair: bond persisted; orchestrator reload requested"
        );
    }
    tracing::info!(target: "syauth_presenced", "pair_event_consumer: channel closed");
}

/// Default name surfaced in `syauth list` for a bond minted by the
/// daemon's pair flow (operator never typed a name on the desktop).
const PAIR_DEFAULT_BOND_NAME: &str = "phone (paired via daemon)";

/// Write a 32-byte bond key to `<keys_dir>/<peer_id>.bin` with mode
/// 0600. Creates `keys_dir` if it does not exist with mode 0700.
fn persist_pair_bond_key(keys_dir: &Path, peer_id: &str, bond_key: &[u8; BOND_KEY_BYTES]) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(keys_dir)?;
    let _ = std::fs::set_permissions(keys_dir, std::fs::Permissions::from_mode(0o700));
    let path = keys_dir.join(format!("{peer_id}{BOND_KEY_FILE_EXT}"));
    std::fs::write(&path, bond_key)?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    Ok(())
}

/// Load `bonds_path`, replace-or-add the bond record, save back.
/// `--force` semantics: any existing record for the same peer_id is
/// replaced; the phone explicitly chose to pair again.
fn persist_pair_bond_record(bonds_path: &Path, bond: Bond) -> Result<(), syauth_core::BondError> {
    let mut store = BondStore::load(bonds_path)?;
    if store.list().iter().any(|b| b.peer_id == bond.peer_id) {
        store.remove(&bond.peer_id)?;
    }
    store.add(bond)?;
    store.save(bonds_path)?;
    Ok(())
}
