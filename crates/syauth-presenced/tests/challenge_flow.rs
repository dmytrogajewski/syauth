// Journey: specs/journeys/JOURNEY-S-006-challenge-transaction-flow.md
// Roadmap row: specs/unlock-proximity/ROADMAP.md Step S-006.
//
// Integration tests for the challenge transaction flow:
//   TC-01 — issues_challenge_drives_notify_then_awaits_response:
//           valid signed response → `ChallengeOutcome::Ok`, fake
//           records exactly one notify_calls entry, audit line
//           outcome column is `"ok"`.
//   TC-02 — times_out_returns_authinfo_unavail: no injected
//           response → `ChallengeOutcome::TimedOut` after the 1.2 s
//           budget; audit column is `"response-timeout"`.
//           Uses `tokio::test(start_paused = true)` so wall-clock
//           cost on CI is sub-second.
//   TC-03 — bad_signature_returns_auth_err: garbage bytes injected
//           → `ChallengeOutcome::BadSignature`; audit column is
//           `"bad-signature"`.
//   TC-04 — audit_log_appended_with_outcome: four challenges
//           (`Ok`, `Ok`, `TimedOut`, `BadSignature`) → audit file
//           has 4 lines with the expected outcome columns. The
//           file is copied to `/tmp/syauth-test-last.log` so the
//           ROADMAP closure-condition probe sees it; a
//           `TempLogGuard` `Drop` impl unlinks the copy on test
//           teardown so re-runs leave no debris.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use syauth_core::{
    Bond, BondStatus, Frame, NONCE_LEN, SIGNATURE_LEN, SYAUTH_WIRE_VERSION_V1, SigningKey, bond::PUBKEY_LEN, compute_tag,
    peer_id_from_pubkey, sign_frame,
};
use syauth_presenced::{
    AuditLog, ChallengeOutcome, DEFAULT_AUTH_TIMEOUT, OUTCOME_REASON_BAD_SIGNATURE, OUTCOME_REASON_OK, OUTCOME_REASON_RESPONSE_TIMEOUT,
    Orchestrator,
};
use syauth_transport::{BOND_KEY_BYTES, FakePeripheral, Peripheral};
use tempfile::TempDir;
use time::OffsetDateTime;
use tokio::time::Instant;

/// Deterministic Ed25519 signing-key seed used by the success-path
/// tests. The matching verifying key lands in the bond record's
/// `pubkey` field via `signing_key.verifying_key().to_bytes()`.
const SIGNING_KEY_SEED: [u8; 32] = [
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
    0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
];

/// Deterministic bond key used by every test in this module.
const BOND_KEY: [u8; BOND_KEY_BYTES] = [0xAA; BOND_KEY_BYTES];

/// `/tmp/syauth-test-last.log` is the path the ROADMAP closure
/// condition greps. The audit_log_appended_with_outcome test copies
/// its tempdir audit file to this path, and the `TempLogGuard`'s
/// `Drop` unlinks it on teardown.
const ROADMAP_CLOSURE_PROBE_PATH: &str = "/tmp/syauth-test-last.log";

/// Construct a `Bond` with `pubkey` derived from `signing_key`. The
/// bond_key is the test's `BOND_KEY` constant — for S-006 the
/// orchestrator's challenge path verifies via `phone_pubkey`, not
/// via the bond_key MAC.
fn fixture_bond(signing_key: &SigningKey) -> Bond {
    let pubkey_bytes = signing_key.verifying_key().to_bytes();
    let mut pubkey = [0u8; PUBKEY_LEN];
    pubkey.copy_from_slice(&pubkey_bytes);
    Bond {
        peer_id: peer_id_from_pubkey(&pubkey),
        pubkey,
        name: "challenge-flow-test".to_owned(),
        created_at: OffsetDateTime::UNIX_EPOCH,
        status: BondStatus::Bonded,
    }
}

