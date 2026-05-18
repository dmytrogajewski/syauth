//! `Orchestrator` — owns the long-lived `Peripheral` handle and drives
//! the per-minute session-UUID rotation timer + the multi-peer bond
//! reload pipeline.
//!
//! SPEC anchor: `specs/unlock-proximity/SPEC.md` §3 Decisions row
//! "Rotating UUID cadence" (per-minute, derived from
//! `session_uuid_for(bond_key, minute)`), §3 scope items #2, #3, #4
//! (multi-peer: union of N rotating UUIDs in one `Advertisement`),
//! #10 (pair flow signals daemon so a fresh bond becomes advertisable
//! without restart), §6 Rehydration cold-start steps 3–6, §7
//! T-Presence-Tracking.
//!
//! Roadmap rows: `specs/unlock-proximity/ROADMAP.md` Step S-004 (the
//! single-bond rotation surface) + Step S-005 (the multi-peer +
//! reload extension).
//! Journeys:
//! `specs/journeys/JOURNEY-S-004-session-uuid-rotation.md`,
//! `specs/journeys/JOURNEY-S-005-multi-peer-bonds-reload.md`.
//!
//! The orchestrator's `Peripheral` handle is `Arc<dyn Peripheral +
//! Send + Sync>` so tests inject a `FakePeripheral` and production
//! injects a `PersistentPeripheral`.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration as StdDuration, SystemTime, UNIX_EPOCH},
};

use syauth_core::{
    Bond, BondStore, Frame, NONCE_LEN, SIGNATURE_LEN, SYAUTH_WIRE_VERSION_V1, Signature, TAG_LEN, VerifyingKey, bond::PUBKEY_LEN,
    verify_frame,
};
use syauth_transport::{BOND_KEY_BYTES, Peripheral, PeripheralError, session_uuid_for};
use tokio::{
    sync::{Mutex as TokioMutex, Semaphore, mpsc, oneshot},
    time::{Duration, Instant, interval_at},
};
use uuid::Uuid;

use crate::audit::{AuditLog, AuditRecord};

/// Number of seconds in one wall-clock minute. Named so the
/// "minute floor" formula at the call site reads as a domain concept,
/// not a magic divisor. Matches `syauth_transport::SECONDS_PER_MINUTE`
/// (which is typed `i64` for the HKDF minute integer; here we need a
/// `u64` for `Duration::from_secs` and a `u32` for arithmetic, so the
/// constant is declared locally to keep the rotation-timer arithmetic
/// in a single unit-system).
pub const SECONDS_PER_MINUTE: u64 = 60;

/// Width of the short UUID hex prefix emitted in the rotation audit
/// line per SPEC §3 scope item #22
/// (`syauth-presenced: rotated id=<peer> minute=<N> uuid=<short>`).
/// 8 hex chars = 32 bits of UUID — enough to disambiguate two
/// consecutive rotations in `journalctl` without printing the full
/// 36-char UUID on every line.
pub const SHORT_UUID_HEX_LEN: usize = 8;

/// `tracing` target string for the rotation + reload audit lines.
/// Matches the syslog tag declared in
/// `crates/syauth-presenced/src/main.rs` so `journalctl -t
/// syauth-presenced` filters pick up both audit trails.
pub const ROTATION_LOG_TARGET: &str = "syauth-presenced";

/// Debounce window applied to the inotify-on-bonds.toml reload path
/// (SPEC §8 Risks row "Phone re-pair changes the bond_key; daemon
/// caches stale key in memory" — closure: "daemon also watches
/// `bonds.toml` via inotify"). A burst of `CLOSE_WRITE` /
/// `MOVED_TO` events from a `tempfile::persist` rename is collapsed
/// into one reload. 200 ms is the published lower bound on
/// operator-observable reload latency.
pub const RELOAD_DEBOUNCE: StdDuration = StdDuration::from_millis(200);

/// Audit-line `trigger=` value emitted on a SIGHUP-driven reload.
pub const RELOAD_TRIGGER_SIGHUP: &str = "sighup";

/// Audit-line `trigger=` value emitted on a `Request::Reload`
/// RPC-driven reload.
pub const RELOAD_TRIGGER_RPC: &str = "rpc";

/// Audit-line `trigger=` value emitted on an inotify-driven reload.
pub const RELOAD_TRIGGER_INOTIFY: &str = "inotify";

/// Audit-line `trigger=` value emitted on the test-shim reload path.
/// `pub(crate)` because no production caller should ever emit this
/// trigger; the constant is only used by the in-process test seam
/// `Orchestrator::signal_reload_for_test`.
pub(crate) const RELOAD_TRIGGER_TEST: &str = "test";

/// Bounded capacity for the reload `mpsc` channel. Three sources push
/// reload commands (SIGHUP, RPC, inotify); a 16-slot queue is
/// comfortably above any plausible burst rate the SPEC's pair flow
/// produces.
pub const RELOAD_CHANNEL_CAPACITY: usize = 16;

/// Default deadline for [`Orchestrator::issue_challenge`]. SPEC §4.3:
/// "Offline-detect latency (daemon socket up, phone unreachable):
/// ≤ 1.2 s per SPEC §4.3". The PAM caller falls through to FIDO
/// when this deadline elapses without a response on the
/// per-peer response characteristic.
pub const DEFAULT_AUTH_TIMEOUT: StdDuration = StdDuration::from_millis(8000);

/// Width in bytes of the per-challenge nonce. The value matches
/// `syauth_core::NONCE_LEN` (SPEC §3 scope item #6); the constant
/// is re-exposed here so the orchestrator's call site reads as a
/// domain concept and grep matches the SPEC clause.
pub const NONCE_BYTES: usize = NONCE_LEN;

/// `Response::Challenge { reason }` value emitted for the
/// [`ChallengeOutcome::Ok`] branch.
pub const OUTCOME_REASON_OK: &str = "ok";

/// `Response::Challenge { reason }` value emitted for the
/// [`ChallengeOutcome::Denied`] branch.
pub const OUTCOME_REASON_DENIED: &str = "denied";

/// `Response::Challenge { reason }` value emitted for the
/// [`ChallengeOutcome::Replay`] branch. S-006 never produces this
/// outcome; the LRU nonce cache that drives it lands in S-007.
pub const OUTCOME_REASON_REPLAY: &str = "replay";

/// `Response::Challenge { reason }` value emitted for the
/// [`ChallengeOutcome::BadSignature`] branch.
pub const OUTCOME_REASON_BAD_SIGNATURE: &str = "bad-signature";

/// `Response::Challenge { reason }` value emitted for the
/// [`ChallengeOutcome::TimedOut`] branch.
pub const OUTCOME_REASON_RESPONSE_TIMEOUT: &str = "response-timeout";

/// `Response::Challenge { reason }` value emitted for the
/// [`ChallengeOutcome::UnknownPeer`] branch.
pub const OUTCOME_REASON_UNKNOWN_PEER: &str = "unknown-peer";

/// `Response::Challenge { reason }` value emitted for the
/// [`ChallengeOutcome::TransportError`] branch.
pub const OUTCOME_REASON_TRANSPORT_ERROR: &str = "transport-error";

