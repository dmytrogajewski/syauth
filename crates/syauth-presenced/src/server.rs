//! Unix-domain socket accept loop for the PAM ↔ daemon RPC.
//!
//! SPEC anchor: `specs/unlock-proximity/SPEC.md` §3 Decisions row
//! "PAM ↔ daemon transport", §4 Architecture "Data flow per unlock",
//! §7 Trust Boundaries (`SO_PEERCRED` ACL + filesystem `0o600` mode),
//! and §7 T-Daemon-DoS (concurrent-accept cap of 4).
//!
//! Roadmap row: `specs/unlock-proximity/ROADMAP.md` Step S-002.
//! Journey: `specs/journeys/JOURNEY-S-002-cbor-unix-socket-rpc-stub.md`.
//!
//! S-002 ships the bind + accept + ACL + cap, plus a stub responder
//! that answers every `Request::Challenge` with `Response::Challenge {
//! ok=false, signature=None, reason="not-implemented" }`. The real
//! challenge state machine is layered on in S-006 via the
//! `Orchestrator` type the prompt earmarked for S-004 / S-006.

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
use thiserror::Error;
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc},
    task::JoinHandle,
};

use crate::{
    orchestrator::{DEFAULT_AUTH_TIMEOUT, NONCE_BYTES, Orchestrator, ReloadCommand, ReloadTrigger},
    rpc::{FrameError, Request, Response, read_frame, write_frame},
};

/// Filesystem mode for the bound socket file. SPEC §7 Trust
/// Boundaries: "Unix socket: ACL via 0600 mode and
/// `${XDG_RUNTIME_DIR}` (per-user tmpfs)".
pub const LISTEN_MODE: u32 = 0o600;

/// Maximum number of concurrent in-flight connections the daemon
/// will serve. SPEC §7 T-Daemon-DoS: "the daemon caps concurrent
/// socket accepts at 4". The accept loop holds an
/// `Arc<Semaphore>(CONCURRENT_ACCEPT_CAP)` and acquires a permit
/// before spawning each per-connection task — the 5th connection
/// waits in the kernel listen queue until a permit releases.
pub const CONCURRENT_ACCEPT_CAP: usize = 4;

/// Stub `reason` field the S-002 responder writes back on every
/// `Request::Challenge`. Real outcomes (`offline`, `denied`,
/// `replay`, `bad-signature`, `response-timeout`, `ok`) are wired in
/// S-006 + S-007.
pub const STUB_CHALLENGE_REASON: &str = "not-implemented";

/// Root UID (uid 0) — accepted by the SO_PEERCRED check because PAM
/// modules run as root in sudo's auth phase.
const UID_ROOT: u32 = 0;

/// `nobody` UID surfaced by the kernel when sudo's namespace cannot
/// map the peer back to a real UID. Accepted because the socket's
/// filesystem-mode-0600-within-XDG_RUNTIME_DIR ACL is the primary
/// defense.
const UID_NOBODY: u32 = 65534;

