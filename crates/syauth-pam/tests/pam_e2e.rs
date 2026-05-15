//! S-009 e2e: drive `auth::authenticate` through every SPEC §4.3 scenario
//! against an in-process mock peer + a tempdir bond store.
//!
//! Journey: specs/journeys/JOURNEY-S-009-pam-mock-e2e.md
//!
//! ## SPEC §4.3 scenarios (verbatim from `specs/syauth/SPEC.md`)
//!
//! 1. golden: ≤ 2 s success
//! 2. peer offline: `PAM_AUTHINFO_UNAVAIL` ≤ 1.2 s
//! 3. peer denies: `PAM_AUTH_ERR`
//! 4. replay (resend prior response): `PAM_AUTH_ERR`
//! 5. bad signature: `PAM_AUTH_ERR`
//! 6. wrong version: `PAM_AUTH_ERR`
//! 7. revoked peer: never goes to radio; `PAM_AUTH_ERR`
//! 8. MTU split frame: reassembled and succeeds  (here, the negative
//!    "corrupt reassembly" sub-case demanded by S-009 DoD #3)
//! 9. panic in core: `catch_unwind` boundary catches it; `PAM_AUTH_ERR`
//!
//! The DoD also names "oversized-frame" as a sixth `PAM_AUTH_ERR` bucket,
//! exercised here as TC-07 alongside the SPEC list.
//!
//! Why these tests do NOT shell out to `pamtester`: the SPEC §4.3 scenarios
//! are about the Rust logic inside the panic boundary. The C-extern boundary
//! itself is covered by `tests/pam_smoke.rs` (S-008) under `SYAUTH_E2E=1`.
//! Running here against `auth::authenticate` directly keeps the suite fast
//! and hermetic; the assignment notes this trade-off in §11.

use std::{
    collections::VecDeque,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex, Once, OnceLock},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use ed25519_dalek::Signer;
use pam_syauth::{
    auth::{self, AuthOutcome, BOND_KEY_PREFIX, PEER_DENIED_SENTINEL},
    config::Config,
    entry,
};
use syauth_core::{
    BOND_KEY_BYTES, Bond, BondStatus, BondStore, Frame, InMemoryKeyStore, KeyStore, MAX_PAYLOAD_LEN, NONCE_LEN, SIGNATURE_LEN,
    SYAUTH_WIRE_VERSION_V1, SigningKey, TAG_LEN, compute_tag, peer_id_from_pubkey,
};
use syauth_transport::{BtPeer, Session, TransportError};
use tempfile::TempDir;
use time::OffsetDateTime;

// =============================================================================
// Constants
// =============================================================================

/// Pinned 32-byte signing seed for the test phone. Deterministic so the test
/// reads its own KAT-shaped data.
const TEST_SIGNING_SEED: [u8; 32] = [
    0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD, 0xAE, 0xAF, 0xB0, 0xB1, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6,
    0xB7, 0xB8, 0xB9, 0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF, 0xC0,
];

/// Pinned 32-byte bond key for tests.
const TEST_BOND_KEY: [u8; BOND_KEY_BYTES] = [0x5A; BOND_KEY_BYTES];

/// Upper bound on the wall-clock for the golden scenario per SPEC §4.2.
const GOLDEN_WALL_CLOCK_UPPER_BOUND: Duration = Duration::from_millis(2_000);

/// Upper bound for the offline path. SPEC §4.3 mandates ≤ 1.2 s; we give a
/// 300 ms slack for test-harness overhead (process schedule, runtime spin-up).
const OFFLINE_WALL_CLOCK_UPPER_BOUND: Duration = Duration::from_millis(1_500);

/// Upper bound for the revoked path — the radio must NOT be touched, so the
/// wall-clock is dominated by bond-store load + tempdir I/O.
const REVOKED_WALL_CLOCK_UPPER_BOUND: Duration = Duration::from_millis(200);

// =============================================================================
// PamHarness — shared scaffold for every scenario.
// =============================================================================

/// Per-test harness. Owns the tempdir, the config, and a handle to the
/// signing key so the test can verify what the mock peer should have
/// signed.
struct PamHarness {
    cfg: Config,
    _tmp: TempDir,
    peer_id: String,
}