/// `Response::Challenge { reason }` value emitted for the
/// [`ChallengeOutcome::Busy`] branch. SPEC §3 scope item #7:
/// "on overflow the daemon returns `ChallengeResponse { ok=false,
/// reason: \"busy\" }`". The PAM module (S-008) maps this reason
/// to `PAM_AUTHINFO_UNAVAIL`.
pub const OUTCOME_REASON_BUSY: &str = "busy";

/// Alias for [`OUTCOME_REASON_BUSY`] — the SPEC §3 scope item #7
/// wire text named in S-007's prompt as the canonical constant.
/// Re-exported via `lib.rs` so the PAM mapper's match arm reads
/// `BUSY_REASON` verbatim from the SPEC.
pub const BUSY_REASON: &str = OUTCOME_REASON_BUSY;

/// SPEC §3 scope item #7 queue deadline: "subsequent
/// `ChallengeRequest`s for the same peer wait in a queue with a 1 s
/// deadline". The orchestrator wraps the per-peer
/// `tokio::sync::Semaphore::acquire()` in
/// `tokio::time::timeout(BUSY_QUEUE_DEADLINE, ..)`; on `Elapsed` the
/// outcome is [`ChallengeOutcome::Busy`].
pub const BUSY_QUEUE_DEADLINE: StdDuration = StdDuration::from_millis(1000);

/// SPEC §6 Idempotency cap: "LRU of last 64 nonces per peer". The
/// orchestrator's per-peer [`NonceCache`] uses this as the
/// pop-front threshold — on `insert` when `len() > NONCE_LRU_CAP`
/// the oldest entry is evicted.
pub const NONCE_LRU_CAP: usize = 64;

/// Per-peer LRU of single-use challenge nonces.
///
/// SPEC §6 Idempotency: "every nonce is single-use. A replayed
/// response (same nonce) is rejected by the daemon's in-memory
/// nonce cache (LRU of last 64 nonces per peer)".
///
/// Backed by a `VecDeque<[u8; NONCE_BYTES]>` with linear
/// contains-check; at cap [`NONCE_LRU_CAP`] = 64 the O(64) scan
/// is fast and trivially correct. `insert` appends to the back;
/// when `len() > NONCE_LRU_CAP` after the append, `pop_front`
/// evicts the oldest.
#[derive(Debug, Default)]
pub struct NonceCache {
    nonces: VecDeque<[u8; NONCE_BYTES]>,
}

impl NonceCache {
    /// Construct a fresh empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nonces: VecDeque::with_capacity(NONCE_LRU_CAP + 1),
        }
    }

    /// Return `true` iff `nonce` is in the cache.
    #[must_use]
    pub fn contains(&self, nonce: &[u8; NONCE_BYTES]) -> bool {
        self.nonces.iter().any(|n| n == nonce)
    }

    /// Insert `nonce` at the back of the cache; evict the oldest
    /// entry once the cache exceeds [`NONCE_LRU_CAP`].
    ///
    /// Callers MUST check [`Self::contains`] first; inserting a
    /// duplicate is a logic error (a duplicate nonce is the SPEC
    /// §6 replay signal, not a cache-update event).
    pub fn insert(&mut self, nonce: [u8; NONCE_BYTES]) {
        self.nonces.push_back(nonce);
        if self.nonces.len() > NONCE_LRU_CAP {
            self.nonces.pop_front();
        }
    }
}

/// Typed outcome of [`Orchestrator::issue_challenge`]. Maps 1:1 to
/// the `Response::Challenge { reason }` wire field via the
/// [`ChallengeOutcome::reason_str`] accessor.
#[derive(Debug)]
pub enum ChallengeOutcome {
    /// Verified Ed25519 signature from the phone over the challenge
    /// frame's body bytes. Carries the signature for the
    /// `Response::Challenge { signature }` wire field.
    Ok {
        /// 64-byte Ed25519 signature returned by the phone.
        signature: Signature,
    },
    /// The phone responded but the response payload signalled an
    /// explicit user denial (e.g., cancel on the BiometricPrompt).
    /// Not produced in S-006 — the denial frame format lands with
    /// the phone-side `ChallengeApprovalActivity` row.
    Denied,
    /// The response nonce was previously seen; the LRU rejected the
    /// frame as a replay. Never produced in S-006 — the LRU is
    /// S-007's deliverable. The variant exists so the wire-shape
    /// surface is stable across S-006 → S-007.
    Replay,
    /// The response carried an Ed25519 signature whose strict
    /// verification (`VerifyingKey::verify_strict`) failed.
    BadSignature,
    /// `Peripheral::wait_for_response` reached its deadline without
    /// observing a write on the per-peer response characteristic.
    TimedOut,
    /// `peer_id` was not registered with the orchestrator
    /// (`add_peer` was never called for it).
    UnknownPeer,
    /// Per-peer single-permit semaphore did not admit this caller
    /// within [`BUSY_QUEUE_DEADLINE`]. SPEC §3 scope item #7
    /// backpressure overflow.
    Busy,
    /// Any other failure surfaced by the `Peripheral` backend.
    TransportError(PeripheralError),
}

impl ChallengeOutcome {
    /// Render the outcome as the `Response::Challenge { reason }`
    /// string the PAM mapper consumes.
    #[must_use]
    pub fn reason_str(&self) -> &'static str {
        match self {
            ChallengeOutcome::Ok { .. } => OUTCOME_REASON_OK,
            ChallengeOutcome::Denied => OUTCOME_REASON_DENIED,
            ChallengeOutcome::Replay => OUTCOME_REASON_REPLAY,
            ChallengeOutcome::BadSignature => OUTCOME_REASON_BAD_SIGNATURE,
            ChallengeOutcome::TimedOut => OUTCOME_REASON_RESPONSE_TIMEOUT,
            ChallengeOutcome::UnknownPeer => OUTCOME_REASON_UNKNOWN_PEER,
            ChallengeOutcome::Busy => OUTCOME_REASON_BUSY,
            ChallengeOutcome::TransportError(_) => OUTCOME_REASON_TRANSPORT_ERROR,
        }
    }

    /// Return the 64-byte signature iff `self` is [`ChallengeOutcome::Ok`].
    #[must_use]
    pub fn signature_bytes(&self) -> Option<Vec<u8>> {
        match self {
            ChallengeOutcome::Ok { signature } => Some(signature.to_bytes().to_vec()),
            _ => None,
        }
    }
}

/// Compute the offset from `now` to the next wall-clock second `s`
/// where `s % SECONDS_PER_MINUTE == 0`.
///
/// Pure function so the rotation alignment is unit-testable without
/// freezing the system clock. Returns a full minute (`SECONDS_PER_MINUTE`)
/// when `now` already sits exactly on a minute boundary — the contract
/// is "the FIRST tick of `interval_at(now + offset, ...)` fires at the
/// NEXT minute boundary, never the current one".
///
/// # Errors
///
/// Returns `Duration::from_secs(SECONDS_PER_MINUTE)` (the safe default)
/// when `now` is before `UNIX_EPOCH`; mathematically impossible on a
/// healthy system clock, but the typed branch avoids an `unwrap` per
/// the AGENTS.md non-negotiables.
#[must_use]
pub fn align_to_next_minute(now: SystemTime) -> Duration {
    let secs = match now.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(_) => return Duration::from_secs(SECONDS_PER_MINUTE),
    };
    let remainder = secs % SECONDS_PER_MINUTE;
    let to_next = SECONDS_PER_MINUTE - remainder;
    Duration::from_secs(to_next)
}

