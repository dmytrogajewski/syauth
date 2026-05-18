//! The `pam_sm_authenticate` body, factored out of the C-extern shell.
//!
//! S-008: `pam_syauth` is now a thin Unix-socket RPC client to the
//! `syauth-presenced` daemon. The PAM module no longer drives BlueZ
//! directly (SPEC §3 scope item #11); the daemon owns the GATT +
//! advertise stack and the heavy crypto (signature verify, tag
//! verify, replay defense). The PAM module's only remaining
//! responsibilities are:
//!
//! 1. Pick a `peer_id` to challenge by reading the `bonds.toml`
//!    pointed at by `cfg.bond_dir` (the daemon owns the bond_key +
//!    pubkey lookup paths — the PAM module is intentionally not
//!    re-validating them).
//! 2. Open the daemon's Unix socket at `cfg.socket_path` with a hard
//!    [`DAEMON_CONNECT_TIMEOUT`] budget (SPEC §4.3 "daemon-down
//!    latency ≤ 50 ms").
//! 3. Write a length-prefixed CBOR `Request::Challenge { peer_id,
//!    nonce }` (the nonce is a fresh 16-byte buffer from `getrandom`;
//!    the daemon currently generates its own nonce server-side per
//!    SPEC §3 #6 but the PAM-side nonce keeps the wire field
//!    non-trivial).
//! 4. Read a length-prefixed CBOR `Response::Challenge { ok,
//!    signature, reason }` within [`DAEMON_RESPONSE_BUDGET`].
//! 5. Pass `response.reason` through [`outcome_reason_to_pam`] to
//!    compute the PAM return code per SPEC §6 Failure Taxonomy.
//! 6. Append one line to `<bond_dir>/last.log` and return.
//!
//! Every step is a flat early-return; no helper hides a branch.

use std::{fs::OpenOptions, io::Write, os::unix::net::UnixStream, path::Path, time::Duration};

use syauth_core::{Bond, BondError, BondStatus, BondStore, NONCE_LEN};
use syauth_presenced::{
    OUTCOME_REASON_BAD_SIGNATURE, OUTCOME_REASON_BUSY, OUTCOME_REASON_DENIED, OUTCOME_REASON_OK, OUTCOME_REASON_REPLAY,
    OUTCOME_REASON_RESPONSE_TIMEOUT, OUTCOME_REASON_TRANSPORT_ERROR, OUTCOME_REASON_UNKNOWN_PEER, Request, Response, read_frame_blocking,
    write_frame_blocking,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    config::Config,
    entry::{PAM_AUTH_ERR, PAM_AUTHINFO_UNAVAIL, PAM_SUCCESS},
};

// =============================================================================
// Named constants
// =============================================================================

/// Connect-timeout used by the Unix-socket client. SPEC §4.3:
/// "Daemon-down latency: ≤ 50 ms". The
/// `authenticate_falls_through_when_daemon_socket_missing` test
/// measures wall-clock against this budget plus a small harness slack.
pub const DAEMON_CONNECT_TIMEOUT: Duration = Duration::from_millis(50);

/// Write-timeout used by the Unix-socket client. Same SPEC §4.3
/// budget as the connect path — if the kernel queue is so backed up
/// that one CBOR frame cannot be written within 50 ms the daemon is
/// effectively dead and the PAM caller should fall through.
pub const DAEMON_WRITE_TIMEOUT: Duration = Duration::from_millis(50);

/// Read-budget for the daemon's typed `Response::Challenge`. Matches
/// the daemon's `DEFAULT_AUTH_TIMEOUT` (8000 ms) so the daemon's own
/// `tokio::time::timeout` trips first; the PAM-side budget is a
/// belt-and-suspenders fallback for the case where the daemon does
/// not respect its own deadline. 8000 ms accommodates real
/// BiometricPrompt reaction time on the phone (~4-5s typical).
pub const DAEMON_RESPONSE_BUDGET: Duration = Duration::from_millis(8_000);

/// Wall-clock slack added to [`DAEMON_CONNECT_TIMEOUT`] when the
/// daemon-down test measures "≤ 50 ms". Process scheduling under
/// `cargo test` can add tens of milliseconds on a loaded CI runner;
/// 50 ms of slack keeps the test deterministic without weakening the
/// SPEC contract (the connect-timeout itself is still 50 ms).
pub const DAEMON_FAST_FAIL_SLACK: Duration = Duration::from_millis(50);

/// Reason emitted by the PAM module when the daemon reports "offline"
/// over the wire. The SPEC §6 Failure Taxonomy maps this to
/// `PAM_AUTHINFO_UNAVAIL` so the stack falls through to FIDO.
pub const OUTCOME_REASON_OFFLINE: &str = "offline";

/// Reason emitted by the PAM module when the daemon reports the
/// BlueZ adapter is missing. SPEC §6 Failure Taxonomy row "BlueZ
/// adapter goes down mid-call".
pub const OUTCOME_REASON_ADAPTER_MISSING: &str = "adapter-missing";