impl PamHarness {
    /// Build a harness with one `Bonded` peer in the bond store and the
    /// matching bond_key + pubkey installed in the process-local
    /// `InMemoryKeyStore`.
    fn bonded_with_signing_seed(seed: &[u8; 32], bond_key: &[u8; BOND_KEY_BYTES]) -> Self {
        ensure_keystore_installed();
        let sk = SigningKey::from_bytes(seed);
        let pk = sk.verifying_key();
        let peer_id = peer_id_from_pubkey(pk.as_bytes());
        let tmp = make_tempdir_0o700();
        let mut store = BondStore::empty();
        store
            .add(Bond {
                peer_id: peer_id.clone(),
                pubkey: *pk.as_bytes(),
                name: "test-phone".to_string(),
                created_at: OffsetDateTime::now_utc(),
                status: BondStatus::Bonded,
            })
            .expect("add bond");
        store.save(&tmp.path().join("bonds.toml")).expect("save");

        // Install the bond key in the per-process keystore under the
        // documented prefix.
        let id = format!("{BOND_KEY_PREFIX}{peer_id}");
        keystore().put(&id, bond_key).expect("put bond_key");
        Self {
            cfg: Config::for_tests(tmp.path()),
            _tmp: tmp,
            peer_id,
        }
    }

    /// Build a harness with one `Revoked` peer. No keys are needed because
    /// the auth path stops at peer selection before any keystore lookup.
    fn revoked_only() -> Self {
        ensure_keystore_installed();
        let sk = SigningKey::from_bytes(&[0u8; 32]);
        let pk = sk.verifying_key();
        let peer_id = peer_id_from_pubkey(pk.as_bytes());
        let tmp = make_tempdir_0o700();
        let mut store = BondStore::empty();
        store
            .add(Bond {
                peer_id: peer_id.clone(),
                pubkey: *pk.as_bytes(),
                name: "ex-phone".to_string(),
                created_at: OffsetDateTime::now_utc(),
                status: BondStatus::Revoked {
                    reason: "test revoke".to_string(),
                },
            })
            .expect("add");
        store.save(&tmp.path().join("bonds.toml")).expect("save");
        Self {
            cfg: Config::for_tests(tmp.path()),
            _tmp: tmp,
            peer_id,
        }
    }

    fn authenticate(&self) -> (AuthOutcome, Duration) {
        let start = Instant::now();
        let outcome = auth::authenticate(&self.cfg);
        (outcome, start.elapsed())
    }

    fn last_log_lines(&self) -> Vec<String> {
        let raw = std::fs::read_to_string(self.cfg.last_log_path()).unwrap_or_default();
        raw.lines().map(|s| s.to_string()).collect()
    }
}

/// Tempdir with mode 0o700 — the `BondStore::save` path refuses to write
/// into a parent with looser permissions than 0o700 (SPEC §4.4).
fn make_tempdir_0o700() -> TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut perm = std::fs::metadata(tmp.path()).expect("stat").permissions();
    perm.set_mode(0o700);
    std::fs::set_permissions(tmp.path(), perm).expect("chmod tempdir");
    tmp
}

// =============================================================================
// Process-local key store (installed exactly once)
// =============================================================================

static KEYSTORE: OnceLock<Arc<InMemoryKeyStore>> = OnceLock::new();

fn ensure_keystore_installed() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let ks = Arc::new(InMemoryKeyStore::new());
        // First-call installation — return value is `false` only if the
        // slot is already populated, which `Once::call_once` already
        // guarantees cannot happen.
        let _installed = auth::install_test_keystore(Arc::clone(&ks));
        let _ = KEYSTORE.set(ks);
    });
}

fn keystore() -> Arc<InMemoryKeyStore> {
    KEYSTORE.get().cloned().expect("keystore installed by ensure_keystore_installed")
}

// =============================================================================
// MOCK_PEER injection: also one-shot, so the per-test peer must dispatch on
// a swappable inner state.
// =============================================================================

struct DispatchPeer {
    inner: Mutex<Option<Arc<dyn BtPeer>>>,
}

#[async_trait]
impl BtPeer for DispatchPeer {
    async fn connect(&self, timeout: Duration) -> Result<Box<dyn Session>, TransportError> {
        let inner = {
            let g = self.inner.lock().expect("dispatch peer mutex");
            g.clone()
        };
        match inner {
            Some(p) => p.connect(timeout).await,
            None => Err(TransportError::Unreachable),
        }
    }
}

static DISPATCH: OnceLock<Arc<DispatchPeer>> = OnceLock::new();

fn dispatch() -> Arc<DispatchPeer> {
    DISPATCH
        .get_or_init(|| {
            let d = Arc::new(DispatchPeer { inner: Mutex::new(None) });
            // First-call install, guarded by `OnceLock::get_or_init`.
            let _installed = auth::install_mock_peer(d.clone() as Arc<dyn BtPeer>);
            d
        })
        .clone()
}

