//! S-008 e2e: drive `auth::authenticate` through every SPEC §6 Failure
//! Taxonomy reason string against an in-process mock daemon + a
//! tempdir bond store.
//!
//! Journey: specs/journeys/JOURNEY-S-008-pam-unix-socket-client.md
//!
//! ## SPEC §6 Failure Taxonomy coverage
//!
//! Each test case below pins one row of the SPEC's reason → PAM-code
//! matrix:
//!
//! - `ok` → `PAM_SUCCESS`
//! - `denied` / `replay` / `bad-signature` → `PAM_AUTH_ERR`
//! - `response-timeout` / `offline` / `busy` / `unknown-peer` /
//!   `transport-error` / `adapter-missing` → `PAM_AUTHINFO_UNAVAIL`
//! - unknown reason → `PAM_AUTH_ERR` (defensive)
//! - daemon socket missing → `PAM_AUTHINFO_UNAVAIL` within 50 ms
//! - setcred → `PAM_SUCCESS`
//!
//! The legacy S-009 SPEC §4.3 scenario harness (which drove an
//! injectable `BtPeer` via `MOCK_PEER`) is gone — the daemon owns
//! the transport now, so this file's substrate is a mock daemon
//! bound on a tempdir socket. The public-shape coverage of every
//! outcome is preserved.

use std::{
    io::Read as _,
    os::unix::net::UnixListener,
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use pam_syauth::{
    auth::{
        self, AuthOutcome, DAEMON_CONNECT_TIMEOUT, DAEMON_FAST_FAIL_SLACK, OUTCOME_REASON_ADAPTER_MISSING, OUTCOME_REASON_OFFLINE,
        REASON_NO_BONDED_PEER,
    },
    config::Config,
    entry,
};
use syauth_core::{Bond, BondStatus, BondStore, SIGNATURE_LEN, peer_id_from_pubkey};
use syauth_presenced::{
    OUTCOME_REASON_BAD_SIGNATURE, OUTCOME_REASON_BUSY, OUTCOME_REASON_DENIED, OUTCOME_REASON_OK, OUTCOME_REASON_REPLAY,
    OUTCOME_REASON_RESPONSE_TIMEOUT, OUTCOME_REASON_TRANSPORT_ERROR, OUTCOME_REASON_UNKNOWN_PEER, Request, Response, read_frame_blocking,
    write_frame_blocking,
};
use tempfile::TempDir;
use time::OffsetDateTime;

// =============================================================================
// Mock daemon: bind a Unix socket, answer one CBOR-framed
// Request::Challenge with a canned Response::Challenge, exit.
// =============================================================================

struct MockDaemon {
    handle: Option<thread::JoinHandle<()>>,
    _stop: mpsc::Sender<()>,
}

impl MockDaemon {
    fn new(socket: PathBuf, reason: &'static str, ok: bool, include_signature: bool) -> Self {
        let listener = UnixListener::bind(&socket).expect("bind mock daemon listener");
        listener.set_nonblocking(false).expect("blocking listener");
        let (tx, _rx) = mpsc::channel::<()>();
        let handle = thread::spawn(move || {
            if let Ok((mut stream, _addr)) = listener.accept() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                if let Ok(_req) = read_frame_blocking::<_, Request>(&mut stream) {
                    let signature = if include_signature { Some(vec![0xAA; SIGNATURE_LEN]) } else { None };
                    let resp = Response::Challenge {
                        ok,
                        signature,
                        reason: reason.to_owned(),
                    };
                    let _ = write_frame_blocking(&mut stream, &resp);
                    let mut buf = [0u8; 1];
                    let _ = stream.read(&mut buf);
                }
            }
        });
        Self {
            handle: Some(handle),
            _stop: tx,
        }
    }
}

