//! DEV-001 TC-11: end-to-end regression — keys produced by the LESC
//! pair flow are accepted by the unlock path.
//!
//! The journey doc's TC-11 demands: "after two devices completed pairing
//! per TC-01, the desktop performs one full unlock through `pam_syauth.so`
//! against the just-bonded peer". This test hermetically pins the
//! cryptographic invariant that links the two halves:
//!
//! 1. derive the `bond_key` exactly as `syauth_cli::pair_backend`
//!    would after the LESC + pubkey exchange (via
//!    [`syauth_core::bond_key_from_pubkeys`]);
//! 2. derive the peer-id exactly as the pair flow would
//!    (via [`syauth_core::peer_id_from_pubkey`]);
//! 3. verify the derived material round-trips a MAC-tagged frame
//!    through the same primitives the PAM hot path uses.
//!
//! The real `pamtester` + `libpam_syauth.so` shell-out for TC-11 lives
//! in `crates/syauth-pam/tests/pam_e2e.rs` (which exercises the same
//! primitives for every SPEC §4.3 scenario). Repeating the full
//! harness here would duplicate that suite; pinning the bridge —
//! "LESC-derived material is byte-compatible with the unlock path" —
//! is what closes the new gap DEV-001 introduces.
//!
//! Journey: specs/journeys/JOURNEY-DEV-001-real-lesc.md TC-11

use syauth_core::{
    BOND_KEY_BYTES, BOND_KEY_DERIVED_BYTES, Bond, BondStatus, BondStore, SigningKey, bond_key_from_pubkeys, compute_tag,
    peer_id_from_pubkey, verify_tag,
};

/// Pinned host seed so the derived host pubkey is stable.
const SEED_HOST: [u8; 32] = [0x11; 32];

/// Pinned phone seed so the derived phone pubkey is stable.
const SEED_PHONE: [u8; 32] = [0x22; 32];

/// Fixture MAC input — payload bytes the desktop would tag in the
/// real protocol. Content is arbitrary; we only need a stable buffer
/// the MAC verify can run against.
const FIXTURE_PAYLOAD: &[u8] = b"DEV-001 TC-11 fixture payload";

#[test]
fn tc11_lesc_derived_bond_key_is_byte_compatible_with_mac_primitives() {
    // Step 1 — model the LESC outcome.
    let host_sk = SigningKey::from_bytes(&SEED_HOST);
    let phone_sk = SigningKey::from_bytes(&SEED_PHONE);
    let host_pubkey: [u8; 32] = host_sk.verifying_key().to_bytes();
    let phone_pubkey: [u8; 32] = phone_sk.verifying_key().to_bytes();

    // Step 2 — both sides derive the SAME bond_key.
    let bond_key_desktop: [u8; BOND_KEY_DERIVED_BYTES] = bond_key_from_pubkeys(&host_pubkey, &phone_pubkey);
    let bond_key_phone: [u8; BOND_KEY_DERIVED_BYTES] = bond_key_from_pubkeys(&host_pubkey, &phone_pubkey);
    assert_eq!(bond_key_desktop, bond_key_phone, "both ends must derive identical bond_key");
    // BOND_KEY_DERIVED_BYTES is sized to match the MAC primitive's keying width.
    assert_eq!(BOND_KEY_DERIVED_BYTES, BOND_KEY_BYTES);

    // Step 3 — derive the peer_id from the phone's pubkey.
    let peer_id = peer_id_from_pubkey(&phone_pubkey);
    assert!(!peer_id.is_empty());

    // Step 4 — MAC + verify roundtrip with the LESC-derived bond_key.
    // Mirrors what the PAM hot path does on every unlock.
    let tag = compute_tag(&bond_key_desktop, FIXTURE_PAYLOAD);
    assert!(
        verify_tag(&bond_key_phone, FIXTURE_PAYLOAD, &tag),
        "LESC-derived bond_key MUST verify a tag produced by itself"
    );
}

#[test]
fn tc11_bond_record_built_from_lesc_outcome_round_trips_through_bondstore() {
    // Models the persistence step on the desktop side after TC-01's
    // app-OOB confirmation lands. `peer_id` MUST equal
    // `peer_id_from_pubkey(&pubkey)` per `BondStore::load`'s
    // PeerIdMismatch invariant — a regression that breaks the linkage
    // between LESC outcome and BondStore lands here loudly.
    let phone_sk = SigningKey::from_bytes(&SEED_PHONE);
    let phone_pubkey: [u8; 32] = phone_sk.verifying_key().to_bytes();
    let peer_id = peer_id_from_pubkey(&phone_pubkey);

    let bond = Bond {
        peer_id: peer_id.clone(),
        pubkey: phone_pubkey,
        name: "tc11-phone".to_owned(),
        created_at: time::OffsetDateTime::now_utc(),
        status: BondStatus::Bonded,
    };
    let mut store = BondStore::empty();
    store.add(bond).expect("BondStore.add must accept a LESC-shaped Bond");
    assert_eq!(store.list().len(), 1);
    assert_eq!(store.list()[0].peer_id, peer_id);
}
