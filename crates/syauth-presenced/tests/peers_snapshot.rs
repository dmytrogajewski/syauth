// Journey: specs/journeys/JOURNEY-S-017-status-extension.md
// Roadmap row: specs/unlock-proximity/ROADMAP.md Step S-017.
//
// Pin the `Orchestrator::peers_snapshot()` shape so the
// `syauth-cli` `status` subcommand can consume a stable wire
// frame.

use std::{path::PathBuf, sync::Arc, time::Duration};

use syauth_core::{Bond, BondStatus, bond::PUBKEY_LEN, peer_id_from_pubkey};
use syauth_presenced::Orchestrator;
use syauth_transport::{BOND_KEY_BYTES, FakePeripheral, Peripheral};
use time::OffsetDateTime;
use tokio::time::Instant;

const BOND_KEY_A: [u8; BOND_KEY_BYTES] = [0xA1; BOND_KEY_BYTES];
const BOND_KEY_B: [u8; BOND_KEY_BYTES] = [0xB2; BOND_KEY_BYTES];
const PUBKEY_A: [u8; PUBKEY_LEN] = [0x0A; PUBKEY_LEN];
const PUBKEY_B: [u8; PUBKEY_LEN] = [0x0B; PUBKEY_LEN];

fn bond_for(pubkey: [u8; PUBKEY_LEN], name: &str) -> Bond {
    Bond {
        peer_id: peer_id_from_pubkey(&pubkey),
        pubkey,
        name: name.to_owned(),
        created_at: OffsetDateTime::UNIX_EPOCH,
        status: BondStatus::Bonded,
    }
}

#[tokio::test]
async fn peers_snapshot_returns_one_row_per_bonded_peer() {
    let fake = FakePeripheral::new();
    let peripheral: Arc<dyn Peripheral + Send + Sync> = fake.clone();
    let bond_a = bond_for(PUBKEY_A, "phone-a");
    let bond_b = bond_for(PUBKEY_B, "phone-b");
    let start = Instant::now() + Duration::from_secs(60);
    let orchestrator = Arc::new(Orchestrator::with_peers_and_audit(
        peripheral,
        vec![(bond_a.clone(), BOND_KEY_A), (bond_b.clone(), BOND_KEY_B)],
        PathBuf::new(),
        PathBuf::new(),
        start,
        None,
    ));
    fake.add_peer(&bond_a.peer_id, &BOND_KEY_A).await.expect("add a");
    fake.add_peer(&bond_b.peer_id, &BOND_KEY_B).await.expect("add b");
    let snap = orchestrator.peers_snapshot().await;
    assert_eq!(snap.len(), 2, "two bonds should produce two rows");
    let ids: Vec<_> = snap.iter().map(|r| r.peer_id.clone()).collect();
    assert!(ids.contains(&bond_a.peer_id));
    assert!(ids.contains(&bond_b.peer_id));
    for row in &snap {
        assert_eq!(row.in_flight_challenges, 0, "no challenges issued yet");
        assert!(row.last_challenge_ms_ago.is_none(), "cold start has no last challenge");
        assert!(row.last_connect_ms_ago.is_none(), "cold start has no last connect");
        assert_ne!(row.current_session_uuid, uuid::Uuid::nil(), "uuid populated");
    }
}