/// Compute the integer minute index (`unix_seconds / 60`) for `now`.
/// Returns `0` if `now` predates the unix epoch (impossible in
/// production, see `align_to_next_minute`).
fn minute_index(now: SystemTime) -> i64 {
    let secs = match now.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(_) => return 0,
    };
    // `i64::try_from` cannot fail until year 292277026596; the typed
    // branch keeps clippy happy without an `as i64` cast.
    i64::try_from(secs / SECONDS_PER_MINUTE).unwrap_or(i64::MAX)
}

/// Render the first [`SHORT_UUID_HEX_LEN`] hex chars of `uuid` for
/// the rotation audit line.
fn short_hex(uuid: &Uuid) -> String {
    let mut full = String::new();
    for b in uuid.as_bytes() {
        use std::fmt::Write as _;
        // `write!` to a String only fails if the underlying writer
        // fails, which `String` never does. The typed match keeps
        // clippy happy and obeys the AGENTS.md "no unwrap" rule.
        let _ = write!(full, "{b:02x}");
        if full.len() >= SHORT_UUID_HEX_LEN {
            break;
        }
    }
    full.truncate(SHORT_UUID_HEX_LEN);
    full
}

/// Source of a `reload_bonds` call. Carried on every audit line so
/// `journalctl -t syauth-presenced | grep reload` shows which signal
/// drove which reload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadTrigger {
    /// `SIGHUP` signal received by `runtime::run` (SPEC §3 scope item
    /// #10 — pair flow signals daemon on bond write).
    Sighup,
    /// `Request::Reload` RPC over the Unix socket (SPEC §3 scope item
    /// #10 — operator-driven reload via the daemon's IPC surface).
    Rpc,
    /// `notify::recommended_watcher` fired on `bonds.toml` (SPEC §8
    /// Risks row, belt-and-suspenders for SIGHUP delivery loss).
    Inotify,
    /// In-process test seam (`Orchestrator::signal_reload_for_test`).
    /// Production callers cannot construct this variant by name —
    /// the constructor is `pub(crate)`.
    Test,
}

impl ReloadTrigger {
    /// Render `self` as the `trigger=<kind>` audit-line segment.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ReloadTrigger::Sighup => RELOAD_TRIGGER_SIGHUP,
            ReloadTrigger::Rpc => RELOAD_TRIGGER_RPC,
            ReloadTrigger::Inotify => RELOAD_TRIGGER_INOTIFY,
            ReloadTrigger::Test => RELOAD_TRIGGER_TEST,
        }
    }
}

/// Single reload command pushed onto the orchestrator's `mpsc`
/// queue. Carries the trigger so the audit line records the cause.
#[derive(Debug, Clone, Copy)]
pub struct ReloadCommand {
    /// Source of the reload (SIGHUP / RPC / inotify / test).
    pub trigger: ReloadTrigger,
}

/// Long-lived rotation + reload driver for N bonded peers.
///
/// Owns the `Peripheral` handle the daemon holds across many PAM
/// calls and a tokio `interval_at` aligned to the next wall-clock
/// minute boundary. On every tick the orchestrator publishes the
/// union of every registered peer's current minute UUID. On every
/// `ReloadCommand` the orchestrator re-loads the `BondStore` from
/// disk, diffs the result against the live peer set, and emits the
/// minimal `add_peer` / `remove_peer` calls before re-publishing the
/// fresh UUID union.
pub struct Orchestrator {
    peripheral: Arc<dyn Peripheral + Send + Sync>,
    /// In-memory mirror of the live peer set keyed by `peer_id`.
    /// `BTreeMap` so iteration order is deterministic across runs
    /// (the test assertion on `peers_in_order()` matches the iteration
    /// the rotation tick uses).
    peers: tokio::sync::Mutex<BTreeMap<String, PeerEntry>>,
    /// Path to the bonds.toml the reload pipeline re-reads.
    bonds_file: PathBuf,
    /// Path to the keys directory (`<keys_dir>/<peer_id>.bin`).
    keys_dir: PathBuf,
    /// Sender side of the reload mpsc queue. Cloned out via
    /// `reload_sender()` to the server crate and the runtime's signal
    /// handler.
    reload_tx: mpsc::Sender<ReloadCommand>,
    /// Receiver side of the reload mpsc queue. Owned by `run`.
    reload_rx: tokio::sync::Mutex<Option<mpsc::Receiver<ReloadCommand>>>,
    /// Audit-log appender. `Mutex<Option<_>>` so the orchestrator
    /// can hold a `None` slot at construction time (rotation-only
    /// tests) and have an `AuditLog` attached later via
    /// [`Orchestrator::attach_audit_log`].
    audit_log: TokioMutex<Option<AuditLog>>,
    /// `tokio::time::Instant` of the next minute-tick.
    start: Instant,
}

/// Per-peer in-memory record. S-006 extends the S-005 layout with
/// the bond's `phone_pubkey` so `Orchestrator::issue_challenge` can
/// verify the phone's Ed25519 signature without re-reading
/// `bonds.toml` on every PAM call. S-007 layers a per-peer
/// [`NonceCache`] (SPEC §6 Idempotency LRU) and a
/// [`tokio::sync::Semaphore`] single-permit gate (SPEC §3 scope
/// item #7 at-most-one-in-flight backpressure) on top.
struct PeerEntry {
    bond_key: [u8; BOND_KEY_BYTES],
    /// 32-byte Ed25519 public key of the bonded phone (the
    /// `phone_pubkey` populated by DEV-002's pair flow). Held as
    /// raw bytes so the construction cost (`VerifyingKey::from_bytes`)
    /// is paid once per `add_peer` rather than once per challenge.
    /// `None` for orchestrators constructed via the S-004
    /// single-bond shim before S-006 (rotation-only tests do not
    /// carry a pubkey).
    phone_pubkey: Option<[u8; PUBKEY_LEN]>,
    /// Per-peer LRU of single-use nonces. SPEC §6 Idempotency.
    /// Held behind a `tokio::sync::Mutex` because the
    /// `issue_challenge` future awaits across the cache lookup +
    /// insert (the audit append is async).
    nonce_cache: Arc<TokioMutex<NonceCache>>,
    /// Single-permit semaphore gating concurrent
    /// `issue_challenge` calls for this peer. SPEC §3 scope item
    /// #7: "at most one in-flight challenge per peer".
    /// `Arc<Semaphore>` so `acquire_owned()` can be called without
    /// holding the peer-map lock across the await.
    challenge_slot: Arc<Semaphore>,
    /// Per-peer S-017 liveness markers. Updated by
    /// `issue_challenge` (challenge timestamp) and
    /// `acquire_challenge_slot` (connect-proxy timestamp). Read by
    /// [`Orchestrator::peers_snapshot`].
    liveness: Arc<TokioMutex<PeerLiveness>>,
}

impl PeerEntry {
    /// Build a fresh `PeerEntry` with an empty nonce cache and a
    /// vacant single-permit semaphore.
    fn new(bond_key: [u8; BOND_KEY_BYTES], phone_pubkey: Option<[u8; PUBKEY_LEN]>) -> Self {
        Self {
            bond_key,
            phone_pubkey,
            nonce_cache: Arc::new(TokioMutex::new(NonceCache::new())),
            challenge_slot: Arc::new(Semaphore::new(1)),
            liveness: Arc::new(TokioMutex::new(PeerLiveness::default())),
        }
    }
}

