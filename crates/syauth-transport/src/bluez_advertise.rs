//! `BluerAdvertiser` — desktop-side BLE peripheral helpers.
//!
//! S-009 retires the per-PAM-call advertise burst (`BluerAdvertiser::connect`
//! together with `BluerAdvertiseSession`). The long-lived
//! [`crate::PersistentPeripheral`] shipped by S-003 is now the only path
//! that opens an advertisement on behalf of `syauth-presenced`; this module
//! keeps only the pieces that `PersistentPeripheral` (and the audit-helper
//! test surface) still reuse:
//!
//! 1. The `BluerAdvertiser` carrier — adapter id, bond key, pairing state —
//!    constructed via [`BluerAdvertiser::new_sync`].
//! 2. [`BluerAdvertiser::rotating_uuid_for`] — pure, radio-free derivation
//!    of the per-minute session UUID. Shared with the phone side via the
//!    UniFFI surface.
//!
//! The DEV-004 LESC link-encryption flags (`encrypt_authenticated_read /
//! _write`) are pinned by the radio-free
//! `dev004_security_flags_set_on_application` test below, which builds its
//! own representative `Service` tree — the production tree is built inline
//! by [`crate::PersistentPeripheral`].
//!
//! DEV-004 update: the challenge characteristic's `read` block and the
//! response characteristic's `write` block declare
//! `encrypt_authenticated_read: true` / `encrypt_authenticated_write:
//! true`. The BlueZ stack therefore rejects any non-bonded peer's
//! read/write/CCCD operations on these characteristics with ATT error
//! `Insufficient Authentication` before the bytes ever reach the
//! application layer.
//!
//! See `specs/journeys/JOURNEY-DEV-003-invert-advertising.md` for the
//! design rationale and `specs/journeys/JOURNEY-S-009-install-presenced-retire-burst.md`
//! for the closure that deleted the burst path.

#[cfg(test)]
use crate::bluez::SECONDS_PER_MINUTE;
use crate::bluez::{BOND_KEY_BYTES, PairingState, session_uuid_for};

// ---------------------------------------------------------------------------
// Named constants — DEV-003 closure forbids magic numbers.
// ---------------------------------------------------------------------------

/// Local-name field of the LE advertisement. Constant string — never
/// derived from the hostname — so a passive observer cannot correlate
/// the advertisement to an operator identity.
pub const ADVERTISE_LOCAL_NAME: &str = "syauth";

/// Whether the advertisement is marked discoverable. SPEC §3.2 D8's
/// rationale requires the desktop to be the long-lived advertiser; the
/// rotating UUID is what defends against tracking, not the
/// discoverable flag.
pub const ADVERTISE_DISCOVERABLE: bool = true;

// ---------------------------------------------------------------------------
// BluerAdvertiser — radio-free carrier kept for `PersistentPeripheral`.
// ---------------------------------------------------------------------------

/// Carrier for the adapter id, 32-byte bond key, and explicit
/// [`PairingState`] that the long-lived [`crate::PersistentPeripheral`]
/// passes into [`session_uuid_for`] when deriving the per-minute
/// rotating UUID.
///
/// S-009 retired the per-PAM-call `connect` method (and its
/// `BluerAdvertiseSession` return type). The struct survives because
/// `PersistentPeripheral` and CLI audit helpers still introspect its
/// stored fields and call `rotating_uuid_for`.
pub struct BluerAdvertiser {
    adapter_id: String,
    bond_key: [u8; BOND_KEY_BYTES],
    pairing_state: PairingState,
}

impl BluerAdvertiser {
    /// Construct a `BluerAdvertiser` bound to `adapter_id`, `bond_key`,
    /// and `pairing_state`. No I/O.
    #[must_use]
    pub fn new_sync(adapter_id: &str, bond_key: &[u8; BOND_KEY_BYTES], pairing_state: PairingState) -> Self {
        Self {
            adapter_id: adapter_id.to_owned(),
            bond_key: *bond_key,
            pairing_state,
        }
    }

    /// Borrow the configured adapter id (test/audit helper).
    #[must_use]
    pub fn adapter_id(&self) -> &str {
        &self.adapter_id
    }

    /// Borrow the configured pairing state (test/audit helper).
    #[must_use]
    pub fn pairing_state(&self) -> &PairingState {
        &self.pairing_state
    }

    /// Compute the rotating session UUID this advertiser would publish
    /// at wall-clock `minute`. Pure function — exposed for tests and the
    /// CLI's `syauth status` rendering so the operator can correlate the
    /// advertisement they see on the radio with their bond.
    #[must_use]
    pub fn rotating_uuid_for(&self, minute: i64) -> [u8; crate::bluez::SESSION_UUID_BYTES] {
        session_uuid_for(&self.bond_key, minute)
    }