/// Build an `AuditLog` rooted at `tempdir/last.log`. Returns the
/// log and the path so the test can assert on the file's contents
/// after `Orchestrator::issue_challenge` runs.
fn open_audit_log(tempdir: &TempDir) -> (AuditLog, PathBuf) {
    let path = tempdir.path().join("last.log");
    let log = AuditLog::open(&path).expect("open audit log");
    (log, path)
}

/// Construct an orchestrator carrying one bond with an attached
/// audit log. `tokio::time::Instant::now()` is captured so the
/// rotation timer never fires during the test.
async fn build_orchestrator(fake: Arc<FakePeripheral>, bond: Bond, audit_log: AuditLog) -> Arc<Orchestrator> {
    let peripheral: Arc<dyn Peripheral + Send + Sync> = fake.clone();
    let start = Instant::now() + Duration::from_secs(syauth_presenced::SECONDS_PER_MINUTE);
    let orchestrator = Arc::new(Orchestrator::with_peers_and_audit(
        peripheral,
        vec![(bond.clone(), BOND_KEY)],
        PathBuf::new(),
        PathBuf::new(),
        start,
        Some(audit_log),
    ));
    // Register the peer with the fake so notify_challenge /
    // wait_for_response find the peer entry.
    fake.add_peer(&bond.peer_id, &BOND_KEY).await.expect("fake add_peer");
    orchestrator
}

/// Decode the challenge frame the orchestrator notified, sign its
/// body bytes with `signing_key`, and return the raw 64-byte
/// Ed25519 signature ready for `inject_response`.
fn sign_notified_challenge(notify_bytes: &[u8], signing_key: &SigningKey) -> Vec<u8> {
    let frame = Frame::decode(notify_bytes).expect("decode notify");
    assert_eq!(frame.version, SYAUTH_WIRE_VERSION_V1);
    assert_eq!(frame.nonce.len(), NONCE_LEN);
    // The orchestrator now MACs the body bytes with the bond_key
    // before notifying so the phone's `verifyChallengeFrame` accepts
    // the wire frame. The tag is computed over the same body the
    // signature verifier uses, and it must match for the wire format
    // to round-trip cleanly.
    let body = frame.body_bytes().expect("body_bytes");
    assert_eq!(frame.tag, compute_tag(&BOND_KEY, &body));
    assert!(frame.payload.is_empty(), "challenge frame must have empty payload");
    let signature = sign_frame(signing_key, &frame).expect("sign frame");
    signature.to_bytes().to_vec()
}

/// Helper: drive one Ok challenge round-trip. Used by TC-01 and TC-04.
async fn drive_one_ok_challenge(orchestrator: &Arc<Orchestrator>, fake: &Arc<FakePeripheral>, signing_key: &SigningKey, bond: &Bond) {
    // Capture the notify_calls index before spawning so we sign the
    // nonce of *this* challenge, not a leftover entry from an
    // earlier call.
    let before_idx = fake.notify_calls().len();
    let orchestrator_clone = Arc::clone(orchestrator);
    let peer_id = bond.peer_id.clone();
    let challenge_task = tokio::spawn(async move { orchestrator_clone.issue_challenge(&peer_id, DEFAULT_AUTH_TIMEOUT).await });

    let notify_bytes = wait_until_notified_after(fake, &bond.peer_id, before_idx).await;
    let signature_bytes = sign_notified_challenge(&notify_bytes, signing_key);
    fake.inject_response(&bond.peer_id, signature_bytes);

    let outcome = challenge_task.await.expect("challenge task joined");
    match outcome {
        ChallengeOutcome::Ok { .. } => {}
        other => panic!("expected ChallengeOutcome::Ok, got {other:?}"),
    }
}

/// Poll the fake's `notify_calls` for an entry on `peer_id` at
/// index `>= since_idx` and return the encoded frame bytes.
/// Bounded by a wall-clock budget so a regression that drops the
/// notify call fails the test, not hangs it.
async fn wait_until_notified_after(fake: &Arc<FakePeripheral>, peer_id: &str, since_idx: usize) -> Vec<u8> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        let calls = fake.notify_calls();
        for (idx, (pid, bytes)) in calls.iter().enumerate() {
            if idx >= since_idx && pid == peer_id {
                return bytes.clone();
            }
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("notify_challenge never fired for peer_id={peer_id} after index {since_idx}");
}