/// Per-peer liveness timestamps surfaced by
/// [`Orchestrator::peers_snapshot`]. Both fields are
/// `Option<SystemTime>` so the cold-start case ("daemon up but no
/// challenge yet for this peer") renders as `None` (which the wire
/// layer translates to a JSON `null`).
///
/// Backed by `SystemTime` (not `Instant`) so the renderer can
/// compute `<duration> ago` against the snapshot wall-clock time
/// without a separate `Instant` reference epoch.
#[derive(Debug, Clone, Default)]
struct PeerLiveness {
    /// Wall-clock time of the most recent `issue_challenge` for
    /// this peer (any outcome). `None` until the first challenge.
    last_challenge_at: Option<SystemTime>,
    /// Wall-clock time of the most recent per-peer challenge-slot
    /// acquisition (the closest proxy the daemon owns to a
    /// "connect" event — see SPEC §3 scope item #24 + the journey
    /// doc).
    last_connect_at: Option<SystemTime>,
}

/// Snapshot of a peer's shared state captured by
/// [`Orchestrator::lookup_peer`] so the challenge body does not
/// hold the peer-map lock across awaits.
struct PeerState {
    phone_pubkey: [u8; PUBKEY_LEN],
    bond_key: [u8; BOND_KEY_BYTES],
    nonce_cache: Arc<TokioMutex<NonceCache>>,
    challenge_slot: Arc<Semaphore>,
    liveness: Arc<TokioMutex<PeerLiveness>>,
}

impl Orchestrator {
    /// Construct a fresh orchestrator carrying a single bonded peer.
    ///
    /// Backwards-compatible shape with S-004 (`tests/rotation.rs`
    /// constructs the orchestrator this way). Multi-peer construction
    /// goes through [`Orchestrator::with_peers`].
    #[must_use]
    pub fn new(peripheral: Arc<dyn Peripheral + Send + Sync>, bond: Bond, bond_key: [u8; BOND_KEY_BYTES], start: Instant) -> Self {
        Self::with_peers(peripheral, vec![(bond, bond_key)], PathBuf::new(), PathBuf::new(), start)
    }

    /// Construct a fresh orchestrator carrying N bonded peers.
    ///
    /// `bonds_file` + `keys_dir` are read on every `ReloadCommand`
    /// receipt to compute the new peer set. Pass empty `PathBuf`s
    /// when reloads are unused (e.g. the S-004 rotation test).
    /// The audit log is left unset; callers that need the SPEC §3
    /// scope item #8 audit trail must use
    /// [`Orchestrator::with_peers_and_audit`] (or wire one in via
    /// [`Orchestrator::attach_audit_log`]).
    #[must_use]
    pub fn with_peers(
        peripheral: Arc<dyn Peripheral + Send + Sync>,
        peers: Vec<(Bond, [u8; BOND_KEY_BYTES])>,
        bonds_file: PathBuf,
        keys_dir: PathBuf,
        start: Instant,
    ) -> Self {
        Self::with_peers_and_audit(peripheral, peers, bonds_file, keys_dir, start, None)
    }

    /// Construct an orchestrator carrying N bonded peers and an
    /// optional audit-log appender. Production callers pass
    /// `Some(AuditLog::open("/var/lib/syauth/last.log")?)`; tests pass
    /// a tempdir-local path so re-runs do not leak files into
    /// `/var/lib/`.
    #[must_use]
    pub fn with_peers_and_audit(
        peripheral: Arc<dyn Peripheral + Send + Sync>,
        peers: Vec<(Bond, [u8; BOND_KEY_BYTES])>,
        bonds_file: PathBuf,
        keys_dir: PathBuf,
        start: Instant,
        audit_log: Option<AuditLog>,
    ) -> Self {
        let mut map = BTreeMap::new();
        for (bond, bond_key) in peers {
            map.insert(bond.peer_id.clone(), PeerEntry::new(bond_key, Some(bond.pubkey)));
        }
        let (reload_tx, reload_rx) = mpsc::channel(RELOAD_CHANNEL_CAPACITY);
        Self {
            peripheral,
            peers: tokio::sync::Mutex::new(map),
            bonds_file,
            keys_dir,
            reload_tx,
            reload_rx: tokio::sync::Mutex::new(Some(reload_rx)),
            audit_log: TokioMutex::new(audit_log),
            start,
        }
    }

    /// Attach an audit-log appender after construction. Returns the
    /// previously-installed log (if any). Useful for tests that
    /// construct the orchestrator first and only later open the
    /// audit file.
    pub async fn attach_audit_log(&self, log: AuditLog) -> Option<AuditLog> {
        let mut slot = self.audit_log.lock().await;
        slot.replace(log)
    }

    /// Clone the reload-channel sender. The runtime hands clones to
    /// the SIGHUP handler, the Unix-socket RPC server, and the inotify
    /// watcher; every source pushes `ReloadCommand` onto the same
    /// queue.
    #[must_use]
    pub fn reload_sender(&self) -> mpsc::Sender<ReloadCommand> {
        self.reload_tx.clone()
    }

