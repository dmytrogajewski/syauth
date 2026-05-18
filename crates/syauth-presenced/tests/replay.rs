// Journey: specs/journeys/JOURNEY-S-007-nonce-lru-backpressure.md
// Roadmap row: specs/unlock-proximity/ROADMAP.md Step S-007.
//
// Integration tests for the SPEC §6 idempotency LRU:
//   TC-01 — repeated_nonce_returns_replay: forcing the SAME nonce
//           on two consecutive challenges (test-only entry point
//           `issue_challenge_with_nonce`) — first returns Ok,
//           second returns Replay with `reason="replay"` audit row.
//   TC-02 — lru_evicts_oldest_nonce_at_cap_65: pure-data test on
//           `NonceCache::insert` showing the first nonce is evicted
//           once the cap+1th nonce is inserted.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use syauth_core::{
    Bond, BondStatus, Frame, NONCE_LEN, SYAUTH_WIRE_VERSION_V1, SigningKey, TAG_LEN, bond::PUBKEY_LEN, peer_id_from_pubkey, sign_frame,
};
use syauth_presenced::{
    AuditLog, ChallengeOutcome, DEFAULT_AUTH_TIMEOUT, NONCE_BYTES, NONCE_LRU_CAP, NonceCache, OUTCOME_REASON_OK, OUTCOME_REASON_REPLAY,
    Orchestrator,
};
use syauth_transport::{BOND_KEY_BYTES, FakePeripheral, Peripheral};
use tempfile::TempDir;
use time::OffsetDateTime;
use tokio::time::Instant;

/// Deterministic Ed25519 signing-key seed shared with the S-006
/// `challenge_flow.rs` tests so a single fixture key produces both
/// suites' verifying keys.
const SIGNING_KEY_SEED: [u8; 32] = [
    0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36,
    0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e, 0x3f, 0x40,
];

/// Deterministic bond key used by every test in this module.
const BOND_KEY: [u8; BOND_KEY_BYTES] = [0xCC; BOND_KEY_BYTES];

/// Fixed nonce `A` used by TC-01 to force the LRU collision on the
/// second `issue_challenge_with_nonce` call.
const FIXED_NONCE_A: [u8; NONCE_BYTES] = [0xA7; NONCE_BYTES];

/// Construct a `Bond` whose `pubkey` is derived from `signing_key`
/// so a test-side `sign_frame` produces signatures that
/// `Orchestrator::issue_challenge_with_nonce` verifies.
fn fixture_bond(signing_key: &SigningKey) -> Bond {
    let pubkey_bytes = signing_key.verifying_key().to_bytes();
    let mut pubkey = [0u8; PUBKEY_LEN];
    pubkey.copy_from_slice(&pubkey_bytes);
    Bond {
        peer_id: peer_id_from_pubkey(&pubkey),
        pubkey,
        name: "replay-test".to_owned(),
        created_at: OffsetDateTime::UNIX_EPOCH,
        status: BondStatus::Bonded,
    }
}

/// Build an `AuditLog` rooted at `tempdir/last.log`.
fn open_audit_log(tempdir: &TempDir) -> (AuditLog, PathBuf) {
    let path = tempdir.path().join("last.log");
    let log = AuditLog::open(&path).expect("open audit log");
    (log, path)
}

/// Construct an orchestrator carrying one bond + audit log.
/// `tokio::time::Instant::now()` is captured so the rotation timer
/// never fires during the test.
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