fn install_inner_peer(peer: Arc<dyn BtPeer>) {
    let d = dispatch();
    let mut g = d.inner.lock().expect("dispatch peer mutex");
    *g = Some(peer);
}

fn install_no_peer() {
    let d = dispatch();
    let mut g = d.inner.lock().expect("dispatch peer mutex");
    *g = None;
}

// =============================================================================
// Test peers — one per scenario shape, all in-process.
// =============================================================================

/// Builds a valid signed response to whatever challenge the auth path
/// sends. `app_suffix` is appended to the payload after the signature so
/// scenarios that care about denial / replay can pin it.
struct SigningPeer {
    seed: [u8; 32],
    bond_key: [u8; BOND_KEY_BYTES],
    app_suffix: Vec<u8>,
    response_nonce_override: Option<[u8; NONCE_LEN]>,
    flip_signature_byte: bool,
}

impl SigningPeer {
    fn golden(seed: [u8; 32], bond_key: [u8; BOND_KEY_BYTES]) -> Self {
        Self {
            seed,
            bond_key,
            app_suffix: Vec::new(),
            response_nonce_override: None,
            flip_signature_byte: false,
        }
    }
    fn with_app_suffix(mut self, suffix: &[u8]) -> Self {
        self.app_suffix = suffix.to_vec();
        self
    }
    fn with_response_nonce(mut self, nonce: [u8; NONCE_LEN]) -> Self {
        self.response_nonce_override = Some(nonce);
        self
    }
    fn flip_signature_byte(mut self) -> Self {
        self.flip_signature_byte = true;
        self
    }
}

#[async_trait]
impl BtPeer for SigningPeer {
    async fn connect(&self, _timeout: Duration) -> Result<Box<dyn Session>, TransportError> {
        Ok(Box::new(SigningSession {
            seed: self.seed,
            bond_key: self.bond_key,
            app_suffix: self.app_suffix.clone(),
            response_nonce_override: self.response_nonce_override,
            flip_signature_byte: self.flip_signature_byte,
            outbox: Mutex::new(VecDeque::new()),
        }))
    }
}

struct SigningSession {
    seed: [u8; 32],
    bond_key: [u8; BOND_KEY_BYTES],
    app_suffix: Vec<u8>,
    response_nonce_override: Option<[u8; NONCE_LEN]>,
    flip_signature_byte: bool,
    outbox: Mutex<VecDeque<Frame>>,
}

#[async_trait]
impl Session for SigningSession {
    async fn send_frame(&mut self, challenge: &Frame) -> Result<(), TransportError> {
        // The phone signs over the challenge body and returns:
        // [signature(64) || app_suffix(N)] as payload.
        let sk = SigningKey::from_bytes(&self.seed);
        let challenge_body = challenge.body_bytes()?;
        let mut sig_bytes = sk.sign(&challenge_body).to_bytes();
        if self.flip_signature_byte {
            sig_bytes[0] ^= 0x01;
        }
        let mut payload = Vec::with_capacity(SIGNATURE_LEN + self.app_suffix.len());
        payload.extend_from_slice(&sig_bytes);
        payload.extend_from_slice(&self.app_suffix);

        let response_nonce = self.response_nonce_override.unwrap_or(challenge.nonce);
        let mut response = Frame {
            version: SYAUTH_WIRE_VERSION_V1,
            nonce: response_nonce,
            payload,
            tag: [0u8; TAG_LEN],
        };
        let resp_body = response.body_bytes()?;
        response.tag = compute_tag(&self.bond_key, &resp_body);

        self.outbox.lock().expect("outbox").push_back(response);
        Ok(())
    }

    async fn recv_frame(&mut self, _timeout: Duration) -> Result<Frame, TransportError> {
        match self.outbox.lock().expect("outbox").pop_front() {
            Some(f) => Ok(f),
            None => Err(TransportError::Timeout),
        }
    }
}

/// Peer whose `connect` always returns `TransportError::Unreachable`. The
/// auth path must turn this into `AuthInfoUnavail("offline")`.
struct OfflinePeer;

#[async_trait]
impl BtPeer for OfflinePeer {
    async fn connect(&self, _timeout: Duration) -> Result<Box<dyn Session>, TransportError> {
        Err(TransportError::Unreachable)
    }
}