    /// Drive the rotation + reload loop until `shutdown` fires.
    ///
    /// On entry the orchestrator publishes the CURRENT minute's UUID
    /// union immediately so the operator's first `sudo` sees the
    /// correct advertisement even before the first wall-clock minute
    /// boundary rolls over. Subsequent rotations are driven by an
    /// `interval_at(start, SECONDS_PER_MINUTE)` aligned to the next
    /// minute boundary at construction time. Reload commands drain
    /// the mpsc queue serially.
    pub async fn run(self: Arc<Self>, mut shutdown: oneshot::Receiver<()>) {
        let mut reload_rx = match self.take_reload_rx().await {
            Some(rx) => rx,
            None => {
                tracing::warn!(target: ROTATION_LOG_TARGET, "orchestrator run() called twice; second call is a no-op");
                return;
            }
        };
        // Publish the current minute's UUID union immediately so the
        // advertisement is correct before the first wall-clock tick.
        self.rotate_once(SystemTime::now()).await;

        let mut interval = interval_at(self.start, Duration::from_secs(SECONDS_PER_MINUTE));
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => {
                    tracing::debug!(target: ROTATION_LOG_TARGET, "orchestrator shutting down");
                    return;
                }
                _ = interval.tick() => {
                    self.rotate_once(SystemTime::now()).await;
                }
                Some(cmd) = reload_rx.recv() => {
                    self.run_reload(cmd.trigger).await;
                }
            }
        }
    }

    /// Take the reload receiver out of its slot (the field is
    /// `Option<Receiver>` so the trait-object orchestrator can hand
    /// ownership of the receiver to its own `run` future without
    /// taking `&mut self`).
    async fn take_reload_rx(&self) -> Option<mpsc::Receiver<ReloadCommand>> {
        self.reload_rx.lock().await.take()
    }

    /// Compute and publish the rotating UUID union for the wall-clock
    /// minute that contains `now`. Emits the SPEC §3 #22 audit line
    /// per peer on success; logs `warn` on a `Peripheral` backend
    /// failure.
    async fn rotate_once(&self, now: SystemTime) {
        let minute = minute_index(now);
        let snapshot = self.snapshot_peers().await;
        let union = build_uuid_union(&snapshot, minute);
        match self.peripheral.set_session_uuids(union.clone()).await {
            Ok(()) => {
                for (peer_id, _) in &snapshot {
                    let uuid_bytes = session_uuid_for(snapshot_key(&snapshot, peer_id), minute);
                    let uuid = Uuid::from_bytes(uuid_bytes);
                    let short = short_hex(&uuid);
                    tracing::info!(
                        target: ROTATION_LOG_TARGET,
                        "rotated id={peer_id} minute={minute} uuid={short}"
                    );
                }
            }
            Err(err) => {
                rotation_warn(&err, minute);
            }
        }
    }

    /// Capture an ordered snapshot of the live peer set for one
    /// rotation tick. The lock is released before the (potentially
    /// awaiting) `set_session_uuids` call so a concurrent reload does
    /// not deadlock against the tick.
    async fn snapshot_peers(&self) -> Vec<(String, [u8; BOND_KEY_BYTES])> {
        let guard = self.peers.lock().await;
        guard.iter().map(|(id, entry)| (id.clone(), entry.bond_key)).collect()
    }

    /// Re-read the `BondStore` from disk, diff against the live peer
    /// set, emit the minimal `add_peer` / `remove_peer` calls, and
    /// publish the fresh UUID union. Audits the trigger + before /
    /// after peer counts on `ROTATION_LOG_TARGET`.
    async fn run_reload(&self, trigger: ReloadTrigger) {
        let store = match BondStore::load(&self.bonds_file) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(
                    target: ROTATION_LOG_TARGET,
                    "reload skipped trigger={} reason=bond_store_load_failed error={err}",
                    trigger.as_str()
                );
                return;
            }
        };
        self.reload_bonds_with_trigger(&store, trigger).await;
    }

    /// Public entry point for the diff: take a `BondStore` snapshot,
    /// reconcile the orchestrator's peer set, publish the fresh union.
    /// Audits `trigger=test` because direct callers are tests or
    /// other in-process orchestration code.
    pub async fn reload_bonds(&self, store: &BondStore) {
        self.reload_bonds_with_trigger(store, ReloadTrigger::Test).await;
    }

    async fn reload_bonds_with_trigger(&self, store: &BondStore, trigger: ReloadTrigger) {
        let new_set: HashMap<String, [u8; BOND_KEY_BYTES]> = self.compute_new_set(store);
        let (to_add, to_remove) = self.compute_diff(&new_set).await;
        let peers_before = self.peers.lock().await.len();
        for peer_id in &to_remove {
            self.do_remove_peer(peer_id).await;
        }
        for peer_id in &to_add {
            if let Some(key) = new_set.get(peer_id) {
                self.do_add_peer(store, peer_id, key).await;
            }
        }
        let peers_after = self.peers.lock().await.len();
        tracing::info!(
            target: ROTATION_LOG_TARGET,
            "reload trigger={} peers_before={peers_before} peers_after={peers_after}",
            trigger.as_str()
        );
        self.rotate_once(SystemTime::now()).await;
    }

    /// Build the new peer set from a `BondStore` snapshot, filtering
    /// revoked entries. The bond_key for each peer is loaded from
    /// `<keys_dir>/<peer_id>.bin`. Peers whose key file is missing or
    /// malformed are skipped with a warn.
    fn compute_new_set(&self, store: &BondStore) -> HashMap<String, [u8; BOND_KEY_BYTES]> {
        let mut out = HashMap::new();
        for bond in store.list().iter().filter(|b| !b.is_revoked()) {
            match load_bond_key(&self.keys_dir, &bond.peer_id) {
                Ok(key) => {
                    out.insert(bond.peer_id.clone(), key);
                }
                Err(reason) => {
                    tracing::warn!(
                        target: ROTATION_LOG_TARGET,
                        "reload skipped peer={} reason=key_load_failed error={reason}",
                        bond.peer_id
                    );
                }
            }
        }
        out
    }

    /// Compute `(to_add, to_remove)` between the new bond set and
    /// the live peer set. `to_add = new - current`, `to_remove =
    /// current - new`.
    async fn compute_diff(&self, new_set: &HashMap<String, [u8; BOND_KEY_BYTES]>) -> (Vec<String>, Vec<String>) {
        let guard = self.peers.lock().await;
        let current: HashSet<&String> = guard.keys().collect();
        let new_keys: HashSet<&String> = new_set.keys().collect();
        let to_add: Vec<String> = new_keys.difference(&current).map(|s| (*s).clone()).collect();
        let to_remove: Vec<String> = current.difference(&new_keys).map(|s| (*s).clone()).collect();
        (to_add, to_remove)
    }

    async fn do_remove_peer(&self, peer_id: &str) {
        match self.peripheral.remove_peer(peer_id).await {
            Ok(()) => {
                let mut guard = self.peers.lock().await;
                guard.remove(peer_id);
                tracing::info!(target: ROTATION_LOG_TARGET, "reload removed peer={peer_id}");
            }
            Err(err) => {
                tracing::warn!(
                    target: ROTATION_LOG_TARGET,
                    "reload remove_peer failed peer={peer_id} error={err}"
                );
            }
        }
    }

    async fn do_add_peer(&self, store: &BondStore, peer_id: &str, key: &[u8; BOND_KEY_BYTES]) {
        let phone_pubkey = store.list().iter().find(|b| b.peer_id == peer_id).map(|b| b.pubkey);
        let phone_pubkey = match phone_pubkey {
            Some(pk) => pk,
            None => return,
        };
        match self.peripheral.add_peer(peer_id, key).await {
            Ok(()) => {
                let mut guard = self.peers.lock().await;
                guard.insert(peer_id.to_owned(), PeerEntry::new(*key, Some(phone_pubkey)));
                tracing::info!(target: ROTATION_LOG_TARGET, "reload added peer={peer_id}");
            }
            Err(err) => {
                tracing::warn!(
                    target: ROTATION_LOG_TARGET,
                    "reload add_peer failed peer={peer_id} error={err}"
                );
            }
        }
    }

    /// Drive one challenge transaction against `peer_id`.
    ///
    /// Implements the SPEC §6 state-model transition `Idle →
    /// ChallengeIssued{nonce, t_start} → ChallengeVerified{ok} |
    /// TimedOut | TransportFailed`. Returns a typed
    /// [`ChallengeOutcome`] the server's dispatcher maps onto the
    /// `Response::Challenge { ok, signature, reason }` wire shape.
    ///
    /// Every outcome (including `UnknownPeer`) appends one line to
    /// the audit log if one is attached; the audit append is
    /// best-effort — a log-write failure does NOT change the
    /// returned outcome (the SPEC §8 Risks row accepts losing the
    /// last 32 records on power loss; losing one record because the
    /// disk is full is a strictly weaker failure).
    pub async fn issue_challenge(&self, peer_id: &str, deadline: StdDuration) -> ChallengeOutcome {
        let t_start_ms = epoch_millis(SystemTime::now());
        let peer_state = match self.lookup_peer(peer_id).await {
            Some(s) => s,
            None => {
                self.audit_outcome(peer_id, ZERO_NONCE_HEX, t_start_ms, OUTCOME_REASON_UNKNOWN_PEER)
                    .await;
                return ChallengeOutcome::UnknownPeer;
            }
        };

        let permit = match self.acquire_challenge_slot(&peer_state.challenge_slot).await {
            Some(p) => p,
            None => {
                let t_end_ms = epoch_millis(SystemTime::now());
                self.audit_at(peer_id, ZERO_NONCE_HEX, t_start_ms, t_end_ms, OUTCOME_REASON_BUSY)
                    .await;
                return ChallengeOutcome::Busy;
            }
        };

        stamp_liveness(&peer_state.liveness, SystemTime::now()).await;

        let mut nonce = [0u8; NONCE_BYTES];
        if let Err(err) = getrandom::fill(&mut nonce) {
            let t_end_ms = epoch_millis(SystemTime::now());
            self.audit_at(peer_id, ZERO_NONCE_HEX, t_start_ms, t_end_ms, OUTCOME_REASON_TRANSPORT_ERROR)
                .await;
            drop(permit);
            return ChallengeOutcome::TransportError(PeripheralError::Backend {
                reason: format!("nonce rng: {err}"),
            });
        }
        let outcome = self.run_challenge(peer_id, nonce, deadline, &peer_state, t_start_ms).await;
        drop(permit);
        outcome
    }

    /// Test-only entry point that bypasses [`getrandom::fill`] so a
    /// test can force a deterministic nonce collision against the
    /// per-peer [`NonceCache`]. Production callers go through
    /// [`Self::issue_challenge`].
    #[doc(hidden)]
    pub async fn issue_challenge_with_nonce(&self, peer_id: &str, nonce: [u8; NONCE_BYTES], deadline: StdDuration) -> ChallengeOutcome {
        let t_start_ms = epoch_millis(SystemTime::now());
        let peer_state = match self.lookup_peer(peer_id).await {
            Some(s) => s,
            None => {
                self.audit_outcome(peer_id, ZERO_NONCE_HEX, t_start_ms, OUTCOME_REASON_UNKNOWN_PEER)
                    .await;
                return ChallengeOutcome::UnknownPeer;
            }
        };
        let permit = match self.acquire_challenge_slot(&peer_state.challenge_slot).await {
            Some(p) => p,
            None => {
                let t_end_ms = epoch_millis(SystemTime::now());
                self.audit_at(peer_id, ZERO_NONCE_HEX, t_start_ms, t_end_ms, OUTCOME_REASON_BUSY)
                    .await;
                return ChallengeOutcome::Busy;
            }
        };
        stamp_liveness(&peer_state.liveness, SystemTime::now()).await;
        let outcome = self.run_challenge(peer_id, nonce, deadline, &peer_state, t_start_ms).await;
        drop(permit);
        outcome
    }

    /// Acquire the per-peer single-permit semaphore with a
    /// [`BUSY_QUEUE_DEADLINE`] budget. Returns `None` on timeout so
    /// the caller maps to [`ChallengeOutcome::Busy`].
    async fn acquire_challenge_slot(&self, slot: &Arc<Semaphore>) -> Option<tokio::sync::OwnedSemaphorePermit> {
        let slot = Arc::clone(slot);
        match tokio::time::timeout(BUSY_QUEUE_DEADLINE, slot.acquire_owned()).await {
            Ok(Ok(permit)) => Some(permit),
            // The semaphore is never closed in the orchestrator's
            // lifetime; the `AcquireError` arm is unreachable on the
            // happy path. Treating it as Busy keeps the outcome
            // surface stable without `unwrap`.
            Ok(Err(_)) | Err(_) => None,
        }
    }

    /// Body of the challenge state machine. Runs after the per-peer
    /// semaphore admits the caller and a nonce is in hand. The
    /// permit is owned by the caller (`issue_challenge` /
    /// `issue_challenge_with_nonce`) and dropped after this future
    /// returns.
    async fn run_challenge(
        &self,
        peer_id: &str,
        nonce: [u8; NONCE_BYTES],
        deadline: StdDuration,
        peer_state: &PeerState,
        t_start_ms: u128,
    ) -> ChallengeOutcome {
        let nonce_hex = hex::encode(nonce);
        let mut challenge = Frame {
            version: SYAUTH_WIRE_VERSION_V1,
            nonce,
            payload: Vec::new(),
            tag: [0u8; TAG_LEN],
        };
        // Compute the BLAKE3-keyed-hash MAC over the frame body so the
        // phone's `verifyChallengeFrame(bond_key, frame)` accepts it.
        // S-006's stub left tag=zero; the phone correctly rejects that
        // as "frame verify failed".
        match challenge.body_bytes() {
            Ok(body) => challenge.tag = syauth_core::compute_tag(&peer_state.bond_key, &body),
            Err(err) => {
                let t_end_ms = epoch_millis(SystemTime::now());
                self.audit_at(peer_id, &nonce_hex, t_start_ms, t_end_ms, OUTCOME_REASON_TRANSPORT_ERROR)
                    .await;
                return ChallengeOutcome::TransportError(PeripheralError::Backend {
                    reason: format!("challenge body_bytes: {err}"),
                });
            }
        }
        let mut encoded: Vec<u8> = Vec::new();
        if let Err(err) = challenge.encode(&mut encoded) {
            let t_end_ms = epoch_millis(SystemTime::now());
            self.audit_at(peer_id, &nonce_hex, t_start_ms, t_end_ms, OUTCOME_REASON_TRANSPORT_ERROR)
                .await;
            return ChallengeOutcome::TransportError(PeripheralError::Backend {
                reason: format!("encode challenge frame: {err}"),
            });
        }
        if let Err(err) = self.peripheral.notify_challenge(peer_id, &encoded).await {
            let t_end_ms = epoch_millis(SystemTime::now());
            let reason = challenge_outcome_for_transport(&err).reason_str();
            self.audit_at(peer_id, &nonce_hex, t_start_ms, t_end_ms, reason).await;
            return challenge_outcome_for_transport(&err);
        }
        let response_bytes = match self.peripheral.wait_for_response(peer_id, deadline).await {
            Ok(b) => b,
            Err(PeripheralError::ResponseTimeout { .. }) => {
                let t_end_ms = epoch_millis(SystemTime::now());
                self.audit_at(peer_id, &nonce_hex, t_start_ms, t_end_ms, OUTCOME_REASON_RESPONSE_TIMEOUT)
                    .await;
                return ChallengeOutcome::TimedOut;
            }
            Err(err) => {
                let t_end_ms = epoch_millis(SystemTime::now());
                let outcome = challenge_outcome_for_transport(&err);
                self.audit_at(peer_id, &nonce_hex, t_start_ms, t_end_ms, outcome.reason_str()).await;
                return outcome;
            }
        };
        let signature = match parse_signature(&response_bytes) {
            Some(s) => s,
            None => {
                let t_end_ms = epoch_millis(SystemTime::now());
                self.audit_at(peer_id, &nonce_hex, t_start_ms, t_end_ms, OUTCOME_REASON_BAD_SIGNATURE)
                    .await;
                return ChallengeOutcome::BadSignature;
            }
        };
        let verifying_key = match VerifyingKey::from_bytes(&peer_state.phone_pubkey) {
            Ok(vk) => vk,
            Err(_) => {
                let t_end_ms = epoch_millis(SystemTime::now());
                self.audit_at(peer_id, &nonce_hex, t_start_ms, t_end_ms, OUTCOME_REASON_BAD_SIGNATURE)
                    .await;
                return ChallengeOutcome::BadSignature;
            }
        };
        if let Err(_e) = verify_frame(&verifying_key, &challenge, &signature) {
            let t_end_ms = epoch_millis(SystemTime::now());
            self.audit_at(peer_id, &nonce_hex, t_start_ms, t_end_ms, OUTCOME_REASON_BAD_SIGNATURE)
                .await;
            return ChallengeOutcome::BadSignature;
        }
        // Post-verify replay check. SPEC §6 Idempotency: a response
        // whose nonce was already seen for this peer is a replay.
        let mut cache = peer_state.nonce_cache.lock().await;
        if cache.contains(&nonce) {
            let t_end_ms = epoch_millis(SystemTime::now());
            drop(cache);
            self.audit_at(peer_id, &nonce_hex, t_start_ms, t_end_ms, OUTCOME_REASON_REPLAY)
                .await;
            return ChallengeOutcome::Replay;
        }
        cache.insert(nonce);
        drop(cache);
        let t_end_ms = epoch_millis(SystemTime::now());
        self.audit_at(peer_id, &nonce_hex, t_start_ms, t_end_ms, OUTCOME_REASON_OK).await;
        ChallengeOutcome::Ok { signature }
    }

    /// Look up the cached per-peer state needed by `issue_challenge`.
    /// `None` if the peer was never registered, or if the entry
    /// exists but carries no `phone_pubkey` (the S-004 single-bond
    /// shim path that `tests/rotation.rs` exercises).
    async fn lookup_peer(&self, peer_id: &str) -> Option<PeerState> {
        let guard = self.peers.lock().await;
        let entry = guard.get(peer_id)?;
        let pubkey = entry.phone_pubkey?;
        Some(PeerState {
            phone_pubkey: pubkey,
            bond_key: entry.bond_key,
            nonce_cache: Arc::clone(&entry.nonce_cache),
            challenge_slot: Arc::clone(&entry.challenge_slot),
            liveness: Arc::clone(&entry.liveness),
        })
    }

    /// Audit a transaction outcome with a `t_end_ms` captured at
    /// the call site. Best-effort: a log-write failure logs a warn
    /// on `ROTATION_LOG_TARGET` and the outcome path continues.
    async fn audit_at(&self, peer_id: &str, nonce_hex: &str, t_start_ms: u128, t_end_ms: u128, reason: &str) {
        let record = AuditRecord {
            peer_id,
            nonce_hex,
            t_start_ms,
            t_end_ms,
            outcome: reason,
            reason,
        };
        let mut slot = self.audit_log.lock().await;
        if let Some(log) = slot.as_mut()
            && let Err(err) = log.append(&record)
        {
            tracing::warn!(target: ROTATION_LOG_TARGET, error = %err, "audit append failed");
        }
        tracing::info!(
            target: ROTATION_LOG_TARGET,
            "tx peer={peer_id} outcome={reason} t_start_ms={t_start_ms} t_end_ms={t_end_ms}"
        );
    }

    /// Audit an early-return outcome where `t_end_ms == t_start_ms`
    /// (e.g., `UnknownPeer` before the notify round-trip).
    async fn audit_outcome(&self, peer_id: &str, nonce_hex: &str, t_start_ms: u128, reason: &str) {
        self.audit_at(peer_id, nonce_hex, t_start_ms, t_start_ms, reason).await;
    }

    /// Take a per-peer liveness snapshot for the `Response::Status`
    /// wire frame. Source-of-truth for the live peer set so a reload
    /// regression that drops a bond from the orchestrator's map
    /// surfaces immediately in the `syauth status` table.
    ///
    /// Snapshot is taken at wall-clock `now`; the
    /// `current_session_uuid` is derived from
    /// `session_uuid_for(bond_key, minute_index(now))`. The
    /// `in_flight_challenges` count comes from
    /// `Semaphore::available_permits()` (`0` available → `1` in
    /// flight; `1` available → `0` in flight; per-peer semaphore
    /// has exactly one permit per SPEC §3 scope item #7).
    ///
    /// Roadmap: `specs/unlock-proximity/ROADMAP.md` Step S-017.
    /// Journey: `specs/journeys/JOURNEY-S-017-status-extension.md`.
    pub async fn peers_snapshot(&self) -> Vec<crate::rpc::PeerStatus> {
        let now = SystemTime::now();
        let minute = minute_index(now);
        let guard = self.peers.lock().await;
        let mut rows: Vec<crate::rpc::PeerStatus> = Vec::with_capacity(guard.len());
        for (peer_id, entry) in guard.iter() {
            let liveness = entry.liveness.lock().await.clone();
            let in_flight = challenge_slot_in_flight(&entry.challenge_slot);
            let uuid_bytes = session_uuid_for(&entry.bond_key, minute);
            rows.push(crate::rpc::PeerStatus {
                peer_id: peer_id.clone(),
                last_challenge_ms_ago: ms_since(now, liveness.last_challenge_at),
                last_connect_ms_ago: ms_since(now, liveness.last_connect_at),
                current_session_uuid: Uuid::from_bytes(uuid_bytes),
                in_flight_challenges: in_flight,
            });
        }
        rows
    }
}

