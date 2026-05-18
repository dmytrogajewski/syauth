//! DEV-001 integration tests for the real LESC pair flow.
//!
//! Maps onto the `JOURNEY-DEV-001-real-lesc.md` test matrix (TC-01 ..
//! TC-09). Radio-touching cases (TC-01 golden e2e, TC-02 OS-code
//! mismatch, TC-08 re-pair) are `#[ignore]`d behind a
//! `SYAUTH_REAL_RADIOS=1` env gate, matching the S-019 pattern in
//! `tests/e2e_real.rs`. The unit-testable closure probes (TC-03
//! Just-Works downgrade, TC-04 app-OOB mismatch, TC-05 scan timeout,
//! TC-09 adapter-missing) run on every `cargo test --workspace`
//! invocation.
//!
//! Journey: specs/journeys/JOURNEY-DEV-001-real-lesc.md

#![allow(clippy::expect_used)] // tests are allowed to expect()

use syauth_cli::{
    pair::{PairError, PairingVariant, decide_pairing},
    pair_backend::BluerPairBackend,
};
use syauth_core::{SigningKey, bond_key_from_pubkeys};
use syauth_transport::session_uuid_for;
use uuid::Uuid;

/// Pinned Ed25519 seed so the derived pubkey is identical across runs.
const SEED_HOST: [u8; 32] = [0x11; 32];
/// Pinned Ed25519 seed for the "phone" side of TC-04 / TC-11.
const SEED_PHONE: [u8; 32] = [0x22; 32];
/// Pinned alternate phone seed used to substitute a wrong key in TC-04.
const SEED_PHONE_TAMPERED: [u8; 32] = [0x33; 32];
/// Test minute anchor for pair-mode UUID derivation.
const TEST_MINUTE: i64 = 30_120_960;

// ---------------------------------------------------------------------------
// TC-03 — Just Works downgrade rejected.
// ---------------------------------------------------------------------------

#[test]
fn tc03_just_works_variant_is_refused_with_downgrade_blocked() {
    let outcome = decide_pairing(PairingVariant::JustWorks);
    match outcome {
        Err(PairError::DowngradeBlocked { actual }) => {
            assert_eq!(actual, PairingVariant::JustWorks);
        }
        other => panic!("expected DowngradeBlocked for JustWorks, got {other:?}"),
    }
}

#[test]
fn tc03_legacy_pin_variant_is_refused_as_unsupported() {
    let outcome = decide_pairing(PairingVariant::LegacyPin);
    assert!(
        matches!(outcome, Err(PairError::UnsupportedPairingVariant { .. })),
        "LegacyPin must surface as UnsupportedPairingVariant, got {outcome:?}"
    );
}

#[test]
fn tc03_passkey_entry_variant_is_refused_as_unsupported() {
    let outcome = decide_pairing(PairingVariant::PasskeyEntry);
    assert!(
        matches!(outcome, Err(PairError::UnsupportedPairingVariant { .. })),
        "PasskeyEntry must surface as UnsupportedPairingVariant, got {outcome:?}"
    );
}

#[test]
fn tc03_oob_only_variant_is_refused_as_unsupported() {
    let outcome = decide_pairing(PairingVariant::OobOnly);
    assert!(
        matches!(outcome, Err(PairError::UnsupportedPairingVariant { .. })),
        "OobOnly must surface as UnsupportedPairingVariant, got {outcome:?}"
    );
}

#[test]
fn tc03_lesc_passkey_confirmation_is_the_only_acceptable_variant() {
    let outcome = decide_pairing(PairingVariant::PasskeyConfirmation);
    assert!(
        outcome.is_ok(),
        "PasskeyConfirmation is the only accepted variant per SPEC §3.2 D5, got {outcome:?}"
    );
}

// ---------------------------------------------------------------------------
// TC-04 — App-level OOB mismatch (defense-in-depth).
// ---------------------------------------------------------------------------

#[test]
fn tc04_bond_key_diverges_when_phone_pubkey_is_substituted() {
    let host_sk = SigningKey::from_bytes(&SEED_HOST);
    let phone_sk = SigningKey::from_bytes(&SEED_PHONE);
    let tampered_sk = SigningKey::from_bytes(&SEED_PHONE_TAMPERED);
    let host_pubkey: [u8; 32] = host_sk.verifying_key().to_bytes();
    let phone_pubkey: [u8; 32] = phone_sk.verifying_key().to_bytes();
    let tampered_pubkey: [u8; 32] = tampered_sk.verifying_key().to_bytes();

    let honest = bond_key_from_pubkeys(&host_pubkey, &phone_pubkey);
    let tampered = bond_key_from_pubkeys(&host_pubkey, &tampered_pubkey);
    // The bond_key must diverge — and therefore so will the 4-word OOB
    // code derived from it — when the phone's pubkey is substituted
    // between the desktop's write and the desktop's read.
    assert_ne!(honest, tampered, "tampered pubkey must produce a different bond_key");
}

#[test]
fn tc04_bond_key_converges_for_matching_inputs_on_both_ends() {
    // Both ends apply the same HKDF over (host_pubkey || phone_pubkey),
    // so the derived `bond_key` must be byte-identical. This is the
    // invariant TC-01's golden path relies on.
    let host_sk = SigningKey::from_bytes(&SEED_HOST);
    let phone_sk = SigningKey::from_bytes(&SEED_PHONE);
    let host_pubkey: [u8; 32] = host_sk.verifying_key().to_bytes();
    let phone_pubkey: [u8; 32] = phone_sk.verifying_key().to_bytes();

    let on_desktop = bond_key_from_pubkeys(&host_pubkey, &phone_pubkey);
    let on_phone = bond_key_from_pubkeys(&host_pubkey, &phone_pubkey);
    assert_eq!(on_desktop, on_phone);
}