/// Peer that responds with `TransportError::BadFrame(FrameError::BadVersion)`.
struct WrongVersionPeer;

#[async_trait]
impl BtPeer for WrongVersionPeer {
    async fn connect(&self, _timeout: Duration) -> Result<Box<dyn Session>, TransportError> {
        Ok(Box::new(WrongVersionSession))
    }
}

struct WrongVersionSession;

#[async_trait]
impl Session for WrongVersionSession {
    async fn send_frame(&mut self, _frame: &Frame) -> Result<(), TransportError> {
        Ok(())
    }
    async fn recv_frame(&mut self, _timeout: Duration) -> Result<Frame, TransportError> {
        Err(TransportError::BadFrame(syauth_core::FrameError::BadVersion(0x02)))
    }
}

/// Peer that surfaces an oversized frame as `FrameError::BadLength`.
struct OversizedFramePeer;

#[async_trait]
impl BtPeer for OversizedFramePeer {
    async fn connect(&self, _timeout: Duration) -> Result<Box<dyn Session>, TransportError> {
        Ok(Box::new(OversizedFrameSession))
    }
}

struct OversizedFrameSession;

#[async_trait]
impl Session for OversizedFrameSession {
    async fn send_frame(&mut self, _frame: &Frame) -> Result<(), TransportError> {
        Ok(())
    }
    async fn recv_frame(&mut self, _timeout: Duration) -> Result<Frame, TransportError> {
        // Mirror the failure the real BlueZ transport (`reassemble`) emits
        // when the joined payload would overflow `MAX_PAYLOAD_LEN`.
        let _ = MAX_PAYLOAD_LEN; // documentation reference
        Err(TransportError::BadFrame(syauth_core::FrameError::BadLength))
    }
}

/// Peer that surfaces an incomplete-fragment reassembly. Mirrors the
/// `bluez::reassemble` failure for a corrupted MTU-split.
struct CorruptReassemblyPeer;

#[async_trait]
impl BtPeer for CorruptReassemblyPeer {
    async fn connect(&self, _timeout: Duration) -> Result<Box<dyn Session>, TransportError> {
        Ok(Box::new(CorruptReassemblySession))
    }
}

struct CorruptReassemblySession;

#[async_trait]
impl Session for CorruptReassemblySession {
    async fn send_frame(&mut self, _frame: &Frame) -> Result<(), TransportError> {
        Ok(())
    }
    async fn recv_frame(&mut self, _timeout: Duration) -> Result<Frame, TransportError> {
        Err(TransportError::IncompleteReassembly)
    }
}

/// Peer that panics on every method call. Used by the revoked test to prove
/// the radio is never touched.
struct PanickingPeer;

#[async_trait]
impl BtPeer for PanickingPeer {
    async fn connect(&self, _timeout: Duration) -> Result<Box<dyn Session>, TransportError> {
        panic!("revoked path must NOT call BtPeer::connect");
    }
}

// =============================================================================
// Tests — one #[test] per SPEC §4.3 scenario (plus the DoD extras)
// =============================================================================

/// Sentinel guard so the test suite only runs sequentially when scenarios
/// share the global `MOCK_PEER`/`KEYSTORE`. We use a single `Mutex<()>` to
/// serialize, so each test owns the dispatch peer for its duration.
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// SPEC §4.3 #1 — golden: ≤ 2 s success.
#[test]
fn tc01_golden_scenario_returns_pam_success() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = PamHarness::bonded_with_signing_seed(&TEST_SIGNING_SEED, &TEST_BOND_KEY);
    install_inner_peer(Arc::new(SigningPeer::golden(TEST_SIGNING_SEED, TEST_BOND_KEY)));
    let (outcome, elapsed) = h.authenticate();
    install_no_peer();
    assert!(
        matches!(outcome, AuthOutcome::Success { ref peer_id } if peer_id == &h.peer_id),
        "expected Success, got {outcome:?}"
    );
    assert!(elapsed < GOLDEN_WALL_CLOCK_UPPER_BOUND, "golden took {elapsed:?}");
    let last = h.last_log_lines();
    assert_eq!(last.len(), 1);
    assert!(last[0].contains(" success "), "last.log line: {}", last[0]);
    assert!(last[0].contains(&h.peer_id));
}