/// Sign a challenge frame with a forced `nonce` so the test-side
/// `inject_response` answers the orchestrator's
/// `issue_challenge_with_nonce` call with a valid signature over
/// the exact frame the orchestrator emits.
fn signed_response_for_nonce(signing_key: &SigningKey, nonce: [u8; NONCE_BYTES]) -> Vec<u8> {
    assert_eq!(nonce.len(), NONCE_LEN);
    let frame = Frame {
        version: SYAUTH_WIRE_VERSION_V1,
        nonce,
        payload: Vec::new(),
        tag: [0u8; TAG_LEN],
    };
    let signature = sign_frame(signing_key, &frame).expect("sign frame");
    signature.to_bytes().to_vec()
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

/// Return the comma-separated nonce_hex column (2nd, 1-indexed) of
/// every line in the audit file.
fn nonce_hex_columns(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .expect("read audit")
        .lines()
        .map(|line| line.split(',').nth(1).map(str::to_owned).unwrap_or_default())
        .collect()
}

#[tokio::test]
async fn repeated_nonce_returns_replay() {
    let signing_key = SigningKey::from_bytes(&SIGNING_KEY_SEED);
    let bond = fixture_bond(&signing_key);
    let td = TempDir::new().expect("tempdir");
    let (audit_log, audit_path) = open_audit_log(&td);
    let fake = FakePeripheral::new();
    let orchestrator = build_orchestrator(fake.clone(), bond.clone(), audit_log).await;

    // First challenge: nonce A, valid signed response — Ok.
    let signed_a = signed_response_for_nonce(&signing_key, FIXED_NONCE_A);
    fake.inject_response(&bond.peer_id, signed_a.clone());
    let first = orchestrator
        .issue_challenge_with_nonce(&bond.peer_id, FIXED_NONCE_A, DEFAULT_AUTH_TIMEOUT)
        .await;
    match first {
        ChallengeOutcome::Ok { .. } => {}
        other => panic!("first issue: expected Ok, got {other:?}"),
    }

    // Second challenge: SAME nonce A, same valid signed response.
    // The orchestrator must short-circuit to Replay because A is
    // now in the per-peer LRU.
    fake.inject_response(&bond.peer_id, signed_a);
    let second = orchestrator
        .issue_challenge_with_nonce(&bond.peer_id, FIXED_NONCE_A, DEFAULT_AUTH_TIMEOUT)
        .await;
    match second {
        ChallengeOutcome::Replay => {}
        other => panic!("second issue: expected Replay, got {other:?}"),
    }
    assert_eq!(second.reason_str(), OUTCOME_REASON_REPLAY);

    // Audit log: two lines, outcomes [ok, replay], same nonce_hex.
    let outcomes = outcome_columns(&audit_path);
    assert_eq!(
        outcomes,
        vec![OUTCOME_REASON_OK.to_owned(), OUTCOME_REASON_REPLAY.to_owned()],
        "audit outcomes must be [ok, replay]"
    );
    let expected_nonce_hex = hex::encode(FIXED_NONCE_A);
    let nonce_columns = nonce_hex_columns(&audit_path);
    assert_eq!(
        nonce_columns,
        vec![expected_nonce_hex.clone(), expected_nonce_hex],
        "audit nonce_hex columns must match the replayed nonce"
    );
}

#[test]
fn lru_evicts_oldest_nonce_at_cap_65() {
    // Direct unit test on the pub `NonceCache` data structure.
    // Inserts NONCE_LRU_CAP + 1 = 65 distinct nonces and asserts:
    //   - the first nonce is no longer in `contains`,
    //   - the 65th nonce is in `contains`,
    //   - the second nonce is still in `contains` (sanity, the cap
    //     window holds entries 1..=64).
    let mut cache = NonceCache::new();
    let mut nonces: Vec<[u8; NONCE_BYTES]> = Vec::with_capacity(NONCE_LRU_CAP + 1);
    for i in 0..=NONCE_LRU_CAP {
        let mut n = [0u8; NONCE_BYTES];
        // Spread the counter across 2 bytes so even at i == 256 the
        // nonces stay distinct. NONCE_LRU_CAP = 64 today so this is
        // over-careful, but it survives any future SPEC bump.
        let i_u16 = u16::try_from(i).unwrap_or(u16::MAX);
        n[0] = (i_u16 & 0xff) as u8;
        n[1] = ((i_u16 >> 8) & 0xff) as u8;
        n[2] = 0x5A; // disambiguator so n[0..2] == [0, 0] still differs from a zero array
        nonces.push(n);
        cache.insert(n);
    }
    assert!(!cache.contains(&nonces[0]), "first nonce must be evicted after cap+1 insert");
    assert!(cache.contains(&nonces[NONCE_LRU_CAP]), "newest nonce must be present");
    assert!(
        cache.contains(&nonces[1]),
        "second-oldest nonce must still be inside the cap window"
    );
}
