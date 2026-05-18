//! `syauth-presenced` library surface.
//!
//! The binary in `src/main.rs` is a clap-derived dispatcher that
//! wires `tracing_subscriber` to a syslog-tagged formatter and then
//! delegates to `runtime::run`. Every behaviour lives in library
//! modules so integration tests (and the later S-002+ orchestrator)
//! can drive the daemon in-process without spawning the binary.
//!
//! Roadmap: `specs/unlock-proximity/ROADMAP.md` Step S-001.
//! Journey: `specs/journeys/JOURNEY-S-001-scaffold-syauth-presenced.md`.

pub mod audit;
pub mod lock;
pub mod orchestrator;
pub mod rpc;
pub mod runtime;
pub mod server;

pub use audit::{AUDIT_FIELD_SEPARATOR, AUDIT_FSYNC_EVERY, AUDIT_LOG_FILE_MODE, AuditLog, AuditRecord};
pub use lock::{LockError, PidFileLock};
pub use orchestrator::{
    BUSY_QUEUE_DEADLINE, BUSY_REASON, ChallengeOutcome, DEFAULT_AUTH_TIMEOUT, NONCE_BYTES, NONCE_LRU_CAP, NonceCache,
    OUTCOME_REASON_BAD_SIGNATURE, OUTCOME_REASON_BUSY, OUTCOME_REASON_DENIED, OUTCOME_REASON_OK, OUTCOME_REASON_REPLAY,
    OUTCOME_REASON_RESPONSE_TIMEOUT, OUTCOME_REASON_TRANSPORT_ERROR, OUTCOME_REASON_UNKNOWN_PEER, Orchestrator, RELOAD_CHANNEL_CAPACITY,
    RELOAD_DEBOUNCE, RELOAD_TRIGGER_INOTIFY, RELOAD_TRIGGER_RPC, RELOAD_TRIGGER_SIGHUP, ROTATION_LOG_TARGET, ReloadCommand, ReloadTrigger,
    SECONDS_PER_MINUTE, SHORT_UUID_HEX_LEN, align_to_next_minute,
};
pub use rpc::{
    FrameError, LENGTH_PREFIX_BYTES, MAX_FRAME_LEN, PeerStatus, Request, Response, decode_frame, encode_frame, read_frame,
    read_frame_blocking, write_frame, write_frame_blocking,
};
pub use runtime::{
    BOND_KEY_FILE_EXT, Config, DEFAULT_AUDIT_LOG_PATH, DEFAULT_BONDS_FILE, DEFAULT_KEYS_DIR, DEFAULT_SOCKET_BASENAME, InjectedResponse,
    PIDFILE_BASENAME, PeripheralMode, RUNTIME_SUBDIR, RunError, ShutdownReason, run,
};
pub use server::{CONCURRENT_ACCEPT_CAP, LISTEN_MODE, STUB_CHALLENGE_REASON, ServeConfig, ServeError, serve};
