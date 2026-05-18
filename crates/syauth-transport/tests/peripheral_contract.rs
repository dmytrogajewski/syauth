//! S-003 peripheral-contract integration tests.
//!
//! Journey: specs/journeys/JOURNEY-S-003-peripheral-library-api.md
//!
//! Both tests run against the [`FakePeripheral`] test double so CI
//! stays radio-free. The production [`PersistentPeripheral`] is
//! exercised against a real bluer adapter under `SYAUTH_REAL_RADIOS=1`
//! by later roadmap steps (S-004 rotation, S-006 challenge flow).

use std::collections::HashSet;

use syauth_transport::{BondKey, FakePeripheral, Peripheral, PeripheralError};
use uuid::Uuid;

/// Deterministic bond-key fixtures so the round-trip test does not
/// hand-type 32-byte literals at every call site.
const KEY_A: BondKey = [0xAA; 32];
const KEY_B: BondKey = [0xBB; 32];
const KEY_C: BondKey = [0xCC; 32];

/// Fixture UUIDs for the session-set replacement test. The values are
/// arbitrary but stable so a regression that scrambles the recorded
/// sequence is mechanically visible.
const UUID_A: Uuid = Uuid::from_u128(0x5a4e_8e3c_1c4c_4a17_9c81_d518_a55a_2001);
const UUID_B: Uuid = Uuid::from_u128(0x5a4e_8e3c_1c4c_4a17_9c81_d518_a55a_2002);
const UUID_C: Uuid = Uuid::from_u128(0x5a4e_8e3c_1c4c_4a17_9c81_d518_a55a_2003);

/// Closure-condition test verbatim from ROADMAP.md S-003:
/// add three peers, remove the middle one, assert the surviving two
/// in insertion order.
#[tokio::test]
async fn add_remove_peer_roundtrip() {
    let fake = FakePeripheral::new();
    fake.add_peer("a", &KEY_A).await.expect("add a");
    fake.add_peer("b", &KEY_B).await.expect("add b");
    fake.add_peer("c", &KEY_C).await.expect("add c");
    fake.remove_peer("b").await.expect("remove b");
    let peers = fake.peers();
    assert_eq!(peers, vec!["a".to_owned(), "c".to_owned()]);
}

/// Closure-condition test verbatim from ROADMAP.md S-003: three
/// consecutive `set_session_uuids` calls must each be recorded in
/// insertion order, with no merging.
#[tokio::test]
async fn set_session_uuids_replaces_advertisement() {
    let fake = FakePeripheral::new();
    let only_a: HashSet<Uuid> = [UUID_A].into_iter().collect();
    let only_b: HashSet<Uuid> = [UUID_B].into_iter().collect();
    let a_and_c: HashSet<Uuid> = [UUID_A, UUID_C].into_iter().collect();
    fake.set_session_uuids(only_a.clone()).await.expect("set a");
    fake.set_session_uuids(only_b.clone()).await.expect("set b");
    fake.set_session_uuids(a_and_c.clone()).await.expect("set a+c");
    let calls = fake.session_uuid_calls();
    assert_eq!(calls, vec![only_a, only_b, a_and_c]);
}

/// TC-04 negative path: notify on an unknown peer is a typed error.
#[tokio::test]
async fn notify_challenge_unknown_peer_is_typed_error() {
    let fake = FakePeripheral::new();
    let err = fake.notify_challenge("ghost", &[0xAB]).await.expect_err("unknown peer");
    match err {
        PeripheralError::UnknownPeer { peer_id } => assert_eq!(peer_id, "ghost"),
        other => panic!("expected UnknownPeer, got {other:?}"),
    }
}

/// TC-05 negative path: removing a peer that was never added is a
/// typed error.
#[tokio::test]
async fn remove_unknown_peer_is_typed_error() {
    let fake = FakePeripheral::new();
    fake.add_peer("a", &KEY_A).await.expect("add a");
    let err = fake.remove_peer("b").await.expect_err("unknown peer");
    match err {
        PeripheralError::UnknownPeer { peer_id } => assert_eq!(peer_id, "b"),
        other => panic!("expected UnknownPeer, got {other:?}"),
    }
}