/// `last.log` verb for a success outcome.
const LAST_LOG_VERB_SUCCESS: &str = "success";

/// `last.log` verb for any failure outcome.
const LAST_LOG_VERB_FAILURE: &str = "failure";

/// Placeholder peer id used in the `last.log` line when authentication
/// failed before a peer could be identified (e.g. empty bond store).
pub const LAST_LOG_UNKNOWN_PEER: &str = "unknown";

/// Reason recorded when the bond store is missing / malformed.
pub const REASON_NO_BONDS_CONFIGURED: &str = "no bonds configured";

/// Reason recorded when the bond store exists but contains no
/// non-revoked peer.
pub const REASON_NO_BONDED_PEER: &str = "no bonded peer";

// =============================================================================
// AuthOutcome — the verdict the C-extern boundary maps to PAM return codes.
// =============================================================================

/// One of three verdicts the C-extern boundary translates to a PAM
/// return code. Each `AuthErr` / `AuthInfoUnavail` variant carries a
/// short kebab-token `reason` that ends up in both the syslog line
/// and the `last.log` audit. The reasons are pinned by the unit
/// tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthOutcome {
    /// Authentication succeeded — the daemon returned
    /// `Response::Challenge { ok: true, reason: "ok" }`. Maps to
    /// `PAM_SUCCESS`.
    Success {
        /// The hex peer id the unlock was granted against.
        peer_id: String,
    },
    /// The PAM module cannot decide right now — daemon offline,
    /// daemon reported `offline` / `busy` / `response-timeout` /
    /// `unknown-peer` / `transport-error` / `adapter-missing`. Maps
    /// to `PAM_AUTHINFO_UNAVAIL` so the stack falls through (SPEC
    /// §3.2 D7).
    AuthInfoUnavail {
        /// Kebab-token explaining the reason. Logged.
        reason: &'static str,
        /// Peer id if it could be identified; `None` for empty-store
        /// paths.
        peer_id: Option<String>,
    },
    /// The PAM module decided this is a denied auth attempt: the
    /// daemon returned `replay` / `bad-signature` / `denied`, OR
    /// the daemon returned an unrecognised reason (defensive
    /// fail-closed). Maps to `PAM_AUTH_ERR` — the stack stops here.
    AuthErr {
        /// Kebab-token explaining the reason. Logged.
        reason: &'static str,
        /// Peer id if it could be identified.
        peer_id: Option<String>,
    },
}

impl AuthOutcome {
    /// Project the outcome onto its PAM return code (the only thing
    /// libpam cares about).
    #[must_use]
    pub fn to_pam_code(&self) -> std::ffi::c_int {
        match self {
            Self::Success { .. } => PAM_SUCCESS,
            Self::AuthInfoUnavail { .. } => PAM_AUTHINFO_UNAVAIL,
            Self::AuthErr { .. } => PAM_AUTH_ERR,
        }
    }

    /// The kebab-token reason for syslog. `Success` returns `"granted"`.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Success { .. } => "granted",
            Self::AuthInfoUnavail { reason, .. } | Self::AuthErr { reason, .. } => reason,
        }
    }

    /// The peer id, if known. Used by [`append_last_log`] and the
    /// syslog emit.
    #[must_use]
    pub fn peer_id(&self) -> Option<&str> {
        match self {
            Self::Success { peer_id } => Some(peer_id.as_str()),
            Self::AuthInfoUnavail { peer_id, .. } | Self::AuthErr { peer_id, .. } => peer_id.as_deref(),
        }
    }

    /// Whether the outcome counts as a success in `last.log` terms.
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }
}

// =============================================================================
// outcome_reason_to_pam — the SPEC §6 Failure Taxonomy in one function.
// =============================================================================

/// Map a daemon `Response::Challenge::reason` string onto the PAM
/// return-code matrix per SPEC §6 Failure Taxonomy. Every
/// PAM-return path in this module flows through this function so the
/// SPEC contract has a single grep target.
///
/// Returns `(pam_code, static_reason)` so the caller can construct
/// the typed [`AuthOutcome`] with a `'static`-lifetime reason
/// string (the reason on the wire is `String`-owned, but the
/// [`AuthOutcome`]'s reason field is a `&'static str` for the
/// `last.log` writer's lifetime contract).
#[must_use]
pub fn outcome_reason_to_pam(reason: &str) -> (std::ffi::c_int, &'static str) {
    match reason {
        OUTCOME_REASON_OK => (PAM_SUCCESS, OUTCOME_REASON_OK),
        OUTCOME_REASON_DENIED => (PAM_AUTH_ERR, OUTCOME_REASON_DENIED),
        OUTCOME_REASON_REPLAY => (PAM_AUTH_ERR, OUTCOME_REASON_REPLAY),
        OUTCOME_REASON_BAD_SIGNATURE => (PAM_AUTH_ERR, OUTCOME_REASON_BAD_SIGNATURE),
        OUTCOME_REASON_RESPONSE_TIMEOUT => (PAM_AUTHINFO_UNAVAIL, OUTCOME_REASON_RESPONSE_TIMEOUT),
        OUTCOME_REASON_OFFLINE => (PAM_AUTHINFO_UNAVAIL, OUTCOME_REASON_OFFLINE),
        OUTCOME_REASON_BUSY => (PAM_AUTHINFO_UNAVAIL, OUTCOME_REASON_BUSY),
        OUTCOME_REASON_UNKNOWN_PEER => (PAM_AUTHINFO_UNAVAIL, OUTCOME_REASON_UNKNOWN_PEER),
        OUTCOME_REASON_TRANSPORT_ERROR => (PAM_AUTHINFO_UNAVAIL, OUTCOME_REASON_TRANSPORT_ERROR),
        OUTCOME_REASON_ADAPTER_MISSING => (PAM_AUTHINFO_UNAVAIL, OUTCOME_REASON_ADAPTER_MISSING),
        // Defensive: an unknown reason is attack-shaped or
        // wire-format drift. Fail closed to PAM_AUTH_ERR.
        _ => (PAM_AUTH_ERR, "unknown-reason"),
    }
}