/// Audit-line `nonce_hex` value used when a challenge fails before
/// the nonce is generated (e.g., `UnknownPeer`). 32 zero hex chars
/// matches the production line shape so a `awk -F,` pipeline does
/// not have to special-case the column width.
const ZERO_NONCE_HEX: &str = "00000000000000000000000000000000";

/// Stamp both `last_challenge_at` and `last_connect_at` on a
/// per-peer liveness slot. Called once per challenge transaction
/// after the per-peer semaphore permit is acquired. The two
/// timestamps coincide because the orchestrator does not (yet)
/// distinguish a GATT-level connect from a slot-acquisition event
/// — see SPEC §3 scope item #24 + JOURNEY-S-017 Note A.
async fn stamp_liveness(slot: &Arc<TokioMutex<PeerLiveness>>, now: SystemTime) {
    let mut guard = slot.lock().await;
    guard.last_challenge_at = Some(now);
    guard.last_connect_at = Some(now);
}

/// Compute `now - then` in whole milliseconds. `None` if `then` is
/// `None` or `then` is somehow after `now` (clock-skew defence).
fn ms_since(now: SystemTime, then: Option<SystemTime>) -> Option<u64> {
    let then = then?;
    let dur = now.duration_since(then).ok()?;
    Some(u64::try_from(dur.as_millis()).unwrap_or(u64::MAX))
}