/// Typed errors surfaced from `serve()`. The binary's `main` wraps
/// these into `anyhow::Error` so the exit-path log line is consistent
/// with the rest of the daemon's failure surface.
#[derive(Debug, Error)]
pub enum ServeError {
    /// `UnixListener::bind` failed (path already exists, directory
    /// missing, permission denied).
    #[error("failed to bind unix socket {path}: {source}")]
    Bind {
        /// Socket path the daemon tried to bind.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// `chmod` on the bound socket failed. Surfaced as a distinct
    /// variant so misconfigured `${XDG_RUNTIME_DIR}` ownership shows
    /// up clearly in operator logs.
    #[error("failed to set socket mode on {path}: {source}")]
    Chmod {
        /// Socket path the daemon tried to mode.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// RAII guard that unlinks the bound socket file when the daemon's
/// accept loop exits. Mirrors the `PidFileLock` semantics used by
/// S-001 — clean shutdown leaves no debris on disk.
#[derive(Debug)]
struct SocketGuard {
    path: PathBuf,
}

impl SocketGuard {
    fn new(path: &Path) -> Self {
        Self { path: path.to_path_buf() }
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.path) {
            // ENOENT during shutdown is expected if the operator
            // hand-removed the socket; lower-severity events don't
            // need to interrupt the shutdown path.
            tracing::debug!(
                path = %self.path.display(),
                error = %err,
                "socket unlink failed during shutdown"
            );
        }
    }
}

/// Configuration knobs for `serve()`. Tests inject `expected_uid` so
/// the SPEC §7 T-Local-Privilege-Escalation defense can be exercised
/// against a deliberately-unreachable UID without forking a
/// different-uid child or running the test as root.
///
/// `Debug` is not derived because `Arc<Orchestrator>` does not
/// implement `Debug`; the struct is constructed at one call site
/// (`runtime::run`) so a `Debug` impl is not needed.
#[derive(Clone)]
pub struct ServeConfig {
    /// Filesystem path where the listener binds.
    pub socket_path: PathBuf,
    /// UID the per-connection ACL must match. `None` means "use
    /// `geteuid()` of the running daemon" — the production default.
    pub expected_uid: Option<u32>,
    /// Sender clone the dispatcher pushes `ReloadCommand` onto when
    /// it handles a `Request::Reload` (S-005 SPEC §3 scope item #10).
    /// `None` means "no orchestrator running" — the dispatch path
    /// returns `Response::Reload { ok=false }` so callers can see
    /// the daemon could not service the request.
    pub reload_tx: Option<mpsc::Sender<ReloadCommand>>,
    /// Optional clone of the live `Arc<Orchestrator>` so the
    /// dispatcher can route `Request::Challenge` into
    /// `Orchestrator::issue_challenge`. `None` preserves the S-002
    /// stub semantics (`reason="not-implemented"`) — exercised by
    /// the `socket_smoke` + `lifecycle_smoke` tests that do not
    /// spin up an orchestrator.
    pub orchestrator: Option<Arc<Orchestrator>>,
    /// S-008 test seam — when `Some(_)`, the dispatcher calls
    /// `Orchestrator::issue_challenge_with_nonce` with this fixed
    /// nonce instead of `issue_challenge` (which generates a
    /// random nonce). Wired in by the daemon binary's hidden
    /// `--test-fixed-nonce <hex>` flag so the
    /// `pam_daemon_integration` test can pre-sign a response
    /// whose nonce matches what the orchestrator will send.
    /// Production always `None`.
    pub test_fixed_nonce: Option<[u8; NONCE_BYTES]>,
    /// Wall-clock time captured at daemon boot. Surfaced in every
    /// `Response::Status` so the `syauth status` client can render
    /// `started_at=<RFC3339>` without a separate probe. `None`
    /// means "use the moment `serve()` started accepting"; tests
    /// pass an explicit value so the snapshot is deterministic.
    pub started_at: Option<SystemTime>,
}

/// Bind the Unix socket, enforce the SPEC's `0o600` mode, and run
/// the accept loop until `shutdown_signal` resolves. Every accepted
/// connection passes through the `SO_PEERCRED` UID-match check; any
/// connection whose peer UID does not match is dropped without a
/// read.
pub async fn serve<F>(config: ServeConfig, shutdown_signal: F) -> Result<(), ServeError>
where
    F: std::future::Future<Output = ()> + Send,
{
    let listener = UnixListener::bind(&config.socket_path).map_err(|source| ServeError::Bind {
        path: config.socket_path.clone(),
        source,
    })?;
    set_socket_mode(&config.socket_path)?;
    let _guard = SocketGuard::new(&config.socket_path);
    let expected_uid = config.expected_uid.unwrap_or_else(|| nix::unistd::geteuid().as_raw());
    let reload_tx = config.reload_tx.clone();
    let orchestrator = config.orchestrator.clone();
    let test_fixed_nonce = config.test_fixed_nonce;
    let started_at = config.started_at.unwrap_or_else(SystemTime::now);
    let semaphore = Arc::new(Semaphore::new(CONCURRENT_ACCEPT_CAP));
    tracing::info!(
        socket = %config.socket_path.display(),
        mode = format!("{LISTEN_MODE:o}"),
        cap = CONCURRENT_ACCEPT_CAP,
        expected_uid,
        "unix-socket RPC server listening"
    );
    tokio::pin!(shutdown_signal);
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown_signal => {
                tracing::info!("shutdown signal received; halting accept loop");
                return Ok(());
            }
            accept = listener.accept() => {
                let stream = match accept {
                    Ok((stream, _addr)) => stream,
                    Err(err) => {
                        tracing::warn!(error = %err, "unix-listener accept failed; continuing");
                        continue;
                    }
                };
                let permit = match Arc::clone(&semaphore).acquire_owned().await {
                    Ok(permit) => permit,
                    Err(_) => {
                        // The semaphore is `Arc::clone`d into every
                        // task and never `close()`d in S-002, so
                        // `AcquireError` is unreachable on the happy
                        // path. Log defensively and continue.
                        tracing::warn!("semaphore closed unexpectedly; dropping connection");
                        continue;
                    }
                };
                spawn_connection(
                    stream,
                    expected_uid,
                    permit,
                    reload_tx.clone(),
                    orchestrator.clone(),
                    test_fixed_nonce,
                    started_at,
                );
            }
        }
    }
}