impl Drop for MockDaemon {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Tempdir with 0o700 perms (SPEC §4.4 — BondStore::save refuses
/// looser parents).
fn make_tempdir_0o700() -> TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut perm = std::fs::metadata(tmp.path()).expect("stat").permissions();
    perm.set_mode(0o700);
    std::fs::set_permissions(tmp.path(), perm).expect("chmod");
    tmp
}

/// Build a tempdir-backed `Config` with one Bonded peer in
/// `bonds.toml`.
fn config_with_one_bonded() -> (TempDir, Config, String) {
    let pubkey = [0x42u8; 32];
    let peer_id = peer_id_from_pubkey(&pubkey);
    let tmp = make_tempdir_0o700();
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
    (tmp, cfg, peer_id)
}

// =============================================================================
// SPEC §6 reason → PAM-code matrix
// =============================================================================

/// `ok` → `PAM_SUCCESS`.
#[test]
fn tc01_ok_returns_pam_success() {
    let (_tmp, cfg, peer_id) = config_with_one_bonded();
    let _daemon = MockDaemon::new(cfg.socket_path.clone(), OUTCOME_REASON_OK, true, true);
    let outcome = auth::authenticate(&cfg);
    match &outcome {
        AuthOutcome::Success { peer_id: got } => assert_eq!(*got, peer_id),
        other => panic!("expected Success, got {other:?}"),
    }
    assert_eq!(outcome.to_pam_code(), entry::PAM_SUCCESS);
}

/// `denied` → `PAM_AUTH_ERR`.
#[test]
fn tc02_denied_returns_pam_auth_err() {
    let (_tmp, cfg, _peer_id) = config_with_one_bonded();
    let _daemon = MockDaemon::new(cfg.socket_path.clone(), OUTCOME_REASON_DENIED, false, false);
    let outcome = auth::authenticate(&cfg);
    match &outcome {
        AuthOutcome::AuthErr { reason, .. } => assert_eq!(*reason, OUTCOME_REASON_DENIED),
        other => panic!("expected AuthErr(denied), got {other:?}"),
    }
}

/// `replay` → `PAM_AUTH_ERR`.
#[test]
fn tc03_replay_returns_pam_auth_err() {
    let (_tmp, cfg, _peer_id) = config_with_one_bonded();
    let _daemon = MockDaemon::new(cfg.socket_path.clone(), OUTCOME_REASON_REPLAY, false, false);
    let outcome = auth::authenticate(&cfg);
    match &outcome {
        AuthOutcome::AuthErr { reason, .. } => assert_eq!(*reason, OUTCOME_REASON_REPLAY),
        other => panic!("expected AuthErr(replay), got {other:?}"),
    }
}

/// `bad-signature` → `PAM_AUTH_ERR`.
#[test]
fn tc04_bad_signature_returns_pam_auth_err() {
    let (_tmp, cfg, _peer_id) = config_with_one_bonded();
    let _daemon = MockDaemon::new(cfg.socket_path.clone(), OUTCOME_REASON_BAD_SIGNATURE, false, false);
    let outcome = auth::authenticate(&cfg);
    match &outcome {
        AuthOutcome::AuthErr { reason, .. } => assert_eq!(*reason, OUTCOME_REASON_BAD_SIGNATURE),
        other => panic!("expected AuthErr(bad-signature), got {other:?}"),
    }
}

/// `response-timeout` → `PAM_AUTHINFO_UNAVAIL`.
#[test]
fn tc05_response_timeout_returns_authinfo_unavail() {
    let (_tmp, cfg, _peer_id) = config_with_one_bonded();
    let _daemon = MockDaemon::new(cfg.socket_path.clone(), OUTCOME_REASON_RESPONSE_TIMEOUT, false, false);
    let outcome = auth::authenticate(&cfg);
    match &outcome {
        AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(*reason, OUTCOME_REASON_RESPONSE_TIMEOUT),
        other => panic!("expected AuthInfoUnavail(response-timeout), got {other:?}"),
    }
}

/// `offline` → `PAM_AUTHINFO_UNAVAIL`.
#[test]
fn tc06_offline_returns_authinfo_unavail() {
    let (_tmp, cfg, _peer_id) = config_with_one_bonded();
    let _daemon = MockDaemon::new(cfg.socket_path.clone(), OUTCOME_REASON_OFFLINE, false, false);
    let outcome = auth::authenticate(&cfg);
    match &outcome {
        AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(*reason, OUTCOME_REASON_OFFLINE),
        other => panic!("expected AuthInfoUnavail(offline), got {other:?}"),
    }
}

/// `busy` → `PAM_AUTHINFO_UNAVAIL`.
#[test]
fn tc07_busy_returns_authinfo_unavail() {
    let (_tmp, cfg, _peer_id) = config_with_one_bonded();
    let _daemon = MockDaemon::new(cfg.socket_path.clone(), OUTCOME_REASON_BUSY, false, false);
    let outcome = auth::authenticate(&cfg);
    match &outcome {
        AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(*reason, OUTCOME_REASON_BUSY),
        other => panic!("expected AuthInfoUnavail(busy), got {other:?}"),
    }
}

/// `unknown-peer` → `PAM_AUTHINFO_UNAVAIL`.
#[test]
fn tc08_unknown_peer_returns_authinfo_unavail() {
    let (_tmp, cfg, _peer_id) = config_with_one_bonded();
    let _daemon = MockDaemon::new(cfg.socket_path.clone(), OUTCOME_REASON_UNKNOWN_PEER, false, false);
    let outcome = auth::authenticate(&cfg);
    match &outcome {
        AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(*reason, OUTCOME_REASON_UNKNOWN_PEER),
        other => panic!("expected AuthInfoUnavail(unknown-peer), got {other:?}"),
    }
}

/// `transport-error` → `PAM_AUTHINFO_UNAVAIL`.
#[test]
fn tc09_transport_error_returns_authinfo_unavail() {
    let (_tmp, cfg, _peer_id) = config_with_one_bonded();
    let _daemon = MockDaemon::new(cfg.socket_path.clone(), OUTCOME_REASON_TRANSPORT_ERROR, false, false);
    let outcome = auth::authenticate(&cfg);
    match &outcome {
        AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(*reason, OUTCOME_REASON_TRANSPORT_ERROR),
        other => panic!("expected AuthInfoUnavail(transport-error), got {other:?}"),
    }
}

/// `adapter-missing` → `PAM_AUTHINFO_UNAVAIL`.
#[test]
fn tc10_adapter_missing_returns_authinfo_unavail() {
    let (_tmp, cfg, _peer_id) = config_with_one_bonded();
    let _daemon = MockDaemon::new(cfg.socket_path.clone(), OUTCOME_REASON_ADAPTER_MISSING, false, false);
    let outcome = auth::authenticate(&cfg);
    match &outcome {
        AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(*reason, OUTCOME_REASON_ADAPTER_MISSING),
        other => panic!("expected AuthInfoUnavail(adapter-missing), got {other:?}"),
    }
}

// =============================================================================
// Transport / daemon-down cases
// =============================================================================

/// Daemon socket missing → `PAM_AUTHINFO_UNAVAIL` within 50 ms (SPEC §4.3).
#[test]
fn tc11_daemon_socket_missing_falls_through_under_50_ms() {
    let (tmp, cfg, _peer_id) = config_with_one_bonded();
    let cfg = cfg.with_socket_path(tmp.path().join("does-not-exist.sock"));
    let started = Instant::now();
    let outcome = auth::authenticate(&cfg);
    let elapsed = started.elapsed();
    match &outcome {
        AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(*reason, OUTCOME_REASON_TRANSPORT_ERROR),
        other => panic!("expected AuthInfoUnavail(transport-error), got {other:?}"),
    }
    assert!(
        elapsed <= DAEMON_CONNECT_TIMEOUT + DAEMON_FAST_FAIL_SLACK,
        "daemon-down latency {elapsed:?} exceeded {:?}",
        DAEMON_CONNECT_TIMEOUT + DAEMON_FAST_FAIL_SLACK
    );
}

/// Empty bond store → `AuthInfoUnavail("no bonded peer")`. The daemon
/// is never contacted because PAM short-circuits on the peer-id
/// lookup.
#[test]
fn tc12_empty_bond_store_short_circuits_before_daemon() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = Config::for_tests(tmp.path());
    let outcome = auth::authenticate(&cfg);
    match &outcome {
        AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(*reason, REASON_NO_BONDED_PEER),
        other => panic!("expected AuthInfoUnavail(no bonded peer), got {other:?}"),
    }
}