/// Read a single-permit semaphore as an in-flight counter:
/// `0 available → 1 in flight`, `1 available → 0 in flight`. SPEC
/// §3 scope item #7: per-peer `Semaphore(1)` bounds the count to
/// `{0, 1}`.
fn challenge_slot_in_flight(slot: &Arc<Semaphore>) -> u32 {
    let available = slot.available_permits();
    if available == 0 { 1 } else { 0 }
}

/// Convert a `SystemTime` to whole epoch milliseconds. Returns `0`
/// for pre-epoch values (impossible on a healthy clock).
fn epoch_millis(t: SystemTime) -> u128 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// Map a `PeripheralError` from `notify_challenge` or
/// `wait_for_response` to the corresponding [`ChallengeOutcome`].
fn challenge_outcome_for_transport(err: &PeripheralError) -> ChallengeOutcome {
    match err {
        PeripheralError::UnknownPeer { .. } => ChallengeOutcome::UnknownPeer,
        PeripheralError::ResponseTimeout { .. } => ChallengeOutcome::TimedOut,
        other => ChallengeOutcome::TransportError(PeripheralError::Backend { reason: other.to_string() }),
    }
}

/// Parse `bytes` as an Ed25519 [`Signature`]. Returns `None` if the
/// length is wrong or if `Signature::from_slice` rejects the bytes.
fn parse_signature(bytes: &[u8]) -> Option<Signature> {
    if bytes.len() != SIGNATURE_LEN {
        return None;
    }
    Signature::from_slice(bytes).ok()
}