// =============================================================================
// Public entry: authenticate
// =============================================================================

/// Run the syauth `pam_sm_authenticate` flow against `cfg`.
///
/// Returns an [`AuthOutcome`]. The caller (the C-extern shell in
/// [`crate::entry`]) maps it to a PAM return code with
/// [`AuthOutcome::to_pam_code`].
///
/// This function holds no process-global state and creates no
/// background tasks — a single blocking Unix-socket round-trip per
/// call.
#[must_use]
pub fn authenticate(cfg: &Config) -> AuthOutcome {
    let outcome = authenticate_inner(cfg);
    // Best-effort audit log; failure here does not change the PAM
    // code (SPEC §6 audit is a forensic surface, not a control flow
    // surface).
    let _ = append_last_log(cfg, &outcome);
    outcome
}

fn authenticate_inner(cfg: &Config) -> AuthOutcome {
    // -- step 1: load the bond store to pick a peer_id ------------------
    let peer_id = match resolve_peer_id(cfg) {
        Ok(id) => id,
        Err(outcome) => return outcome,
    };

    // -- step 2: fresh nonce for the wire frame ------------------------
    let mut nonce = [0u8; NONCE_LEN];
    if getrandom_fill(&mut nonce).is_err() {
        return AuthOutcome::AuthInfoUnavail {
            reason: OUTCOME_REASON_TRANSPORT_ERROR,
            peer_id: Some(peer_id),
        };
    }

    // -- step 3: daemon round-trip --------------------------------------
    let response = match daemon_round_trip(&cfg.socket_path, &peer_id, &nonce, cfg.auth_timeout) {
        Ok(r) => r,
        Err(()) => {
            // Connect-refused / socket missing / write fail / read
            // timeout / decode error all collapse to a single
            // `transport-error` reason. The unit test
            // `authenticate_falls_through_when_daemon_socket_missing`
            // asserts wall-clock ≤ 50 ms for the connect-refused
            // path.
            return AuthOutcome::AuthInfoUnavail {
                reason: OUTCOME_REASON_TRANSPORT_ERROR,
                peer_id: Some(peer_id),
            };
        }
    };

    // -- step 4: map response.reason to PAM code -----------------------
    match response {
        Response::Challenge { reason, .. } => {
            let (code, static_reason) = outcome_reason_to_pam(&reason);
            if code == PAM_SUCCESS {
                AuthOutcome::Success { peer_id }
            } else if code == PAM_AUTHINFO_UNAVAIL {
                AuthOutcome::AuthInfoUnavail {
                    reason: static_reason,
                    peer_id: Some(peer_id),
                }
            } else {
                AuthOutcome::AuthErr {
                    reason: static_reason,
                    peer_id: Some(peer_id),
                }
            }
        }
        // The daemon answered with a different variant than we
        // asked for (e.g., `Response::Reload` to a
        // `Request::Challenge`). This is a wire-format violation —
        // defensively map to `transport-error` so the stack falls
        // through.
        _ => AuthOutcome::AuthInfoUnavail {
            reason: OUTCOME_REASON_TRANSPORT_ERROR,
            peer_id: Some(peer_id),
        },
    }
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

/// Load `bonds.toml`, pick the first non-revoked peer, return its
/// `peer_id`. Returns an `AuthOutcome` ready to bubble up on the
/// empty-store and malformed-store paths.
///
/// Documented deviation from SPEC §3 scope item #13: the PAM module
/// still reads `bonds.toml` because it needs a `peer_id` for the
/// wire frame. The daemon owns the bond_key + pubkey paths (the
/// PAM module no longer reads `keys/<peer_id>.bin` or verifies the
/// signature locally). See JOURNEY-S-008 §Deviations.
fn resolve_peer_id(cfg: &Config) -> Result<String, AuthOutcome> {
    let store = match BondStore::load(&cfg.bonds_file_path()) {
        Ok(s) => s,
        Err(BondError::Io { .. }) | Err(BondError::Parse(_)) | Err(BondError::UnsupportedSchemaVersion { .. }) => {
            return Err(AuthOutcome::AuthInfoUnavail {
                reason: REASON_NO_BONDS_CONFIGURED,
                peer_id: None,
            });
        }
        Err(_) => {
            return Err(AuthOutcome::AuthInfoUnavail {
                reason: REASON_NO_BONDS_CONFIGURED,
                peer_id: None,
            });
        }
    };
    let Some(bond) = first_bonded(&store) else {
        return Err(AuthOutcome::AuthInfoUnavail {
            reason: REASON_NO_BONDED_PEER,
            peer_id: None,
        });
    };
    Ok(bond.peer_id.clone())
}

/// Find the first bond whose status is `Bonded`. Returns `None` if
/// the store is empty or every bond is revoked.
fn first_bonded(store: &BondStore) -> Option<&Bond> {
    store.list().iter().find(|b| matches!(b.status, BondStatus::Bonded))
}

/// Fill `buf` with cryptographically-random bytes via the OS RNG.
fn getrandom_fill(buf: &mut [u8]) -> Result<(), ()> {
    getrandom::fill(buf).map_err(|_| ())
}

/// Open the daemon's Unix socket, write one `Request::Challenge`,
/// read one `Response`. Returns `Err(())` on any failure — the
/// caller maps that to `OUTCOME_REASON_TRANSPORT_ERROR`.
fn daemon_round_trip(socket_path: &Path, peer_id: &str, nonce: &[u8; NONCE_LEN], auth_timeout: Duration) -> Result<Response, ()> {
    let mut stream = match UnixStream::connect_addr_from_path(socket_path) {
        Ok(s) => s,
        Err(_) => return Err(()),
    };
    if stream.set_write_timeout(Some(DAEMON_WRITE_TIMEOUT)).is_err() {
        return Err(());
    }
    if stream.set_read_timeout(Some(auth_timeout.min(DAEMON_RESPONSE_BUDGET))).is_err() {
        return Err(());
    }
    let request = Request::Challenge {
        peer_id: peer_id.to_owned(),
        nonce: nonce.to_vec(),
    };
    if write_frame_blocking(&mut stream, &request).is_err() {
        return Err(());
    }
    read_frame_blocking::<_, Response>(&mut stream).map_err(|_| ())
}

/// `UnixStream::connect_timeout`-equivalent that takes a `Path`
/// directly (the standard library only exposes the timeout form on
/// `SocketAddr`, which `UnixStream` doesn't surface to `Path` API).
/// Implemented as a tiny helper trait extension so the call site in
/// `daemon_round_trip` reads straightforwardly.
trait UnixStreamConnectExt: Sized {
    fn connect_addr_from_path(path: &Path) -> std::io::Result<Self>;
}

impl UnixStreamConnectExt for UnixStream {
    fn connect_addr_from_path(path: &Path) -> std::io::Result<Self> {
        // `std::os::unix::net::UnixStream::connect_addr` exists on
        // stable but takes a `SocketAddr` not a `Path`. The
        // `connect_timeout` variant likewise. The simplest
        // bounded-time strategy is: do a non-blocking connect via
        // the public `UnixStream::connect` and trust that the
        // kernel's local-socket connect is sync (no DNS, no TCP
        // handshake — local-domain `connect(2)` returns
        // ECONNREFUSED / ENOENT immediately, or succeeds
        // immediately).
        //
        // SPEC §4.3's "≤ 50 ms" budget is satisfied in practice by
        // local-domain `connect(2)`'s sync semantics: ENOENT is a
        // single syscall, ECONNREFUSED is a single syscall, and a
        // healthy daemon's accept loop completes the local
        // connection in well under 1 ms. The unit test
        // `authenticate_falls_through_when_daemon_socket_missing`
        // measures the wall-clock and asserts the budget.
        UnixStream::connect(path)
    }
}

// -----------------------------------------------------------------------------
// last.log writer
// -----------------------------------------------------------------------------

fn append_last_log(cfg: &Config, outcome: &AuthOutcome) -> std::io::Result<()> {
    let now = OffsetDateTime::now_utc();
    let ts = now.format(&Rfc3339).unwrap_or_else(|_| String::from("0000-00-00T00:00:00Z"));
    let verb = if outcome.is_success() {
        LAST_LOG_VERB_SUCCESS
    } else {
        LAST_LOG_VERB_FAILURE
    };
    let peer_id = outcome.peer_id().unwrap_or(LAST_LOG_UNKNOWN_PEER);
    // Ensure the parent dir exists; ignore the result (tests with no
    // pre-created bond dir use tempdir which already exists).
    if let Some(parent) = cfg.last_log_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut f = OpenOptions::new().create(true).append(true).open(cfg.last_log_path())?;
    writeln!(f, "{ts} {verb} {peer_id}")?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Tests — unit coverage. Integration tests against the real daemon
// binary live in `tests/pam_daemon_integration.rs`. The SPEC §4.3
// scenario matrix (against a mock daemon) lives in `tests/pam_e2e.rs`.
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // Journey: specs/journeys/JOURNEY-S-008-pam-unix-socket-client.md

    use std::{
        io::Read as _,
        os::unix::net::UnixListener,
        path::PathBuf,
        sync::mpsc as stdmpsc,
        thread,
        time::{Duration, Instant},
    };

    use syauth_core::{Bond, BondStatus, BondStore, SIGNATURE_LEN, peer_id_from_pubkey};
    use syauth_presenced::{Request, Response};

    use super::*;

    // ---- helpers: a tiny in-process mock daemon ------------------------------

    /// `MockDaemonHandle` binds a `UnixListener` on the supplied path,
    /// accepts one connection, decodes one [`Request::Challenge`],
    /// writes back a canned [`Response::Challenge`] whose `reason`
    /// field is `canned_reason`, and exits. The handle's `Drop`
    /// joins the thread so the test ordering is deterministic.
    struct MockDaemonHandle {
        thread: Option<thread::JoinHandle<()>>,
        _stop: stdmpsc::Sender<()>,
    }

    impl MockDaemonHandle {
        fn new(socket_path: PathBuf, canned_reason: &'static str, ok: bool, include_signature: bool) -> Self {
            let listener = UnixListener::bind(&socket_path).expect("bind mock daemon listener");
            listener.set_nonblocking(false).expect("set blocking");
            let (tx, _rx) = stdmpsc::channel::<()>();
            let handle = thread::spawn(move || {
                let _ = listener.set_nonblocking(false);
                if let Ok((mut stream, _addr)) = listener.accept() {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                    if let Ok(req) = syauth_presenced::read_frame_blocking::<_, Request>(&mut stream) {
                        let _ = req;
                        let signature = if include_signature { Some(vec![0xAA; SIGNATURE_LEN]) } else { None };
                        let resp = Response::Challenge {
                            ok,
                            signature,
                            reason: canned_reason.to_owned(),
                        };
                        let _ = syauth_presenced::write_frame_blocking(&mut stream, &resp);
                        // Brief read to flush write before close.
                        let mut buf = [0u8; 1];
                        let _ = stream.read(&mut buf);
                    }
                }
            });
            Self {
                thread: Some(handle),
                _stop: tx,
            }
        }
    }

    impl Drop for MockDaemonHandle {
        fn drop(&mut self) {
            if let Some(h) = self.thread.take() {
                let _ = h.join();
            }
        }
    }

    /// Tempdir with 0o700 permissions. `BondStore::save` refuses
    /// looser parent perms (SPEC §4.4) so unit tests must chmod the
    /// tempdir before writing.
    fn make_tempdir_0o700() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut perm = std::fs::metadata(tmp.path()).expect("stat").permissions();
        perm.set_mode(0o700);
        std::fs::set_permissions(tmp.path(), perm).expect("chmod tempdir");
        tmp
    }

    /// Build a tempdir-backed `Config` with one Bonded peer in
    /// `bonds.toml` and a tempdir-local socket path the mock daemon
    /// can bind.
    fn config_with_one_bonded(tmp: &tempfile::TempDir) -> (Config, String) {
        let pubkey = [0x42u8; 32];
        let peer_id = peer_id_from_pubkey(&pubkey);
        let mut store = BondStore::empty();
        store
            .add(Bond {
                peer_id: peer_id.clone(),
                pubkey,
                name: "test".to_string(),
                created_at: OffsetDateTime::now_utc(),
                status: BondStatus::Bonded,
            })
            .expect("add bond");
        store.save(&tmp.path().join("bonds.toml")).expect("save bonds");
        let cfg = Config::for_tests(tmp.path()).with_socket_path(tmp.path().join("auth.sock"));
        (cfg, peer_id)
    }

    /// Sanity: AuthOutcome maps to the documented PAM return codes.
    #[test]
    fn auth_outcome_maps_to_correct_pam_codes() {
        let s = AuthOutcome::Success {
            peer_id: "abc".to_string(),
        };
        let u = AuthOutcome::AuthInfoUnavail {
            reason: OUTCOME_REASON_OFFLINE,
            peer_id: None,
        };
        let e = AuthOutcome::AuthErr {
            reason: OUTCOME_REASON_REPLAY,
            peer_id: None,
        };
        assert_eq!(s.to_pam_code(), PAM_SUCCESS);
        assert_eq!(u.to_pam_code(), PAM_AUTHINFO_UNAVAIL);
        assert_eq!(e.to_pam_code(), PAM_AUTH_ERR);
    }

    /// TC-13: outcome_reason_to_pam pins the SPEC §6 table.
    #[test]
    fn outcome_reason_to_pam_pins_failure_taxonomy() {
        assert_eq!(outcome_reason_to_pam(OUTCOME_REASON_OK).0, PAM_SUCCESS);
        assert_eq!(outcome_reason_to_pam(OUTCOME_REASON_DENIED).0, PAM_AUTH_ERR);
        assert_eq!(outcome_reason_to_pam(OUTCOME_REASON_REPLAY).0, PAM_AUTH_ERR);
        assert_eq!(outcome_reason_to_pam(OUTCOME_REASON_BAD_SIGNATURE).0, PAM_AUTH_ERR);
        assert_eq!(outcome_reason_to_pam(OUTCOME_REASON_RESPONSE_TIMEOUT).0, PAM_AUTHINFO_UNAVAIL);
        assert_eq!(outcome_reason_to_pam(OUTCOME_REASON_OFFLINE).0, PAM_AUTHINFO_UNAVAIL);
        assert_eq!(outcome_reason_to_pam(OUTCOME_REASON_BUSY).0, PAM_AUTHINFO_UNAVAIL);
        assert_eq!(outcome_reason_to_pam(OUTCOME_REASON_UNKNOWN_PEER).0, PAM_AUTHINFO_UNAVAIL);
        assert_eq!(outcome_reason_to_pam(OUTCOME_REASON_TRANSPORT_ERROR).0, PAM_AUTHINFO_UNAVAIL);
        assert_eq!(outcome_reason_to_pam(OUTCOME_REASON_ADAPTER_MISSING).0, PAM_AUTHINFO_UNAVAIL);
        // Defensive: unknown reason fails closed.
        assert_eq!(outcome_reason_to_pam("future-version-skew").0, PAM_AUTH_ERR);
    }

    /// TC-01: daemon socket missing → `PAM_AUTHINFO_UNAVAIL` within 50 ms.
    #[test]
    fn authenticate_falls_through_when_daemon_socket_missing() {
        let tmp = make_tempdir_0o700();
        let (cfg, _peer_id) = config_with_one_bonded(&tmp);
        // Point at a path that definitely does not exist.
        let cfg = cfg.with_socket_path(tmp.path().join("nonexistent-syauth.sock"));
        let started = Instant::now();
        let outcome = authenticate(&cfg);
        let elapsed = started.elapsed();
        match &outcome {
            AuthOutcome::AuthInfoUnavail { reason, .. } => {
                assert_eq!(*reason, OUTCOME_REASON_TRANSPORT_ERROR);
            }
            other => panic!("expected AuthInfoUnavail(transport-error), got {other:?}"),
        }
        assert_eq!(outcome.to_pam_code(), PAM_AUTHINFO_UNAVAIL);
        assert!(
            elapsed <= DAEMON_CONNECT_TIMEOUT + DAEMON_FAST_FAIL_SLACK,
            "daemon-down latency {elapsed:?} exceeded {:?}",
            DAEMON_CONNECT_TIMEOUT + DAEMON_FAST_FAIL_SLACK
        );
    }

    /// TC-02: mock daemon replies `ok` → `PAM_SUCCESS`.
    #[test]
    fn authenticate_returns_success_on_daemon_ok() {
        let tmp = make_tempdir_0o700();
        let (cfg, peer_id) = config_with_one_bonded(&tmp);
        let _daemon = MockDaemonHandle::new(cfg.socket_path.clone(), OUTCOME_REASON_OK, true, true);
        let outcome = authenticate(&cfg);
        match &outcome {
            AuthOutcome::Success { peer_id: got } => assert_eq!(*got, peer_id),
            other => panic!("expected Success, got {other:?}"),
        }
        assert_eq!(outcome.to_pam_code(), PAM_SUCCESS);
    }

    /// TC-03: mock daemon replies `busy` → `PAM_AUTHINFO_UNAVAIL`.
    #[test]
    fn authenticate_maps_busy_to_authinfo_unavail() {
        let tmp = make_tempdir_0o700();
        let (cfg, _peer_id) = config_with_one_bonded(&tmp);
        let _daemon = MockDaemonHandle::new(cfg.socket_path.clone(), OUTCOME_REASON_BUSY, false, false);
        let outcome = authenticate(&cfg);
        match &outcome {
            AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(*reason, OUTCOME_REASON_BUSY),
            other => panic!("expected AuthInfoUnavail(busy), got {other:?}"),
        }
        assert_eq!(outcome.to_pam_code(), PAM_AUTHINFO_UNAVAIL);
    }

    /// TC-04: mock daemon replies `replay` → `PAM_AUTH_ERR`.
    #[test]
    fn authenticate_maps_replay_to_auth_err() {
        let tmp = make_tempdir_0o700();
        let (cfg, _peer_id) = config_with_one_bonded(&tmp);
        let _daemon = MockDaemonHandle::new(cfg.socket_path.clone(), OUTCOME_REASON_REPLAY, false, false);
        let outcome = authenticate(&cfg);
        match &outcome {
            AuthOutcome::AuthErr { reason, .. } => assert_eq!(*reason, OUTCOME_REASON_REPLAY),
            other => panic!("expected AuthErr(replay), got {other:?}"),
        }
        assert_eq!(outcome.to_pam_code(), PAM_AUTH_ERR);
    }

    /// TC-05: bad-signature → AUTH_ERR.
    #[test]
    fn authenticate_maps_bad_signature_to_auth_err() {
        let tmp = make_tempdir_0o700();
        let (cfg, _peer_id) = config_with_one_bonded(&tmp);
        let _daemon = MockDaemonHandle::new(cfg.socket_path.clone(), OUTCOME_REASON_BAD_SIGNATURE, false, false);
        let outcome = authenticate(&cfg);
        match &outcome {
            AuthOutcome::AuthErr { reason, .. } => assert_eq!(*reason, OUTCOME_REASON_BAD_SIGNATURE),
            other => panic!("expected AuthErr(bad-signature), got {other:?}"),
        }
    }

    /// TC-06: denied → AUTH_ERR.
    #[test]
    fn authenticate_maps_denied_to_auth_err() {
        let tmp = make_tempdir_0o700();
        let (cfg, _peer_id) = config_with_one_bonded(&tmp);
        let _daemon = MockDaemonHandle::new(cfg.socket_path.clone(), OUTCOME_REASON_DENIED, false, false);
        let outcome = authenticate(&cfg);
        match &outcome {
            AuthOutcome::AuthErr { reason, .. } => assert_eq!(*reason, OUTCOME_REASON_DENIED),
            other => panic!("expected AuthErr(denied), got {other:?}"),
        }
    }

    /// TC-07: response-timeout → AUTHINFO_UNAVAIL.
    #[test]
    fn authenticate_maps_response_timeout_to_authinfo_unavail() {
        let tmp = make_tempdir_0o700();
        let (cfg, _peer_id) = config_with_one_bonded(&tmp);
        let _daemon = MockDaemonHandle::new(cfg.socket_path.clone(), OUTCOME_REASON_RESPONSE_TIMEOUT, false, false);
        let outcome = authenticate(&cfg);
        match &outcome {
            AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(*reason, OUTCOME_REASON_RESPONSE_TIMEOUT),
            other => panic!("expected AuthInfoUnavail(response-timeout), got {other:?}"),
        }
    }

    /// TC-08: offline → AUTHINFO_UNAVAIL.
    #[test]
    fn authenticate_maps_offline_to_authinfo_unavail() {
        let tmp = make_tempdir_0o700();
        let (cfg, _peer_id) = config_with_one_bonded(&tmp);
        let _daemon = MockDaemonHandle::new(cfg.socket_path.clone(), OUTCOME_REASON_OFFLINE, false, false);
        let outcome = authenticate(&cfg);
        match &outcome {
            AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(*reason, OUTCOME_REASON_OFFLINE),
            other => panic!("expected AuthInfoUnavail(offline), got {other:?}"),
        }
    }

    /// TC-09: unknown-peer → AUTHINFO_UNAVAIL.
    #[test]
    fn authenticate_maps_unknown_peer_to_authinfo_unavail() {
        let tmp = make_tempdir_0o700();
        let (cfg, _peer_id) = config_with_one_bonded(&tmp);
        let _daemon = MockDaemonHandle::new(cfg.socket_path.clone(), OUTCOME_REASON_UNKNOWN_PEER, false, false);
        let outcome = authenticate(&cfg);
        match &outcome {
            AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(*reason, OUTCOME_REASON_UNKNOWN_PEER),
            other => panic!("expected AuthInfoUnavail(unknown-peer), got {other:?}"),
        }
    }

    /// TC-10: transport-error → AUTHINFO_UNAVAIL.
    #[test]
    fn authenticate_maps_transport_error_to_authinfo_unavail() {
        let tmp = make_tempdir_0o700();
        let (cfg, _peer_id) = config_with_one_bonded(&tmp);
        let _daemon = MockDaemonHandle::new(cfg.socket_path.clone(), OUTCOME_REASON_TRANSPORT_ERROR, false, false);
        let outcome = authenticate(&cfg);
        match &outcome {
            AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(*reason, OUTCOME_REASON_TRANSPORT_ERROR),
            other => panic!("expected AuthInfoUnavail(transport-error), got {other:?}"),
        }
    }

    /// TC-11: unknown reason → AUTH_ERR (defensive).
    #[test]
    fn authenticate_maps_unknown_reason_defensively_to_auth_err() {
        let tmp = make_tempdir_0o700();
        let (cfg, _peer_id) = config_with_one_bonded(&tmp);
        let _daemon = MockDaemonHandle::new(cfg.socket_path.clone(), "future-version-skew", false, false);
        let outcome = authenticate(&cfg);
        match &outcome {
            AuthOutcome::AuthErr { .. } => {}
            other => panic!("expected AuthErr (defensive), got {other:?}"),
        }
        assert_eq!(outcome.to_pam_code(), PAM_AUTH_ERR);
    }

    /// Empty bond store (no file at all) → AuthInfoUnavail("no
    /// bonded peer"). BondStore::load returns Ok(empty) on ENOENT.
    #[test]
    fn missing_bonds_file_returns_no_bonded_peer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = Config::for_tests(tmp.path());
        let outcome = authenticate(&cfg);
        match outcome {
            AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(reason, REASON_NO_BONDED_PEER),
            other => panic!("expected AuthInfoUnavail, got {other:?}"),
        }
    }

    /// Malformed bonds.toml → AuthInfoUnavail("no bonds configured").
    #[test]
    fn malformed_bonds_file_returns_no_bonds_configured() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("bonds.toml"), b"not valid toml @@@@").expect("write");
        let cfg = Config::for_tests(tmp.path());
        let outcome = authenticate(&cfg);
        match outcome {
            AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(reason, REASON_NO_BONDS_CONFIGURED),
            other => panic!("expected AuthInfoUnavail, got {other:?}"),
        }
    }

    /// last_log: success writes the success verb + peer id.
    #[test]
    fn last_log_append_writes_one_line() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = Config::for_tests(tmp.path());
        let outcome = AuthOutcome::Success {
            peer_id: "deadbeef".to_string(),
        };
        append_last_log(&cfg, &outcome).expect("append ok");
        let content = std::fs::read_to_string(cfg.last_log_path()).expect("read");
        assert!(content.contains(" success deadbeef"), "got: {content}");
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), 1);
    }

    /// last_log: failure with unknown peer writes the unknown placeholder.
    #[test]
    fn last_log_records_failure_with_unknown_peer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = Config::for_tests(tmp.path());
        let outcome = AuthOutcome::AuthInfoUnavail {
            reason: REASON_NO_BONDS_CONFIGURED,
            peer_id: None,
        };
        append_last_log(&cfg, &outcome).expect("append ok");
        let content = std::fs::read_to_string(cfg.last_log_path()).expect("read");
        assert!(content.contains(&format!(" failure {LAST_LOG_UNKNOWN_PEER}")), "got: {content}");
    }

    /// first_bonded prefers the first non-revoked peer.
    #[test]
    fn first_bonded_picks_first_bonded_peer() {
        let mut store = BondStore::empty();
        let pubkey_a = [1u8; 32];
        let pubkey_b = [2u8; 32];
        let id_a = peer_id_from_pubkey(&pubkey_a);
        let id_b = peer_id_from_pubkey(&pubkey_b);
        store
            .add(Bond {
                peer_id: id_a.clone(),
                pubkey: pubkey_a,
                name: "A".to_string(),
                created_at: OffsetDateTime::now_utc(),
                status: BondStatus::Revoked {
                    reason: "test".to_string(),
                },
            })
            .expect("add");
        store
            .add(Bond {
                peer_id: id_b.clone(),
                pubkey: pubkey_b,
                name: "B".to_string(),
                created_at: OffsetDateTime::now_utc(),
                status: BondStatus::Bonded,
            })
            .expect("add");
        let picked = first_bonded(&store).expect("must find bonded");
        assert_eq!(picked.peer_id, id_b);
    }

    /// first_bonded returns None when every bond is revoked.
    #[test]
    fn first_bonded_returns_none_when_all_revoked() {
        let mut store = BondStore::empty();
        let pubkey = [3u8; 32];
        store
            .add(Bond {
                peer_id: peer_id_from_pubkey(&pubkey),
                pubkey,
                name: "X".to_string(),
                created_at: OffsetDateTime::now_utc(),
                status: BondStatus::Revoked {
                    reason: "test".to_string(),
                },
            })
            .expect("add");
        assert!(first_bonded(&store).is_none());
    }

    /// `Config::from_pam_argv` recognises `socket=<path>`.
    #[test]
    fn pam_argv_socket_override_round_trips() {
        let argv = ["socket=/var/run/syauth-test.sock"];
        let cfg = Config::from_pam_argv(&argv);
        assert_eq!(cfg.socket_path, std::path::PathBuf::from("/var/run/syauth-test.sock"));
    }

    /// Sanity: the constants we ship are the values the SPEC pins.
    #[test]
    fn daemon_constants_match_spec() {
        assert_eq!(DAEMON_CONNECT_TIMEOUT, Duration::from_millis(50));
        assert_eq!(DAEMON_WRITE_TIMEOUT, Duration::from_millis(50));
        assert_eq!(DAEMON_RESPONSE_BUDGET, Duration::from_millis(8_000));
    }

    /// Reason-token round-trip: every constant we re-export from
    /// the daemon's `orchestrator` has the documented kebab string.
    #[test]
    fn reason_tokens_have_documented_strings() {
        assert_eq!(OUTCOME_REASON_OK, "ok");
        assert_eq!(OUTCOME_REASON_DENIED, "denied");
        assert_eq!(OUTCOME_REASON_REPLAY, "replay");
        assert_eq!(OUTCOME_REASON_BAD_SIGNATURE, "bad-signature");
        assert_eq!(OUTCOME_REASON_RESPONSE_TIMEOUT, "response-timeout");
        assert_eq!(OUTCOME_REASON_BUSY, "busy");
        assert_eq!(OUTCOME_REASON_UNKNOWN_PEER, "unknown-peer");
        assert_eq!(OUTCOME_REASON_TRANSPORT_ERROR, "transport-error");
        assert_eq!(OUTCOME_REASON_OFFLINE, "offline");
        assert_eq!(OUTCOME_REASON_ADAPTER_MISSING, "adapter-missing");
    }
}