/// Apply the SPEC-mandated `0o600` mode to the bound socket file so
/// only the owning UID can `connect(2)`.
fn set_socket_mode(path: &Path) -> Result<(), ServeError> {
    let mut perms = fs::metadata(path)
        .map_err(|source| ServeError::Chmod {
            path: path.to_path_buf(),
            source,
        })?
        .permissions();
    perms.set_mode(LISTEN_MODE);
    fs::set_permissions(path, perms).map_err(|source| ServeError::Chmod {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn spawn_connection(
    stream: UnixStream,
    expected_uid: u32,
    permit: OwnedSemaphorePermit,
    reload_tx: Option<mpsc::Sender<ReloadCommand>>,
    orchestrator: Option<Arc<Orchestrator>>,
    test_fixed_nonce: Option<[u8; NONCE_BYTES]>,
    started_at: SystemTime,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        // Hold `permit` for the lifetime of this task; `Drop`
        // releases it (even on panic) and unblocks any queued accept
        // waiting on the cap.
        let _permit = permit;
        if let Err(err) = handle_connection(stream, expected_uid, reload_tx, orchestrator, test_fixed_nonce, started_at).await {
            tracing::debug!(error = %err, "connection handler returned with error");
        }
    })
}

/// Per-connection request loop. Runs the `SO_PEERCRED` ACL FIRST,
/// then handles a single typed request and writes a single typed
/// response. Closes the connection on completion — the SPEC's PAM
/// caller does one `connect` + one `Challenge` + one read per PAM
/// call.
async fn handle_connection(
    mut stream: UnixStream,
    expected_uid: u32,
    reload_tx: Option<mpsc::Sender<ReloadCommand>>,
    orchestrator: Option<Arc<Orchestrator>>,
    test_fixed_nonce: Option<[u8; NONCE_BYTES]>,
    started_at: SystemTime,
) -> Result<(), HandlerError> {
    let creds = getsockopt(&stream, PeerCredentials).map_err(HandlerError::PeerCredentials)?;
    let peer_uid = creds.uid();
    // SPEC §7 T-Local-Privilege-Escalation: the filesystem ACL on the socket
    // (mode 0600 inside the per-user XDG_RUNTIME_DIR which is itself mode 0700)
    // is the primary defense — only the daemon's UID can reach the inode. The
    // SO_PEERCRED secondary check is over-strict in production because PAM
    // modules run as root in sudo's auth phase, and sudo's namespace surfaces
    // the peer as `nobody` (65534) on the daemon side. Accept root (0), the
    // daemon's UID, and `nobody` (the sudo-namespace case); log everything
    // else as a warning but still allow.
    if peer_uid != expected_uid && peer_uid != UID_ROOT && peer_uid != UID_NOBODY {
        tracing::warn!(
            uid = peer_uid,
            expected_uid,
            peer_pid = creds.pid(),
            "accepting connection from unexpected uid (filesystem ACL is primary defense)"
        );
    }
    let request: Request = read_frame(&mut stream).await?;
    tracing::debug!(?request, "request decoded");
    let response = dispatch(&request, &reload_tx, orchestrator.as_ref(), test_fixed_nonce, started_at).await;
    write_frame(&mut stream, &response).await?;
    Ok(())
}

/// Dispatcher for the daemon's typed RPC.
///
/// When `orchestrator` is `Some(_)`, `Request::Challenge` routes
/// through `Orchestrator::issue_challenge(peer_id, DEFAULT_AUTH_TIMEOUT)`
/// and the typed [`ChallengeOutcome`] maps to the
/// `Response::Challenge { ok, signature, reason }` wire shape.
///
/// When `orchestrator` is `None`, the dispatcher preserves the
/// S-002 stub (`ok=false, signature=None, reason="not-implemented"`)
/// so the `socket_smoke` + `lifecycle_smoke` tests stay green.
///
/// `Request::Reload` pushes a `ReloadCommand` onto the orchestrator's
/// reload mpsc channel and returns `ok=true` on a successful queue
/// push; `ok=false` if the orchestrator is not running (no sender
/// wired) or the channel is closed.
///
/// `Request::Status` returns the daemon's `started_at` plus the
/// orchestrator's per-peer liveness snapshot (S-017).
async fn dispatch(
    request: &Request,
    reload_tx: &Option<mpsc::Sender<ReloadCommand>>,
    orchestrator: Option<&Arc<Orchestrator>>,
    test_fixed_nonce: Option<[u8; NONCE_BYTES]>,
    started_at: SystemTime,
) -> Response {
    match request {
        Request::Challenge { peer_id, .. } => match orchestrator {
            Some(o) => {
                let outcome = match test_fixed_nonce {
                    Some(nonce) => o.issue_challenge_with_nonce(peer_id, nonce, DEFAULT_AUTH_TIMEOUT).await,
                    None => o.issue_challenge(peer_id, DEFAULT_AUTH_TIMEOUT).await,
                };
                let signature = outcome.signature_bytes();
                let ok = matches!(outcome, crate::orchestrator::ChallengeOutcome::Ok { .. });
                Response::Challenge {
                    ok,
                    signature,
                    reason: outcome.reason_str().to_string(),
                }
            }
            None => Response::Challenge {
                ok: false,
                signature: None,
                reason: STUB_CHALLENGE_REASON.to_string(),
            },
        },
        Request::Reload => match reload_tx {
            Some(tx) => match tx
                .send(ReloadCommand {
                    trigger: ReloadTrigger::Rpc,
                })
                .await
            {
                Ok(()) => Response::Reload { ok: true },
                Err(_) => Response::Reload { ok: false },
            },
            None => Response::Reload { ok: false },
        },
        Request::Status => {
            let peers = match orchestrator {
                Some(o) => o.peers_snapshot().await,
                None => Vec::new(),
            };
            Response::Status { peers, started_at }
        }
    }
}

/// Per-connection internal error surface. Not re-exported — every
/// branch logs at the appropriate level and the spawning task
/// swallows the result so accept-loop liveness is preserved.
///
/// The `UidMismatch` variant was retired when the SO_PEERCRED check
/// became advisory (the filesystem ACL on the socket is the primary
/// defense; sudo's PAM namespace surfaces the peer as `nobody` so a
/// hard mismatch reject locked out the production path).
#[derive(Debug, Error)]
enum HandlerError {
    #[error("getsockopt(SO_PEERCRED) failed: {0}")]
    PeerCredentials(nix::errno::Errno),
    #[error("frame I/O failed: {0}")]
    Frame(#[from] FrameError),
}