/// Build the union of `session_uuid_for(bond_key, minute)` across
/// every peer in `snapshot`.
fn build_uuid_union(snapshot: &[(String, [u8; BOND_KEY_BYTES])], minute: i64) -> HashSet<Uuid> {
    snapshot
        .iter()
        .map(|(_id, key)| Uuid::from_bytes(session_uuid_for(key, minute)))
        .collect()
}

/// Look up the bond_key for `peer_id` inside a captured snapshot.
/// Returns a reference to a zero-array when the peer is missing
/// (callers must already have validated membership; the zero fallback
/// keeps the function total without an `unwrap`).
fn snapshot_key<'a>(snapshot: &'a [(String, [u8; BOND_KEY_BYTES])], peer_id: &str) -> &'a [u8; BOND_KEY_BYTES] {
    static ZERO: [u8; BOND_KEY_BYTES] = [0u8; BOND_KEY_BYTES];
    snapshot
        .iter()
        .find_map(|(id, key)| if id == peer_id { Some(key) } else { None })
        .unwrap_or(&ZERO)
}

/// Per-peer bond-key file extension under `<keys_dir>/<peer_id>.bin`.
/// Mirrored from `runtime::BOND_KEY_FILE_EXT` so the orchestrator does
/// not pull the runtime module into its dependency graph.
const BOND_KEY_FILE_EXT: &str = ".bin";

/// Read `<keys_dir>/<peer_id>.bin`, validate length, return the
/// 32-byte bond key. Mirrors the runtime crate's `load_bond_key`
/// shape but returns a `String` reason so callers can warn-log the
/// human-readable cause.
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

/// Emit a `warn` line on rotation failure. Extracted into a free
/// function so the call site stays one line and the test surface
/// for the success path stays uncluttered.
fn rotation_warn(err: &PeripheralError, minute: i64) {
    tracing::warn!(
        target: ROTATION_LOG_TARGET,
        "rotation failed minute={minute} error={err}"
    );
}

#[cfg(test)]
mod tests {
    // Journey: specs/journeys/JOURNEY-S-004-session-uuid-rotation.md
    // Journey: specs/journeys/JOURNEY-S-005-multi-peer-bonds-reload.md
    use super::*;

    /// Pure-function test: at a minute boundary the next-minute
    /// offset is a full minute.
    #[test]
    fn align_to_next_minute_at_minute_mark_returns_full_minute() {
        let at_mark = UNIX_EPOCH + StdDuration::from_secs(60 * 1_000_000);
        let offset = align_to_next_minute(at_mark);
        assert_eq!(offset, Duration::from_secs(SECONDS_PER_MINUTE));
    }

    /// Pure-function test: mid-minute the offset is the remainder
    /// to the next minute.
    #[test]
    fn align_to_next_minute_mid_minute_returns_remainder() {
        let mid = UNIX_EPOCH + StdDuration::from_secs(60 * 1_000_000 + 17);
        let offset = align_to_next_minute(mid);
        assert_eq!(offset, Duration::from_secs(43));
    }

    /// Pure-function test: one second before the next minute the
    /// offset is one second.
    #[test]
    fn align_to_next_minute_one_second_before_mark_returns_one() {
        let almost = UNIX_EPOCH + StdDuration::from_secs(60 * 1_000_000 + 59);
        let offset = align_to_next_minute(almost);
        assert_eq!(offset, Duration::from_secs(1));
    }

    /// `short_hex` truncates to exactly [`SHORT_UUID_HEX_LEN`] chars.
    #[test]
    fn short_hex_truncates_to_constant_length() {
        let u = Uuid::from_bytes([0xab; 16]);
        let h = short_hex(&u);
        assert_eq!(h.len(), SHORT_UUID_HEX_LEN);
        assert_eq!(h, "abababab");
    }

    /// `minute_index` floor-divides the unix-epoch seconds.
    #[test]
    fn minute_index_floors_unix_seconds() {
        let t = UNIX_EPOCH + StdDuration::from_secs(125);
        assert_eq!(minute_index(t), 2);
    }

    /// `ReloadTrigger::as_str` returns the canonical audit-line tag.
    #[test]
    fn reload_trigger_renders_to_canonical_tag() {
        assert_eq!(ReloadTrigger::Sighup.as_str(), RELOAD_TRIGGER_SIGHUP);
        assert_eq!(ReloadTrigger::Rpc.as_str(), RELOAD_TRIGGER_RPC);
        assert_eq!(ReloadTrigger::Inotify.as_str(), RELOAD_TRIGGER_INOTIFY);
        assert_eq!(ReloadTrigger::Test.as_str(), RELOAD_TRIGGER_TEST);
    }

    // Journey: specs/journeys/JOURNEY-S-007-nonce-lru-backpressure.md

    /// `NonceCache::contains` returns `true` for an inserted nonce
    /// and `false` for a never-inserted one.
    #[test]
    fn nonce_cache_contains_after_insert() {
        let mut c = NonceCache::new();
        let a = [0x11u8; NONCE_BYTES];
        let b = [0x22u8; NONCE_BYTES];
        c.insert(a);
        assert!(c.contains(&a));
        assert!(!c.contains(&b));
    }

    /// SPEC §6 Idempotency LRU semantics: inserting `NONCE_LRU_CAP +
    /// 1` distinct nonces evicts the first one. Mirrors TC-02 of the
    /// journey.
    #[test]
    fn nonce_cache_evicts_oldest_when_exceeding_cap() {
        let mut c = NonceCache::new();
        let mut nonces: Vec<[u8; NONCE_BYTES]> = Vec::with_capacity(NONCE_LRU_CAP + 1);
        for i in 0u16..=u16::try_from(NONCE_LRU_CAP).unwrap_or(u16::MAX) {
            let mut n = [0u8; NONCE_BYTES];
            n[0] = (i & 0xff) as u8;
            n[1] = ((i >> 8) & 0xff) as u8;
            n[2] = 0xAB; // disambiguator so n[0..2]==0 nonces never collide
            nonces.push(n);
            c.insert(n);
        }
        // Oldest evicted, newest retained, middle entry still present.
        assert!(!c.contains(&nonces[0]), "first nonce must be evicted at cap+1");
        assert!(c.contains(&nonces[1]), "second-oldest nonce must still be present");
        assert!(c.contains(&nonces[NONCE_LRU_CAP]), "newest nonce must be present after insert");
    }
}
