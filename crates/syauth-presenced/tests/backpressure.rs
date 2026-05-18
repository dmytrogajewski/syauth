// Journey: specs/journeys/JOURNEY-S-007-nonce-lru-backpressure.md
// Roadmap row: specs/unlock-proximity/ROADMAP.md Step S-007.
//
// Integration test for the SPEC §3 scope item #7 per-peer
// backpressure:
//   TC-03 — second_in_flight_request_returns_busy_after_1s:
//           two concurrent `issue_challenge` calls for the SAME
//           peer; the first acquires the semaphore and parks on
//           `wait_for_response` (no injected response — it never
//           completes within the test). The second hits the
//           semaphore, waits up to `BUSY_QUEUE_DEADLINE = 1 s`,
//           and returns `ChallengeOutcome::Busy`. Uses
//           `tokio::test(start_paused = true)` + `tokio::time::advance`
//           so CI wall-clock cost is sub-second.

use std::{path::PathBuf, sync::Arc, time::Duration};

use syauth_core::{Bond, BondStatus, SigningKey, bond::PUBKEY_LEN, peer_id_from_pubkey};
use syauth_presenced::{
    AuditLog, BUSY_QUEUE_DEADLINE, BUSY_REASON, ChallengeOutcome, DEFAULT_AUTH_TIMEOUT, OUTCOME_REASON_BUSY, Orchestrator,
};
use syauth_transport::{BOND_KEY_BYTES, FakePeripheral, Peripheral};
use tempfile::TempDir;
use time::OffsetDateTime;
use tokio::time::Instant;

/// Deterministic Ed25519 signing-key seed for the backpressure
/// fixture peer. The signing happens to be unused in this test
/// (the first task never receives a response), but the
/// orchestrator still needs a valid `phone_pubkey` so the peer
/// passes the `Orchestrator::lookup_peer` check.
const SIGNING_KEY_SEED: [u8; 32] = [
    0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x4b, 0x4c, 0x4d, 0x4e, 0x4f, 0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56,
    0x57, 0x58, 0x59, 0x5a, 0x5b, 0x5c, 0x5d, 0x5e, 0x5f, 0x60,
];

/// Deterministic bond key used by the backpressure test.
const BOND_KEY: [u8; BOND_KEY_BYTES] = [0xBB; BOND_KEY_BYTES];

/// Extra virtual time advanced past `BUSY_QUEUE_DEADLINE` so the
/// timeout inside the semaphore acquire fires deterministically.
const BUSY_DEADLINE_SLOP: Duration = Duration::from_millis(50);

/// Settle period between spawning task A and spawning task B so
/// task A is guaranteed to have acquired the semaphore permit
/// before task B contends for it. Virtual time under
/// `start_paused = true`.
const SPAWN_SETTLE: Duration = Duration::from_millis(20);

fn fixture_bond(signing_key: &SigningKey) -> Bond {
    let pubkey_bytes = signing_key.verifying_key().to_bytes();
    let mut pubkey = [0u8; PUBKEY_LEN];
    pubkey.copy_from_slice(&pubkey_bytes);
    Bond {
        peer_id: peer_id_from_pubkey(&pubkey),
        pubkey,
        name: "backpressure-test".to_owned(),
        created_at: OffsetDateTime::UNIX_EPOCH,
        status: BondStatus::Bonded,
    }
}

fn open_audit_log(tempdir: &TempDir) -> (AuditLog, PathBuf) {
    let path = tempdir.path().join("last.log");
    let log = AuditLog::open(&path).expect("open audit log");
    (log, path)
}

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
    fake.add_peer(&bond.peer_id, &BOND_KEY).await.expect("fake add_peer");
    orchestrator
}

#[tokio::test(start_paused = true)]
async fn second_in_flight_request_returns_busy_after_1s() {
    let signing_key = SigningKey::from_bytes(&SIGNING_KEY_SEED);
    let bond = fixture_bond(&signing_key);
    let td = TempDir::new().expect("tempdir");
    let (audit_log, _audit_path) = open_audit_log(&td);
    let fake = FakePeripheral::new();
    let orchestrator = build_orchestrator(fake.clone(), bond.clone(), audit_log).await;

    // Task A: never gets a response, parks on wait_for_response
    // inside the orchestrator. The semaphore permit is held by
    // task A for the lifetime of this test.
    let orch_a = Arc::clone(&orchestrator);
    let peer_a = bond.peer_id.clone();
    // Give task A a `deadline` LONGER than BUSY_QUEUE_DEADLINE +
    // the slop so the inner wait_for_response does not itself
    // resolve before task B's semaphore timeout fires.
    let long_deadline = BUSY_QUEUE_DEADLINE * 10;
    let task_a = tokio::spawn(async move { orch_a.issue_challenge(&peer_a, long_deadline).await });

    // Let task A acquire the semaphore and park on
    // wait_for_response before we contend.
    tokio::time::advance(SPAWN_SETTLE).await;

    // Task B: contends for the semaphore. Under start_paused the
    // tokio runtime parks task B on the semaphore queue
    // immediately; advancing virtual time past
    // BUSY_QUEUE_DEADLINE fires the inner `tokio::time::timeout`
    // and returns Busy.
    let orch_b = Arc::clone(&orchestrator);
    let peer_b = bond.peer_id.clone();
    let task_b = tokio::spawn(async move { orch_b.issue_challenge(&peer_b, DEFAULT_AUTH_TIMEOUT).await });

    // Advance past the busy-queue deadline so task B's
    // semaphore-acquire timeout elapses.
    tokio::time::advance(BUSY_QUEUE_DEADLINE + BUSY_DEADLINE_SLOP).await;

    let outcome_b = task_b.await.expect("task B joined");
    match outcome_b {
        ChallengeOutcome::Busy => {}
        other => panic!("expected ChallengeOutcome::Busy, got {other:?}"),
    }
    assert_eq!(outcome_b.reason_str(), BUSY_REASON);
    assert_eq!(outcome_b.reason_str(), OUTCOME_REASON_BUSY);

    // Task A must still be parked — its semaphore permit was not
    // released by task B's Busy outcome.
    assert!(
        !task_a.is_finished(),
        "task A must remain parked after task B times out on the busy queue"
    );
    task_a.abort();
}