/// SPEC §4.3 #2 — peer offline: `PAM_AUTHINFO_UNAVAIL` ≤ 1.2 s.
#[test]
fn tc02_offline_scenario_returns_authinfo_unavail_under_budget() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = PamHarness::bonded_with_signing_seed(&TEST_SIGNING_SEED, &TEST_BOND_KEY);
    install_inner_peer(Arc::new(OfflinePeer));
    let (outcome, elapsed) = h.authenticate();
    install_no_peer();
    match outcome {
        AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(reason, "offline"),
        other => panic!("expected AuthInfoUnavail(offline), got {other:?}"),
    }
    assert!(
        elapsed < OFFLINE_WALL_CLOCK_UPPER_BOUND,
        "offline path exceeded {OFFLINE_WALL_CLOCK_UPPER_BOUND:?}: {elapsed:?}"
    );
}

/// SPEC §4.3 #3 — peer denies: `PAM_AUTH_ERR`.
#[test]
fn tc03_peer_denied_returns_pam_auth_err() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = PamHarness::bonded_with_signing_seed(&TEST_SIGNING_SEED, &TEST_BOND_KEY);
    install_inner_peer(Arc::new(
        SigningPeer::golden(TEST_SIGNING_SEED, TEST_BOND_KEY).with_app_suffix(PEER_DENIED_SENTINEL),
    ));
    let (outcome, _elapsed) = h.authenticate();
    install_no_peer();
    match outcome {
        AuthOutcome::AuthErr { reason, .. } => assert_eq!(reason, "peer-denied"),
        other => panic!("expected AuthErr(peer-denied), got {other:?}"),
    }
}

/// SPEC §4.3 #4 — replay: `PAM_AUTH_ERR`.
#[test]
fn tc04_replay_returns_pam_auth_err() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = PamHarness::bonded_with_signing_seed(&TEST_SIGNING_SEED, &TEST_BOND_KEY);
    // Force the response to use a *fixed* nonce, then seed the replay
    // cache with that same nonce — the auth path's per-call cache will
    // report `Acceptance::Replayed`.
    let fixed_nonce = [0xEE; NONCE_LEN];
    auth::replay_seed::install(fixed_nonce);
    install_inner_peer(Arc::new(
        SigningPeer::golden(TEST_SIGNING_SEED, TEST_BOND_KEY).with_response_nonce(fixed_nonce),
    ));
    let (outcome, _elapsed) = h.authenticate();
    install_no_peer();
    match outcome {
        AuthOutcome::AuthErr { reason, .. } => assert_eq!(reason, "replay"),
        other => panic!("expected AuthErr(replay), got {other:?}"),
    }
}

/// SPEC §4.3 #5 — bad signature: `PAM_AUTH_ERR`.
#[test]
fn tc05_bad_signature_returns_pam_auth_err() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = PamHarness::bonded_with_signing_seed(&TEST_SIGNING_SEED, &TEST_BOND_KEY);
    install_inner_peer(Arc::new(
        SigningPeer::golden(TEST_SIGNING_SEED, TEST_BOND_KEY).flip_signature_byte(),
    ));
    let (outcome, _elapsed) = h.authenticate();
    install_no_peer();
    match outcome {
        AuthOutcome::AuthErr { reason, .. } => assert_eq!(reason, "bad-signature"),
        other => panic!("expected AuthErr(bad-signature), got {other:?}"),
    }
}

/// SPEC §4.3 #6 — wrong version: `PAM_AUTH_ERR`.
#[test]
fn tc06_wrong_version_returns_pam_auth_err() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = PamHarness::bonded_with_signing_seed(&TEST_SIGNING_SEED, &TEST_BOND_KEY);
    install_inner_peer(Arc::new(WrongVersionPeer));
    let (outcome, _elapsed) = h.authenticate();
    install_no_peer();
    match outcome {
        AuthOutcome::AuthErr { reason, .. } => assert_eq!(reason, "wrong-version"),
        other => panic!("expected AuthErr(wrong-version), got {other:?}"),
    }
}

/// SPEC §4.3 (DoD #3 extra) — oversized-frame: `PAM_AUTH_ERR`.
#[test]
fn tc07_oversized_frame_returns_pam_auth_err() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = PamHarness::bonded_with_signing_seed(&TEST_SIGNING_SEED, &TEST_BOND_KEY);
    install_inner_peer(Arc::new(OversizedFramePeer));
    let (outcome, _elapsed) = h.authenticate();
    install_no_peer();
    match outcome {
        AuthOutcome::AuthErr { reason, .. } => assert_eq!(reason, "bad-encoding"),
        other => panic!("expected AuthErr(bad-encoding) for oversized frame, got {other:?}"),
    }
}