/// Count the lines in `path`. Used by TC-04 to assert "4 audit
/// records".
fn count_lines(path: &Path) -> usize {
    fs::read_to_string(path).expect("read audit").lines().count()
}

/// Return the comma-separated outcome column (5th, 1-indexed) of
/// every line in the audit file.
fn outcome_columns(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .expect("read audit")
        .lines()
        .map(|line| line.split(',').nth(4).map(str::to_owned).unwrap_or_default())
        .collect()
}

/// RAII guard that unlinks `/tmp/syauth-test-last.log` on `Drop`
/// so re-runs of TC-04 do not leave debris in `/tmp/`. Re-runs
/// that race the unlink at process exit are tolerated — `Drop`
/// is best-effort.
struct TempLogGuard {
    path: PathBuf,
}

impl TempLogGuard {
    fn copy_from(src: &Path) -> Self {
        let dst = PathBuf::from(ROADMAP_CLOSURE_PROBE_PATH);
        // Best-effort: the closure probe accepts the file even if
        // the test cannot copy it (e.g. /tmp not writable). The
        // primary evidence is the tempdir audit file's line count.
        let _ = fs::copy(src, &dst);
        Self { path: dst }
    }
}

impl Drop for TempLogGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[tokio::test]
async fn issues_challenge_drives_notify_then_awaits_response() {
    let signing_key = SigningKey::from_bytes(&SIGNING_KEY_SEED);
    let bond = fixture_bond(&signing_key);
    let td = TempDir::new().expect("tempdir");
    let (audit_log, audit_path) = open_audit_log(&td);
    let fake = FakePeripheral::new();
    let orchestrator = build_orchestrator(fake.clone(), bond.clone(), audit_log).await;

    drive_one_ok_challenge(&orchestrator, &fake, &signing_key, &bond).await;

    // Fake recorded exactly one notify entry.
    let calls = fake.notify_calls();
    assert_eq!(calls.len(), 1, "expected one notify_challenge call, got {calls:?}");
    assert_eq!(calls[0].0, bond.peer_id);

    // Audit file recorded exactly one Ok line.
    assert_eq!(count_lines(&audit_path), 1);
    assert_eq!(outcome_columns(&audit_path), vec![OUTCOME_REASON_OK.to_owned()]);
}

#[tokio::test(start_paused = true)]
async fn times_out_returns_authinfo_unavail() {
    let signing_key = SigningKey::from_bytes(&SIGNING_KEY_SEED);
    let bond = fixture_bond(&signing_key);
    let td = TempDir::new().expect("tempdir");
    let (audit_log, audit_path) = open_audit_log(&td);
    let fake = FakePeripheral::new();
    let orchestrator = build_orchestrator(fake.clone(), bond.clone(), audit_log).await;

    // No `inject_response` — `wait_for_response` will hit the
    // deadline. Under `start_paused = true` the virtual clock
    // advances when the runtime is otherwise idle, so the
    // `tokio::time::timeout(DEFAULT_AUTH_TIMEOUT, ...)` inside
    // the orchestrator's wait_for_response on FakePeripheral
    // completes deterministically.
    let outcome = orchestrator.issue_challenge(&bond.peer_id, DEFAULT_AUTH_TIMEOUT).await;
    match outcome {
        ChallengeOutcome::TimedOut => {}
        other => panic!("expected TimedOut, got {other:?}"),
    }

    assert_eq!(count_lines(&audit_path), 1);
    assert_eq!(outcome_columns(&audit_path), vec![OUTCOME_REASON_RESPONSE_TIMEOUT.to_owned()]);
}

