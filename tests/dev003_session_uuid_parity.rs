//! DEV-003 cross-crate byte-identity check.
//!
//! The desktop's `syauth_transport::session_uuid_for(bond_key, minute)`
//! and the phone's `syauth_mobile::session_uuid_for_bond(bond_key,
//! minute)` MUST produce byte-identical output for every input —
//! otherwise the phone's `BluetoothLeScanner.ScanFilter` UUID set
//! would never match the desktop's advertised service UUID and
//! DEV-003's closure would be cosmetic only.
//!
//! Journey: specs/journeys/JOURNEY-DEV-003-invert-advertising.md (TC-02).

use syauth_mobile::session_uuid_for_bond;
use syauth_transport::session_uuid_for;

/// Deterministic fixture bond key (32 bytes of incrementing nibbles).
/// Same byte pattern the per-crate unit tests use, so a manual diff is
/// fast.
const FIXTURE_BOND_KEY: [u8; 32] = [
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
    0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F, 0x20,
];

/// Anchor minute that exercises a non-zero `i64::to_be_bytes` payload.
const TEST_MINUTE_ANCHOR: i64 = 30_120_960;

#[test]
fn session_uuid_for_bond_matches_transport_byte_for_byte() {
    for offset in 0..=3i64 {
        let minute = TEST_MINUTE_ANCHOR + offset;
        let from_desktop = session_uuid_for(&FIXTURE_BOND_KEY, minute);
        let from_phone = session_uuid_for_bond(FIXTURE_BOND_KEY.to_vec(), minute).expect("mobile uuid");
        assert_eq!(from_phone, from_desktop.to_vec(), "minute={minute}");
    }
}

#[test]
fn session_uuid_differs_per_minute() {
    let u0 = session_uuid_for_bond(FIXTURE_BOND_KEY.to_vec(), TEST_MINUTE_ANCHOR).expect("u0");
    let u1 = session_uuid_for_bond(FIXTURE_BOND_KEY.to_vec(), TEST_MINUTE_ANCHOR + 1).expect("u1");
    assert_ne!(u0, u1);
}

#[test]
fn session_uuid_differs_per_bond_key() {
    let other_bond: [u8; 32] = [0xAA; 32];
    let u_a = session_uuid_for_bond(FIXTURE_BOND_KEY.to_vec(), TEST_MINUTE_ANCHOR).expect("u_a");
    let u_b = session_uuid_for_bond(other_bond.to_vec(), TEST_MINUTE_ANCHOR).expect("u_b");
    assert_ne!(u_a, u_b);
}
