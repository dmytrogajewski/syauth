//! `syauth-transport` — the seam between the `syauth-core` protocol and the
//! Bluetooth (or, in v0.2, LAN) layer that actually moves bytes.
//!
//! S-007 ships:
//!
//! - the async [`BtPeer`] / [`Session`] trait pair,
//! - a typed [`TransportError`] enum,
//! - an in-process [`MockBtPeer`] driven by a [`MockScenario`] enum so the
//!   PAM module (S-008/S-009) can be tested end-to-end before a single byte
//!   of `bluer` code lands in S-010.
//!
//! S-010 (this commit) adds:
//!
//! - [`BlueZBtPeer`], the `bluer`-backed real BLE central, behind the same
//!   trait,
//! - the rotating session UUID derivation
//!   ([`session_uuid_for`](bluez::session_uuid_for)),
//! - the fragment reassembly primitive ([`reassemble`](bluez::reassemble)),
//! - the [`PairingState`](bluez::PairingState) consult,
//! - the [`run_suspend_resume_loop`](bluez::BlueZBtPeer::run_suspend_resume_loop)
//!   hook fed by an injectable `tokio::sync::mpsc::Receiver<bool>`.
//!
//! The only types that cross the trait boundary are [`syauth_core::Frame`],
//! [`std::time::Duration`], and [`TransportError`]. That is the contract that
//! lets `BlueZBtPeer` slot in as a drop-in replacement for [`MockBtPeer`]
//! without touching any caller.
//!
//! See `specs/journeys/JOURNEY-S-007-transport-trait.md` for the trait
//! design rationale and `specs/journeys/JOURNEY-S-010-bluez-transport.md`
//! for the BlueZ design rationale.

#![deny(missing_docs)]

pub mod bluez;
pub mod error;
pub mod mock;

use std::time::Duration;

use async_trait::async_trait;
pub use bluez::{
    BOND_KEY_BYTES, BlueZBtPeer, DEFAULT_ADAPTER_NAME, FRAGMENT_HEADER_LEN, FRAGMENT_MORE_BIT, FRAGMENT_PAYLOAD_MAX, HKDF_INFO_SESSION_V1,
    MAX_BLE_MTU, PairingState, SECONDS_PER_MINUTE, SESSION_UUID_BYTES, SESSION_UUID_ROTATION_INTERVAL, reassemble, session_uuid_for,
};
pub use error::TransportError;
pub use mock::{
    GOLDEN_PAYLOAD_XOR_MASK, GOLDEN_RECV_TIMEOUT, GOLDEN_ROUNDTRIP_BUDGET, MOCK_CHAN_CAP, MOCK_SLOW_DELAY, MockBtPeer, MockScenario,
    REORDERED_BUFFER_DEPTH, REPLAY_DEFAULT_DUPLICATES, SHORT_CALLER_TIMEOUT, SLOW_DEFAULT_DELAY, TIMEOUT_BUDGET_MULT,
    WRONG_VERSION_DEFAULT,
};
use syauth_core::Frame;

/// A Bluetooth peer the PAM module can talk to.
///
/// The trait is intentionally tiny: `connect` is the only verb. Everything
/// past the connection — frame send, frame recv, timeouts — lives on
/// [`Session`]. That split mirrors the BlueZ surface where adapter discovery
/// and GATT-server connection are separate calls.
///
/// Implementations are `Send + Sync` so the PAM module can hold one in a
/// `OnceLock<Box<dyn BtPeer>>`, populated at module load and reused across
/// PAM invocations.
#[async_trait]
pub trait BtPeer: Send + Sync {
    /// Open a session to the peer, returning after at most `timeout` with
    /// [`TransportError::Timeout`].
    ///
    /// May also return [`TransportError::Unreachable`] if the peer is known
    /// to be off the air (mock scenario `Offline`, or in S-010 the adapter
    /// reports no advertisement during the discovery window).
    async fn connect(&self, timeout: Duration) -> Result<Box<dyn Session>, TransportError>;
}

/// An open session with a single peer.
///
/// One `Session` represents one BLE GATT connection in the production
/// implementation. The PAM module sends one challenge frame and reads one
/// response per `pam_sm_authenticate` call; we do not hold sessions across
/// calls.
#[async_trait]
pub trait Session: Send + Sync {
    /// Send a wire-format frame to the peer.
    ///
    /// Returns [`TransportError::Closed`] if the peer hung up, or
    /// [`TransportError::BadFrame`] / [`TransportError::WrongVersion`] if the
    /// transport detected a wire-level corruption before the bytes left the
    /// host.
    async fn send_frame(&mut self, frame: &Frame) -> Result<(), TransportError>;

    /// Read the next inbound frame, returning [`TransportError::Timeout`]
    /// after `timeout`.
    ///
    /// May also return [`TransportError::BadFrame`] if the bytes arrived but
    /// `syauth_core::Frame::decode` rejected them, or
    /// [`TransportError::Closed`] if the peer hung up.
    async fn recv_frame(&mut self, timeout: Duration) -> Result<Frame, TransportError>;
}