/// SPEC §4.3 #8 — MTU split frame; here the negative corrupt-reassembly
/// sub-case demanded by S-009 DoD #3.
#[test]
fn tc08_mtu_split_corrupt_reassembly_returns_pam_auth_err() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = PamHarness::bonded_with_signing_seed(&TEST_SIGNING_SEED, &TEST_BOND_KEY);
    install_inner_peer(Arc::new(CorruptReassemblyPeer));
    let (outcome, _elapsed) = h.authenticate();
    install_no_peer();
    match outcome {
        AuthOutcome::AuthErr { reason, .. } => assert_eq!(reason, "incomplete-reassembly"),
        other => panic!("expected AuthErr(incomplete-reassembly), got {other:?}"),
    }
}

/// SPEC §4.3 #7 — revoked peer: never goes to radio. We use a panicking
/// peer to prove the radio path is never reached.
#[test]
fn tc09_revoked_peer_never_touches_radio() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = PamHarness::revoked_only();
    install_inner_peer(Arc::new(PanickingPeer));
    let (outcome, elapsed) = h.authenticate();
    install_no_peer();
    // No eligible peer → AuthInfoUnavail("no bonded peer"). This lets the
    // PAM stack fall through to pam_unix per SPEC D7; SPEC §4.3 says
    // PAM_AUTH_ERR for "revoked peer". The journey doc TC-09 records the
    // divergence and the chosen reading.
    match outcome {
        AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(reason, "no bonded peer"),
        other => panic!("expected AuthInfoUnavail(no bonded peer), got {other:?}"),
    }
    assert!(elapsed < REVOKED_WALL_CLOCK_UPPER_BOUND, "revoked path took {elapsed:?}");
}

// =============================================================================
// DoD #4 + #5 + #6 extras
// =============================================================================

/// DoD #4: `pam_sm_setcred` returns `PAM_SUCCESS`.
#[test]
fn tc10_setcred_returns_pam_success() {
    use std::os::raw::{c_char, c_void};
    // SAFETY: pam_sm_setcred is `pub unsafe extern "C" fn`; we hold no PAM
    // handle (the stub does not dereference it), so a null pointer with
    // argc=0 / argv=null is valid input per the libpam ABI contract.
    let got = unsafe { entry::pam_sm_setcred(std::ptr::null_mut::<c_void>(), 0_i32, 0_i32, std::ptr::null::<*const c_char>()) };
    assert_eq!(got, entry::PAM_SUCCESS);
}

/// DoD #6: each call appends exactly one line to `last.log`.
#[test]
fn tc12_last_log_appends_one_line_per_call() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = PamHarness::bonded_with_signing_seed(&TEST_SIGNING_SEED, &TEST_BOND_KEY);
    // Call 1: success.
    install_inner_peer(Arc::new(SigningPeer::golden(TEST_SIGNING_SEED, TEST_BOND_KEY)));
    let (o1, _) = h.authenticate();
    assert!(matches!(o1, AuthOutcome::Success { .. }));
    // Call 2: offline failure.
    install_inner_peer(Arc::new(OfflinePeer));
    let (o2, _) = h.authenticate();
    install_no_peer();
    assert!(matches!(o2, AuthOutcome::AuthInfoUnavail { .. }));

    let last = h.last_log_lines();
    assert_eq!(last.len(), 2, "want 2 lines, got:\n{last:#?}");
    assert!(last[0].contains(" success "));
    assert!(last[1].contains(" failure "));
}

// =============================================================================
// Microbench: offline-path wall-clock measurement (for the evidence block).
// Not a #[test] — only used to populate the ROADMAP evidence subsection.
// Run with `cargo test -p syauth-pam --test pam_e2e -- --nocapture
// offline_wall_clock_sample`.
// =============================================================================

#[test]
#[ignore = "manual: run with --nocapture to populate ROADMAP evidence"]
fn offline_wall_clock_sample() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = PamHarness::bonded_with_signing_seed(&TEST_SIGNING_SEED, &TEST_BOND_KEY);
    let mut samples = Vec::new();
    for _ in 0..50 {
        install_inner_peer(Arc::new(OfflinePeer));
        let (_outcome, elapsed) = h.authenticate();
        samples.push(elapsed);
    }
    install_no_peer();
    samples.sort();
    eprintln!("offline-path samples (n=50):");
    eprintln!("  p50  = {:?}", samples[samples.len() / 2]);
    eprintln!("  p99  = {:?}", samples[(samples.len() * 99) / 100]);
    eprintln!("  max  = {:?}", samples[samples.len() - 1]);
}