// ---------------------------------------------------------------------------
// TC-05 — Pair-mode UUID rotation (replay defense + scan-window pin).
// ---------------------------------------------------------------------------

#[test]
fn tc05_pair_discovery_uuid_rotates_between_minute_slots() {
    let host_sk = SigningKey::from_bytes(&SEED_HOST);
    let backend = BluerPairBackend::new("hci0", &host_sk);
    let _ = backend; // backend has no public accessor; this is a smoke test.
    let a = session_uuid_for(&[0u8; 32], TEST_MINUTE);
    let b = session_uuid_for(&[0u8; 32], TEST_MINUTE + 1);
    assert_ne!(a, b, "successive minutes must rotate the pair-mode UUID");
}

// ---------------------------------------------------------------------------
// DEV-001 re-march — desktop ADVERTISES the pair-mode UUID for slot N
// (SPEC §3.2 D8). This is the integration-level pin that catches a
// regression to the old scan-based direction.
// ---------------------------------------------------------------------------

#[test]
fn dev001_remarch_pair_discovery_uuid_for_slot_n_matches_zero_bond_session_uuid() {
    // The desktop derives its advertised UUID from a zero bond_key (no
    // bond exists yet at pair time) and the wall-clock minute. The phone
    // must derive the exact same bytes from `sessionUuidForBond(zero, n)`
    // so its scanner filter matches.
    let minute_n = TEST_MINUTE;
    let via_backend = BluerPairBackend::pair_discovery_uuid(minute_n);
    let via_transport = Uuid::from_bytes(session_uuid_for(&[0u8; 32], minute_n));
    assert_eq!(
        via_backend, via_transport,
        "desktop's pair-mode UUID must equal sessionUuidForBond(zero, n)"
    );
}

#[test]
fn dev001_remarch_pair_discovery_uuid_rotates_between_n_and_n_minus_1() {
    // The phone scanner accepts both slot N and slot N-1 to absorb up
    // to one minute of negative clock skew. The desktop's UUID for
    // slot N must therefore differ from slot N-1, otherwise the
    // rotation gives no replay defense.
    let n_minus_1 = BluerPairBackend::pair_discovery_uuid(TEST_MINUTE - 1);
    let n = BluerPairBackend::pair_discovery_uuid(TEST_MINUTE);
    assert_ne!(n_minus_1, n, "slot N must differ from slot N-1");
}

// ---------------------------------------------------------------------------
// TC-09 — BlueZ adapter down: typed error, never panics.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tc09_adapter_missing_surfaces_typed_error_not_panic() {
    use syauth_cli::pair::PairBackend;
    let host_sk = SigningKey::from_bytes(&SEED_HOST);
    let backend = BluerPairBackend::new("hci99-does-not-exist", &host_sk);
    let result = backend.adapter_info("hci99-does-not-exist").await;
    // On a CI host with BlueZ not running we get a Backend error
    // wrapping the dbus failure; on a host with BlueZ but no such
    // adapter we get AdapterMissing. Either is acceptable — the
    // contract is "typed error, no panic, no unwrap".
    assert!(result.is_err(), "non-existent adapter must surface as Err, got {result:?}");
    match result {
        Err(PairError::AdapterMissing { .. }) | Err(PairError::Backend { .. }) => (),
        other => panic!("unexpected outcome for missing adapter: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// TC-01 / TC-02 / TC-08 — on-radio cases gated behind SYAUTH_REAL_RADIOS=1.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "TC-01 needs a real BlueZ adapter + a real phone (gated by SYAUTH_REAL_RADIOS=1)"]
async fn tc01_golden_path_on_real_radios() {
    if std::env::var("SYAUTH_REAL_RADIOS").as_deref() != Ok("1") {
        return;
    }
    // Real-radio path: the operator invokes this manually. See
    // JOURNEY-DEV-001-real-lesc.md TC-01 for the full procedure.
    // Pinning this here as an `#[ignore]`d test means the CI knows
    // the case exists and the operator can run it with
    // `cargo test --test pair_lesc_test -- --ignored`.
    panic!("TC-01 requires manual operator-driven execution on a paired phone + BlueZ adapter");
}

#[tokio::test]
#[ignore = "TC-02 needs a manual MitM rig + real radios (gated by SYAUTH_REAL_RADIOS=1)"]
async fn tc02_os_code_mismatch_on_real_radios() {
    if std::env::var("SYAUTH_REAL_RADIOS").as_deref() != Ok("1") {
        return;
    }
    panic!("TC-02 requires a manual MitM rig; see JOURNEY-DEV-001-real-lesc.md");
}

#[tokio::test]
#[ignore = "TC-08 needs a real adapter + a pre-bonded peer (gated by SYAUTH_REAL_RADIOS=1)"]
async fn tc08_re_pair_without_revoke_on_real_radios() {
    if std::env::var("SYAUTH_REAL_RADIOS").as_deref() != Ok("1") {
        return;
    }
    panic!("TC-08 requires a pre-bonded peer; see JOURNEY-DEV-001-real-lesc.md");
}