/// Unknown reason (forward-compat) → `PAM_AUTH_ERR` (defensive).
#[test]
fn tc13_unknown_reason_defensively_returns_auth_err() {
    let (_tmp, cfg, _peer_id) = config_with_one_bonded();
    let _daemon = MockDaemon::new(cfg.socket_path.clone(), "future-version-skew", false, false);
    let outcome = auth::authenticate(&cfg);
    match &outcome {
        AuthOutcome::AuthErr { .. } => {}
        other => panic!("expected AuthErr (defensive), got {other:?}"),
    }
    assert_eq!(outcome.to_pam_code(), entry::PAM_AUTH_ERR);
}

// =============================================================================
// PAM C-extern surface
// =============================================================================

/// `pam_sm_setcred` returns `PAM_SUCCESS` (unchanged from S-009).
#[test]
fn tc14_setcred_returns_pam_success() {
    use std::os::raw::{c_char, c_void};
    // SAFETY: pam_sm_setcred is `pub unsafe extern "C" fn`; we hold
    // no PAM handle (the stub does not dereference it), so a null
    // pointer with argc=0 / argv=null is valid input per the libpam
    // ABI contract.
    let got = unsafe { entry::pam_sm_setcred(std::ptr::null_mut::<c_void>(), 0_i32, 0_i32, std::ptr::null::<*const c_char>()) };
    assert_eq!(got, entry::PAM_SUCCESS);
}
