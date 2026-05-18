//! DEV-004 integration test: LE link encryption gates the unlock GATT
//! characteristics.
//!
//! Journey: specs/journeys/JOURNEY-DEV-004-link-encryption.md
//!
//! This file houses the on-radio TCs from the DEV-004 closure
//! condition. Each test is `#[ignore]`-gated behind the
//! `SYAUTH_REAL_RADIOS=1` env var following the S-019 / DEV-001 /
//! DEV-003 pattern, because they require:
//!
//! - a live BlueZ daemon on the host running the test;
//! - a BLE adapter reachable from the test process;
//! - a second BlueZ-driven test peer (locally on a second adapter, or
//!   via an external rig) that has NOT bonded with the advertiser.
//!
//! The radio-free structural pin lives in
//! `crates/syauth-transport/src/bluez_advertise.rs::tests::dev004_security_flags_set_on_application`
//! — that test asserts `encrypt_authenticated_read: true` /
//! `encrypt_authenticated_write: true` on the unlock characteristics.
//! The on-radio TCs below verify the *consequence* of those flags:
//! the BlueZ stack rejects non-bonded peers before the bytes reach the
//! application layer.

/// Required value of the `SYAUTH_REAL_RADIOS` env var that unlocks the
/// on-radio tests. Documented in the journey doc and in
/// `docs/known-gaps.md`.
const REAL_RADIOS_GATE: &str = "1";

/// Name of the env var used to gate the on-radio tests.
const REAL_RADIOS_VAR: &str = "SYAUTH_REAL_RADIOS";

fn real_radios_enabled() -> bool {
    std::env::var(REAL_RADIOS_VAR).as_deref() == Ok(REAL_RADIOS_GATE)
}

// ---------------------------------------------------------------------------
// TC-02 — non-bonded peer's write to the challenge characteristic is
// rejected by the BlueZ stack with ATT error Insufficient Authentication
// (or Insufficient Encryption, depending on the kernel version). The
// `BluerAdvertiseSession`'s characteristic-control stream never observes
// a `Write` event for the rogue peer.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "TC-02 needs a live BlueZ adapter + an unbonded second peer (gated by SYAUTH_REAL_RADIOS=1)"]
async fn dev004_non_bonded_write_rejected() {
    if !real_radios_enabled() {
        return;
    }
    // Real-radio path: the operator drives a second BlueZ-backed peer
    // (a second adapter or an external rig) that has *not* completed
    // any pair flow with the advertiser. The procedure is documented
    // in JOURNEY-DEV-004-link-encryption.md §4 TC-02. The expected
    // outcome is:
    //
    //   1. The rogue peer's `gattlib`/`btmgmt` write to the challenge
    //      characteristic returns ATT error 0x05 (Insufficient
    //      Authentication) or 0x0F (Insufficient Encryption).
    //   2. `tracing` events on the desktop log zero
    //      `CharacteristicControlEvent::Write` events for the rogue
    //      peer.
    //   3. `Frame::decode` is never called with attacker-controlled
    //      bytes.
    //
    // The structural guarantee that drives this outcome is pinned in
    // the radio-free unit test
    // `bluez_advertise::tests::dev004_security_flags_set_on_application`,
    // which asserts `encrypt_authenticated_write: true` on the
    // response characteristic and `encrypt_authenticated_read: true`
    // on the challenge characteristic (the latter inherits to the CCCD
    // descriptor BlueZ auto-creates for the notify configuration).
    panic!("DEV-004 TC-02 requires manual operator-driven execution; see specs/journeys/JOURNEY-DEV-004-link-encryption.md §4 TC-02");
}

// ---------------------------------------------------------------------------
// TC-03 — CCCD subscription on the response characteristic is rejected
// from a non-bonded peer. The CCCD descriptor inherits its encryption
// requirement from the characteristic's own read/write security flags
// (bluer 0.17.4 contract).
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "TC-03 needs a live BlueZ adapter + an unbonded second peer (gated by SYAUTH_REAL_RADIOS=1)"]
async fn dev004_cccd_subscribe_rejected_when_unbonded() {
    if !real_radios_enabled() {
        return;
    }
    // The rogue peer attempts a CCCD write of `0x0100` (notifications
    // enabled) to the response characteristic's auto-created CCCD
    // descriptor. The BlueZ stack should reject the write with the
    // same Insufficient-Authentication ATT error as TC-02.
    panic!("DEV-004 TC-03 requires manual operator-driven execution; see specs/journeys/JOURNEY-DEV-004-link-encryption.md §4 TC-03");
}

// ---------------------------------------------------------------------------
// TC-01 — bonded peer write succeeds end-to-end on real radios.
// Encompasses the DEV-001 + DEV-003 + DEV-004 closure path: the LESC
// LTK from pairing satisfies the new encryption flags transparently.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "TC-01 needs a live BlueZ adapter + a real bonded phone (gated by SYAUTH_REAL_RADIOS=1)"]
async fn dev004_bonded_write_succeeds_e2e() {
    if !real_radios_enabled() {
        return;
    }
    panic!(
        "DEV-004 TC-01 requires manual operator-driven execution on a paired phone + BlueZ adapter; see specs/journeys/JOURNEY-DEV-004-link-encryption.md §4 TC-01"
    );
}