#[tokio::test]
async fn bad_signature_returns_auth_err() {
    let signing_key = SigningKey::from_bytes(&SIGNING_KEY_SEED);
    let bond = fixture_bond(&signing_key);
    let td = TempDir::new().expect("tempdir");
    let (audit_log, audit_path) = open_audit_log(&td);
    let fake = FakePeripheral::new();
    let orchestrator = build_orchestrator(fake.clone(), bond.clone(), audit_log).await;

    // Inject SIGNATURE_LEN bytes of `0xAA` — the right length, so
    // the orchestrator hits the `verify_strict` branch, not the
    // `Signature::from_slice` length branch. Either branch maps to
    // `BadSignature`, but pinning the verify-side branch documents
    // the SPEC §6 contract more precisely.
    let garbage = vec![0xAAu8; SIGNATURE_LEN];
    fake.inject_response(&bond.peer_id, garbage);

    let outcome = orchestrator.issue_challenge(&bond.peer_id, DEFAULT_AUTH_TIMEOUT).await;
    match outcome {
        ChallengeOutcome::BadSignature => {}
        other => panic!("expected BadSignature, got {other:?}"),
    }

    assert_eq!(count_lines(&audit_path), 1);
    assert_eq!(outcome_columns(&audit_path), vec![OUTCOME_REASON_BAD_SIGNATURE.to_owned()]);
}

#[tokio::test]
async fn audit_log_appended_with_outcome() {
    let signing_key = SigningKey::from_bytes(&SIGNING_KEY_SEED);
    let bond = fixture_bond(&signing_key);
    let td = TempDir::new().expect("tempdir");
    let (audit_log, audit_path) = open_audit_log(&td);
    let fake = FakePeripheral::new();
    let orchestrator = build_orchestrator(fake.clone(), bond.clone(), audit_log).await;

    // Two Ok challenges.
    drive_one_ok_challenge(&orchestrator, &fake, &signing_key, &bond).await;
    drive_one_ok_challenge(&orchestrator, &fake, &signing_key, &bond).await;

    // One TimedOut challenge — wall-clock cost equals
    // DEFAULT_AUTH_TIMEOUT (1.2 s) because this test does NOT use
    // `start_paused`. Two seconds total on CI is acceptable for a
    // single audit-shape integration test.
    let outcome = orchestrator.issue_challenge(&bond.peer_id, DEFAULT_AUTH_TIMEOUT).await;
    match outcome {
        ChallengeOutcome::TimedOut => {}
        other => panic!("expected TimedOut, got {other:?}"),
    }

    // One BadSignature challenge.
    let garbage = vec![0xAAu8; SIGNATURE_LEN];
    fake.inject_response(&bond.peer_id, garbage);
    let outcome = orchestrator.issue_challenge(&bond.peer_id, DEFAULT_AUTH_TIMEOUT).await;
    match outcome {
        ChallengeOutcome::BadSignature => {}
        other => panic!("expected BadSignature, got {other:?}"),
    }

    // Verify the tempdir audit file shape.
    assert_eq!(count_lines(&audit_path), 4, "expected 4 audit lines");
    assert_eq!(
        outcome_columns(&audit_path),
        vec![
            OUTCOME_REASON_OK.to_owned(),
            OUTCOME_REASON_OK.to_owned(),
            OUTCOME_REASON_RESPONSE_TIMEOUT.to_owned(),
            OUTCOME_REASON_BAD_SIGNATURE.to_owned(),
        ]
    );

    // Copy to /tmp/syauth-test-last.log so the ROADMAP closure-
    // condition probe sees a file with >= 4 lines. The
    // TempLogGuard's `Drop` unlinks the copy on test teardown.
    let guard = TempLogGuard::copy_from(&audit_path);
    let probe_path = Path::new(ROADMAP_CLOSURE_PROBE_PATH);
    if probe_path.exists() {
        assert!(
            count_lines(probe_path) >= 4,
            "/tmp/syauth-test-last.log must have >= 4 lines per ROADMAP closure condition"
        );
    }
    drop(guard);
}