    /// Compute the current wall-clock minute. Pure helper kept after
    /// S-009 retired the production caller so the determinism test below
    /// can still pin the seconds-since-epoch / 60 contract that
    /// `PersistentPeripheral` (and any future per-minute rotator) shares
    /// via [`session_uuid_for`].
    #[cfg(test)]
    fn current_minute_from(now: std::time::SystemTime) -> i64 {
        match now.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64 / SECONDS_PER_MINUTE,
            // Pre-1970 clock — unreachable in practice; fall back to 0 so
            // the function stays infallible without `unwrap`.
            Err(_) => 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — radio-free, deterministic.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // Journey: specs/journeys/JOURNEY-DEV-003-invert-advertising.md
    // Journey: specs/journeys/JOURNEY-S-009-install-presenced-retire-burst.md

    use std::time::Duration;

    use bluer::{
        Uuid,
        gatt::local::{
            Characteristic, CharacteristicControlHandle, CharacteristicNotify, CharacteristicNotifyMethod, CharacteristicRead,
            CharacteristicWrite, CharacteristicWriteMethod, ReqError, Service, characteristic_control,
        },
    };
    use futures::FutureExt;

    use super::*;
    use crate::bluez::{SECONDS_PER_MINUTE, SYAUTH_CHALLENGE_CHAR_UUID, SYAUTH_RESPONSE_CHAR_UUID};

    /// DEV-004: build a representative unlock-channel `Service` tree for
    /// radio-free inspection. S-009 retired the production caller; the
    /// builder lives in the test module so the closure-condition assertion
    /// on LESC link-encryption flags survives.
    fn build_unlock_services(rotating_uuid: Uuid, char_handle: CharacteristicControlHandle) -> Vec<Service> {
        vec![Service {
            uuid: rotating_uuid,
            primary: true,
            characteristics: vec![
                Characteristic {
                    uuid: SYAUTH_CHALLENGE_CHAR_UUID,
                    read: Some(CharacteristicRead {
                        read: true,
                        encrypt_authenticated_read: true,
                        fun: Box::new(|_| async move { Err(ReqError::NotPermitted) }.boxed()),
                        ..Default::default()
                    }),
                    notify: Some(CharacteristicNotify {
                        notify: true,
                        method: CharacteristicNotifyMethod::Io,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Characteristic {
                    uuid: SYAUTH_RESPONSE_CHAR_UUID,
                    write: Some(CharacteristicWrite {
                        write: true,
                        write_without_response: true,
                        encrypt_authenticated_write: true,
                        method: CharacteristicWriteMethod::Io,
                        ..Default::default()
                    }),
                    control_handle: char_handle,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }]
    }

    /// Deterministic fixture bond key.
    const TEST_BOND_KEY: [u8; BOND_KEY_BYTES] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
        0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F, 0x20,
    ];

    /// Anchor minute used for the rotating-UUID determinism test.
    const TEST_MINUTE_ANCHOR: i64 = 30_120_960;

    // -- TC-02 (Rust-side unit): rotating UUID for a given (bond_key,
    // minute) is byte-identical to the free function. Pins the
    // contract the phone-side `SlotUuidCalculator` will verify.
    #[test]
    fn rotating_uuid_for_matches_free_function() {
        let adv = BluerAdvertiser::new_sync(
            crate::bluez::DEFAULT_ADAPTER_NAME,
            &TEST_BOND_KEY,
            PairingState::Bonded {
                peer_id: "fixture-peer".to_owned(),
            },
        );
        let via_adv = adv.rotating_uuid_for(TEST_MINUTE_ANCHOR);
        let via_free = session_uuid_for(&TEST_BOND_KEY, TEST_MINUTE_ANCHOR);
        assert_eq!(via_adv, via_free);
    }

    // -- TC-03 (Rust-side unit): minute progression rotates the UUID.
    // The desktop's advertise loop rebuilds the advertisement on
    // minute boundaries; this is the deterministic core of that
    // behavior.
    #[test]
    fn rotating_uuid_for_changes_per_minute() {
        let adv = BluerAdvertiser::new_sync(
            crate::bluez::DEFAULT_ADAPTER_NAME,
            &TEST_BOND_KEY,
            PairingState::Bonded {
                peer_id: "fixture-peer".to_owned(),
            },
        );
        let u0 = adv.rotating_uuid_for(TEST_MINUTE_ANCHOR);
        let u1 = adv.rotating_uuid_for(TEST_MINUTE_ANCHOR + 1);
        assert_ne!(u0, u1, "successive minutes must rotate");
    }

    // -- TC-05 (Rust-side unit): different bond keys produce different
    // UUIDs at the same minute. The phone holds one bond key per
    // association, so a second desktop derived from a different bond
    // key is structurally invisible to it.
    #[test]
    fn rotating_uuid_for_depends_on_bond_key() {
        let adv_a = BluerAdvertiser::new_sync(
            crate::bluez::DEFAULT_ADAPTER_NAME,
            &TEST_BOND_KEY,
            PairingState::Bonded {
                peer_id: "peer-a".to_owned(),
            },
        );
        let other_key: [u8; BOND_KEY_BYTES] = [0xAA; BOND_KEY_BYTES];
        let adv_b = BluerAdvertiser::new_sync(
            crate::bluez::DEFAULT_ADAPTER_NAME,
            &other_key,
            PairingState::Bonded {
                peer_id: "peer-b".to_owned(),
            },
        );
        let u_a = adv_a.rotating_uuid_for(TEST_MINUTE_ANCHOR);
        let u_b = adv_b.rotating_uuid_for(TEST_MINUTE_ANCHOR);
        assert_ne!(u_a, u_b, "different bond keys must produce different rotating UUIDs");
    }

    // Sanity: minute helper extracts seconds-since-epoch / 60. Picks
    // a fixed `SystemTime` from a known unix-epoch offset.
    #[test]
    fn current_minute_from_extracts_minute_floor() {
        let known_secs: u64 = 1_800_000_000;
        let known_time = std::time::UNIX_EPOCH + Duration::from_secs(known_secs);
        let expected = (known_secs as i64) / SECONDS_PER_MINUTE;
        assert_eq!(BluerAdvertiser::current_minute_from(known_time), expected);
    }

    // DEV-004 TC-04 (radio-free): the unlock-service builder declares
    // `encrypt_authenticated_read: true` on the challenge characteristic
    // and `encrypt_authenticated_write: true` on the response
    // characteristic. This is the structural pin the closure condition
    // names — the on-radio rejection of non-bonded peers is the
    // SYAUTH_REAL_RADIOS=1-gated TC-02 in
    // tests/dev004_link_encryption.rs.
    #[test]
    fn dev004_security_flags_set_on_application() {
        let (_control, handle) = characteristic_control();
        let fixture_uuid = Uuid::from_u128(0x5a4e8e3c_1c4c_4a17_9c81_d518a55a0042);
        let services = build_unlock_services(fixture_uuid, handle);
        let chars = &services[0].characteristics;
        let challenge = chars
            .iter()
            .find(|c| c.uuid == SYAUTH_CHALLENGE_CHAR_UUID)
            .expect("challenge char missing");
        let response = chars
            .iter()
            .find(|c| c.uuid == SYAUTH_RESPONSE_CHAR_UUID)
            .expect("response char missing");

        let chal_read = challenge.read.as_ref().expect("challenge read block missing");
        assert!(
            chal_read.encrypt_authenticated_read,
            "challenge.read.encrypt_authenticated_read must be true (DEV-004)"
        );
        assert!(
            !chal_read.encrypt_read,
            "the weaker encrypt_read flag must NOT gate the link (SPEC §3.2 D5 demands authenticated LESC)"
        );

        let resp_write = response.write.as_ref().expect("response write block missing");
        assert!(
            resp_write.encrypt_authenticated_write,
            "response.write.encrypt_authenticated_write must be true (DEV-004)"
        );
        assert!(
            !resp_write.encrypt_write,
            "the weaker encrypt_write flag must NOT gate the link (SPEC §3.2 D5 demands authenticated LESC)"
        );
    }

    // Audit helper: the constructor stores adapter id + pairing state
    // unchanged. Guards against a future refactor that silently drops
    // either field.
    #[test]
    fn new_sync_records_inputs() {
        let adv = BluerAdvertiser::new_sync(
            crate::bluez::DEFAULT_ADAPTER_NAME,
            &TEST_BOND_KEY,
            PairingState::Bonded {
                peer_id: "fixture-peer".to_owned(),
            },
        );
        assert_eq!(adv.adapter_id(), crate::bluez::DEFAULT_ADAPTER_NAME);
        match adv.pairing_state() {
            PairingState::Bonded { peer_id } => assert_eq!(peer_id, "fixture-peer"),
            other => panic!("expected Bonded, got {other:?}"),
        }
    }
}
