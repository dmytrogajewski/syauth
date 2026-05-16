//! `BlueZBtPeer` — real BLE central via [`bluer`].
//!
//! S-010 (this commit) drops `BlueZBtPeer` in behind the same `BtPeer` /
//! `Session` trait pair `MockBtPeer` implements. Callers swap one for the
//! other by changing only the constructor.
//!
//! What this module owns:
//!
//! 1. The rotating session UUID derivation
//!    (`HKDF(bond_key, "syauth-session-v1" || minute)[0..16]`). Per-minute
//!    rotation defeats presence tracking (SPEC D8, T-009).
//! 2. The fragment reassembly primitive used by GATT writes that straddle the
//!    negotiated MTU. Pure-function and public so tests do not need a radio.
//! 3. The `PairingState` consult — the `/bt` Phase 2 non-negotiable: the
//!    unlock path never reads from a non-`Bonded` peer. The consult is the
//!    first executable statement of `BtPeer::connect`.
//! 4. The suspend/resume hook fed by an injectable channel (so tests do not
//!    depend on a live `logind` DBus daemon).
//! 5. Typed error mapping from `bluer::Error` into [`TransportError`].
//!
//! What this module deliberately does NOT own in S-010: a working
//! challenge/response over a real radio. The `Bonded` arm of `connect` returns
//! [`TransportError::Backend`] with a documented reason; S-019
//! ("Full e2e on real radios") wires the actual roundtrip end-to-end against
//! an emulator/CI rack.
//!
//! See `specs/journeys/JOURNEY-S-010-bluez-transport.md` for the design
//! rationale, the HKDF salt choice (`None`), and the test seam diagram.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use bluer::{AdapterEvent, gatt::remote::Characteristic};
use futures::{StreamExt, stream::BoxStream};
use hkdf::Hkdf;
use sha2::Sha256;
use syauth_core::Frame;
use tokio::{
    sync::{Mutex, mpsc},
    time::timeout as tokio_timeout,
};
use uuid::Uuid;

use crate::{BtPeer, Session, error::TransportError};

// ---------------------------------------------------------------------------
// Named constants — every magic number a test would otherwise hand-type.
// ---------------------------------------------------------------------------

/// Default BlueZ adapter id. Matches the SPEC §4.1 default; overridable in
/// `/etc/syauth.conf` (S-010 carries the name as a constructor argument so
/// the parser can be wired in S-011 without touching this module).
pub const DEFAULT_ADAPTER_NAME: &str = "hci0";

/// Wall-clock period between session-UUID rotations. SPEC D8 / DoD #3 mandate
/// "same UUID for ~1 minute then rotates"; the function exposes the minute
/// integer so tests are deterministic without injecting a clock.
pub const SESSION_UUID_ROTATION_INTERVAL: Duration = Duration::from_secs(60);

/// HKDF info string for the session UUID derivation. Concatenated with the
/// big-endian `minute` bytes to form the full HKDF info input. Versioned so a
/// future protocol revision can rotate without recomputing existing bonds.
pub const HKDF_INFO_SESSION_V1: &[u8] = b"syauth-session-v1";

/// Length of the derived session UUID in bytes. 16 = standard 128-bit UUID
/// width matching `bluer::Uuid` and SPEC §4.1's "rotating session UUID".
pub const SESSION_UUID_BYTES: usize = 16;

/// Width of the bond-key the HKDF expand step is keyed on. 32 bytes mirrors
/// the `syauth-core` bond-key width.
pub const BOND_KEY_BYTES: usize = 32;

/// Maximum BLE characteristic MTU we plan for post-negotiation on modern
/// stacks. 247 is the typical "DLE-enabled" ceiling; payloads larger than this
/// are split across multiple GATT writes and reassembled by [`reassemble`].
pub const MAX_BLE_MTU: usize = 247;

/// Length of the per-segment fragment header (one byte, high bit = "more
/// fragments follow").
pub const FRAGMENT_HEADER_LEN: usize = 1;

/// Bit mask within the fragment header that signals "more fragments follow".
/// A segment whose header has this bit cleared is the final segment of a
/// reassembled frame.
pub const FRAGMENT_MORE_BIT: u8 = 0x80;

/// Maximum payload bytes per segment after peeling the fragment header.
pub const FRAGMENT_PAYLOAD_MAX: usize = MAX_BLE_MTU - FRAGMENT_HEADER_LEN;

/// Number of seconds in one wall-clock minute. Named so the "minute floor"
/// formula at the call site reads as a domain concept, not a magic divisor.
pub const SECONDS_PER_MINUTE: i64 = 60;

/// Fixed 128-bit UUID of the syauth GATT service. Mirrors the Kotlin
/// constant `SYAUTH_GATT_SERVICE_UUID` in
/// `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/GattServer.kt`.
/// Phone advertises this UUID; the desktop scans by it.
pub const SYAUTH_GATT_SERVICE_UUID: Uuid = Uuid::from_u128(0x5a4e8e3c_1c4c_4a17_9c81_d518a55a0001);

/// Characteristic the desktop WRITES challenge frames to.
pub const SYAUTH_CHALLENGE_CHAR_UUID: Uuid = Uuid::from_u128(0x5a4e8e3c_1c4c_4a17_9c81_d518a55a0002);

/// Characteristic the phone NOTIFIES the desktop on when a response is
/// ready. Desktop subscribes and awaits the next emission per call.
pub const SYAUTH_RESPONSE_CHAR_UUID: Uuid = Uuid::from_u128(0x5a4e8e3c_1c4c_4a17_9c81_d518a55a0003);

/// How long [`BlueZBtPeer::connect`] spends scanning for a peer
/// advertising [`SYAUTH_GATT_SERVICE_UUID`] before giving up with
/// [`TransportError::Unreachable`]. Picked as half the caller-supplied
/// timeout when possible; this is the upper cap when the caller passes
/// a very large timeout.
pub const DEFAULT_SCAN_WINDOW: Duration = Duration::from_secs(8);

/// Minimum cap for the scan window. Adapters typically need >= 1 second
/// to flush the kernel scan results into the dbus stream we read from.
pub const MIN_SCAN_WINDOW: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------------------
// PairingState — the `/bt` Phase 2 non-negotiable made executable.
// ---------------------------------------------------------------------------

/// Pairing state consulted before any unlock-path read.
///
/// `/bt` Phase 2 mandates an explicit enum here rather than an `is_paired`
/// boolean. The S-010 implementation exhaustively matches every variant in
/// `BtPeer::connect` so a future maintainer adding a new variant cannot let
/// it fall through silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingState {
    /// The peer is fully bonded. Unlock-path operations may proceed.
    Bonded {
        /// Stable peer identifier (the BLAKE3-derived 16-byte hex string
        /// produced by `syauth_core::peer_id_from_pubkey`).
        peer_id: String,
    },

    /// The peer is not paired — either never bonded, or its bond has been
    /// revoked. Unlock-path operations short-circuit to
    /// [`TransportError::NotPaired`] before touching the radio.
    NotPaired,
}

// ---------------------------------------------------------------------------
// Rotating session UUID — pure function so tests are deterministic.
// ---------------------------------------------------------------------------

/// Derive the 16-byte session UUID for the wall-clock minute `minute`.
///
/// `minute` is the floor of the unix-epoch seconds by [`SECONDS_PER_MINUTE`];
/// callers compute it from `SystemTime` and pass it in so this function is
/// pure (deterministic for a given input, no `Instant::now()` inside).
///
/// The formula is, verbatim from the DoD:
///
/// ```text
/// HKDF(bond_key, "syauth-session-v1" || minute_be_bytes)[0..16]
/// ```
///
/// HKDF salt is `None`: the bond key is already 32 high-entropy bytes
/// (BLAKE3-keyed bond secret), so a separate salt buys no additional
/// security. See JOURNEY-S-010 §1 for the rationale.
///
/// The `info` parameter is the concatenation of
/// [`HKDF_INFO_SESSION_V1`] and the big-endian byte representation of
/// `minute`. Big-endian network order pins the byte layout across host
/// architectures so a derived UUID is portable.
#[must_use]
pub fn session_uuid_for(bond_key: &[u8; BOND_KEY_BYTES], minute: i64) -> [u8; SESSION_UUID_BYTES] {
    let hk = Hkdf::<Sha256>::new(None, bond_key);
    let mut info = Vec::with_capacity(HKDF_INFO_SESSION_V1.len() + core::mem::size_of::<i64>());
    info.extend_from_slice(HKDF_INFO_SESSION_V1);
    info.extend_from_slice(&minute.to_be_bytes());
    let mut out = [0u8; SESSION_UUID_BYTES];
    // `expand` only errors when the requested output exceeds 255 * HashLen
    // (= 255 * 32 = 8160 bytes for SHA-256). 16 bytes is far below that, so
    // the call is infallible by construction. We still match the result to
    // avoid `unwrap()` per the AGENTS.md non-negotiables.
    match hk.expand(&info, &mut out) {
        Ok(()) => out,
        // Unreachable in practice; preserve the all-zero buffer rather than
        // panicking. A test exercises the determinism (`session_uuid_for_is_*`)
        // and would catch a regression that ever produced all-zero output.
        Err(_) => [0u8; SESSION_UUID_BYTES],
    }
}

// ---------------------------------------------------------------------------
// Fragment reassembly — pure function so tests don't need a radio.
// ---------------------------------------------------------------------------

/// Reassemble a sequence of fragment segments into one whole frame.
///
/// Each segment is one BLE GATT write. The first byte of every segment is the
/// fragment header: if [`FRAGMENT_MORE_BIT`] is set, more segments follow;
/// otherwise this is the final segment of a frame.
///
/// Returns the concatenated payload bytes (with all headers stripped) on
/// success, or [`TransportError::IncompleteReassembly`] if the input is
/// malformed — empty slice, sub-header-length segment, or final segment with
/// the more-fragments bit still set.
///
/// This function is public so tests at the upper layer (and the smoke test in
/// `tests/bluer_smoke.rs`) can drive it without a real GATT connection. The
/// DoD test that asserts a 2-segment frame is reassembled correctly lives in
/// the [`tests`] module below.
pub fn reassemble(segments: &[Vec<u8>]) -> Result<Vec<u8>, TransportError> {
    if segments.is_empty() {
        return Err(TransportError::IncompleteReassembly);
    }
    let last_idx = segments.len() - 1;
    let mut out = Vec::with_capacity(segments.len() * FRAGMENT_PAYLOAD_MAX);
    for (idx, seg) in segments.iter().enumerate() {
        if seg.len() < FRAGMENT_HEADER_LEN {
            return Err(TransportError::IncompleteReassembly);
        }
        let header = seg[0];
        let more = (header & FRAGMENT_MORE_BIT) != 0;
        let is_last = idx == last_idx;
        if is_last && more {
            // Last segment must clear the more-fragments bit.
            return Err(TransportError::IncompleteReassembly);
        }
        if !is_last && !more {
            // Non-last segments must set the more-fragments bit.
            return Err(TransportError::IncompleteReassembly);
        }
        out.extend_from_slice(&seg[FRAGMENT_HEADER_LEN..]);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Error mapping helper — keeps `bluer::Error` from leaking into the public
// `TransportError` surface.
// ---------------------------------------------------------------------------

/// Map a `bluer::Error` produced while opening the adapter named `adapter_id`
/// into a [`TransportError`].
///
/// A `NotFound` kind becomes [`TransportError::AdapterMissing`] so the
/// operator sees a fix-specific error (name a real adapter). Every other kind
/// becomes [`TransportError::Backend`] carrying the rendered `Display` of the
/// upstream error so the upstream type never escapes this crate.
pub(crate) fn map_adapter_open_error(adapter_id: &str, err: bluer::Error) -> TransportError {
    match err.kind {
        bluer::ErrorKind::NotFound => TransportError::AdapterMissing {
            name: adapter_id.to_owned(),
        },
        _ => TransportError::Backend { reason: err.to_string() },
    }
}

// ---------------------------------------------------------------------------
// BlueZBtPeer — the `BtPeer` implementation.
// ---------------------------------------------------------------------------

/// Real BLE central backed by [`bluer`].
///
/// Drop-in replacement for `MockBtPeer` behind the S-007 [`BtPeer`] trait.
/// Holds the configured adapter id, the 32-byte bond key (for session-UUID
/// derivation), the explicit [`PairingState`] consulted by `connect`, and an
/// internal restart counter the suspend/resume loop increments on every
/// observed true→false `PrepareForSleep` transition.
///
/// Constructed via [`BlueZBtPeer::new`]. The constructor opens the adapter
/// eagerly so a missing adapter is surfaced at construction time, not at the
/// first unlock attempt — except when `pairing_state` is `NotPaired`, in
/// which case no adapter is opened at all (preserving the `/bt` Phase 2 rule
/// that the unlock path never touches the radio for non-`Bonded` peers).
pub struct BlueZBtPeer {
    adapter_id: String,
    /// Held so future S-011 / S-019 work can derive the rotating session
    /// UUID from the same instance without re-fetching from the keyring.
    /// Boxed slice (not a `[u8; 32]`) so the type stays `Send + Sync` without
    /// extra ceremony when shared via `Arc`.
    bond_key: [u8; BOND_KEY_BYTES],
    pairing_state: PairingState,
    /// Atomic restart counter incremented by [`Self::restart`]. The
    /// suspend/resume integration test asserts on this counter.
    restart_count: AtomicU64,
}

impl BlueZBtPeer {
    /// Construct a `BlueZBtPeer` bound to `adapter_id` (e.g. `"hci0"`) and
    /// the given 32-byte `bond_key` / [`PairingState`].
    ///
    /// When `pairing_state` is `Bonded`, the adapter is opened eagerly:
    /// returns [`TransportError::AdapterMissing`] if the named adapter is
    /// unknown to BlueZ, or [`TransportError::Backend`] for any other
    /// upstream error.
    ///
    /// When `pairing_state` is `NotPaired`, the constructor does NOT open the
    /// adapter — preserving the `/bt` Phase 2 rule that the unlock-path
    /// never touches the radio for a non-bonded peer. The eager adapter open
    /// would be visible side effect (DBus chatter) that this constructor
    /// must not produce.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::AdapterMissing`] or
    /// [`TransportError::Backend`] mapped via [`map_adapter_open_error`].
    pub async fn new(adapter_id: &str, bond_key: &[u8; BOND_KEY_BYTES], pairing_state: PairingState) -> Result<Self, TransportError> {
        // /bt Phase 2: the unlock-path consult comes BEFORE any bluer call.
        // For a NotPaired peer we never open the adapter — there is nothing
        // to do until the user runs `syauth pair`.
        if matches!(pairing_state, PairingState::Bonded { .. }) {
            // Probe the adapter at construction time so a missing adapter is
            // surfaced immediately rather than at first unlock. The probe
            // discards the handle; `connect` reopens as needed.
            Self::probe_adapter(adapter_id).await?;
        }
        Ok(Self {
            adapter_id: adapter_id.to_owned(),
            bond_key: *bond_key,
            pairing_state,
            restart_count: AtomicU64::new(0),
        })
    }

    /// Open and discard a handle for `adapter_id`. Returns the typed error on
    /// failure so the caller can map it onto a PAM return code.
    async fn probe_adapter(adapter_id: &str) -> Result<(), TransportError> {
        let session = bluer::Session::new().await.map_err(|err| map_adapter_open_error(adapter_id, err))?;
        // `adapter` is synchronous in bluer 0.17 and returns a typed error if
        // the named adapter does not exist on this host.
        let _adapter = session.adapter(adapter_id).map_err(|err| map_adapter_open_error(adapter_id, err))?;
        Ok(())
    }

    /// Compute the rotating session UUID for the given wall-clock minute.
    ///
    /// Public so callers (CLI, tests) can drive it deterministically without
    /// a real clock. See the free [`session_uuid_for`] function for the
    /// formula; this method just forwards.
    #[must_use]
    pub fn session_uuid_for(bond_key: &[u8; BOND_KEY_BYTES], minute: i64) -> [u8; SESSION_UUID_BYTES] {
        session_uuid_for(bond_key, minute)
    }

    /// Read the current restart counter. Used by the suspend/resume
    /// integration test to assert that a `PrepareForSleep` true→false
    /// transition produced exactly one restart.
    #[must_use]
    pub fn restart_count(&self) -> u64 {
        self.restart_count.load(Ordering::SeqCst)
    }

    /// Atomically record one restart. Called by
    /// [`Self::run_suspend_resume_loop`] on every true→false transition of
    /// the injected `PrepareForSleep` stream.
    fn restart(&self) {
        // SeqCst is overkill for a monotonic counter, but matches the test's
        // assertion-style read and removes a memory-ordering bikeshed.
        self.restart_count.fetch_add(1, Ordering::SeqCst);
    }

    /// Borrow the configured adapter id. Exposed so the suspend/resume loop
    /// or a future production caller can re-open the adapter on restart.
    #[must_use]
    pub fn adapter_id(&self) -> &str {
        &self.adapter_id
    }

    /// Borrow the configured pairing state. Used by tests and by future
    /// `syauth status` rendering (S-012).
    #[must_use]
    pub fn pairing_state(&self) -> &PairingState {
        &self.pairing_state
    }

    /// Compute the rotating session UUID for `minute` using this peer's
    /// bond key. Convenience wrapper around the free [`session_uuid_for`]
    /// for callers that already hold a `BlueZBtPeer`.
    #[must_use]
    pub fn current_session_uuid(&self, minute: i64) -> [u8; SESSION_UUID_BYTES] {
        session_uuid_for(&self.bond_key, minute)
    }

    /// Consume `prepare_for_sleep` events. On every true→false transition
    /// (suspend ended, system resumed), records one restart on `peer` and
    /// continues. Exits cleanly when the channel is closed by the caller.
    ///
    /// The DBus subscription itself is the caller's responsibility: in
    /// production code, a thin wrapper subscribes to
    /// `org.freedesktop.login1.Manager.PrepareForSleep` and forwards each
    /// `bool` into this loop's receiver. The seam exists so tests can drive
    /// the loop with a `tokio::sync::mpsc::channel(...)` without a live DBus
    /// daemon. See JOURNEY-S-010 §Phase 4 for the rationale.
    pub async fn run_suspend_resume_loop(peer: Arc<Self>, mut events: mpsc::Receiver<bool>) {
        let mut last_was_true = false;
        while let Some(event) = events.recv().await {
            if last_was_true && !event {
                peer.restart();
            }
            last_was_true = event;
        }
    }
}

#[async_trait]
impl BtPeer for BlueZBtPeer {
    async fn connect(&self, peer_timeout: Duration) -> Result<Box<dyn Session>, TransportError> {
        // /bt Phase 2 non-negotiable: the unlock-path NEVER reads from a
        // non-Bonded peer. This is the literal first statement of `connect`
        // and there is no code path that bypasses it.
        match &self.pairing_state {
            PairingState::NotPaired => return Err(TransportError::NotPaired),
            PairingState::Bonded { .. } => (),
        }
        self.connect_inner(peer_timeout).await
    }
}

impl BlueZBtPeer {
    /// Real GATT-client connect path. Opens the configured adapter, scans
    /// briefly for a peer advertising [`SYAUTH_GATT_SERVICE_UUID`],
    /// connects, discovers the syauth service + challenge/response
    /// characteristics, subscribes to response notifications, and wraps
    /// the result in a [`BlueZSession`].
    ///
    /// `peer_timeout` is the caller's total budget for the connect step;
    /// we split it into a scan window (capped by [`DEFAULT_SCAN_WINDOW`])
    /// and a remainder spent on the bluer `Device::connect` /
    /// `services()` / `characteristics()` discovery. Any failure maps to
    /// a typed [`TransportError`] so the PAM module picks the right
    /// return code without string-matching.
    async fn connect_inner(&self, peer_timeout: Duration) -> Result<Box<dyn Session>, TransportError> {
        let scan_window = scan_window_for(peer_timeout);

        let session = bluer::Session::new()
            .await
            .map_err(|err| map_adapter_open_error(&self.adapter_id, err))?;
        let adapter = session
            .adapter(&self.adapter_id)
            .map_err(|err| map_adapter_open_error(&self.adapter_id, err))?;
        adapter.set_powered(true).await.map_err(|err| TransportError::Backend {
            reason: format!("adapter set_powered: {err}"),
        })?;

        let mut filter = adapter.discovery_filter().await;
        filter.uuids = [SYAUTH_GATT_SERVICE_UUID].into_iter().collect();
        adapter.set_discovery_filter(filter).await.map_err(|err| TransportError::Backend {
            reason: format!("set_discovery_filter: {err}"),
        })?;

        let mut events = adapter.discover_devices().await.map_err(|err| TransportError::Backend {
            reason: format!("discover_devices: {err}"),
        })?;

        let device = match tokio_timeout(scan_window, find_syauth_device(&adapter, &mut events)).await {
            Ok(Ok(d)) => d,
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(TransportError::Unreachable),
        };

        // Connect + discover. bluer's `connect()` is idempotent: a
        // previously-bonded device returns Ok(()) immediately.
        device.connect().await.map_err(|err| TransportError::Backend {
            reason: format!("device.connect: {err}"),
        })?;

        let services = device.services().await.map_err(|err| TransportError::Backend {
            reason: format!("device.services: {err}"),
        })?;

        let mut challenge_char: Option<Characteristic> = None;
        let mut response_char: Option<Characteristic> = None;
        for svc in services {
            let uuid = svc.uuid().await.map_err(|err| TransportError::Backend {
                reason: format!("service.uuid: {err}"),
            })?;
            if uuid != SYAUTH_GATT_SERVICE_UUID {
                continue;
            }
            let chars = svc.characteristics().await.map_err(|err| TransportError::Backend {
                reason: format!("service.characteristics: {err}"),
            })?;
            for ch in chars {
                let cu = ch.uuid().await.map_err(|err| TransportError::Backend {
                    reason: format!("characteristic.uuid: {err}"),
                })?;
                if cu == SYAUTH_CHALLENGE_CHAR_UUID {
                    challenge_char = Some(ch);
                } else if cu == SYAUTH_RESPONSE_CHAR_UUID {
                    response_char = Some(ch);
                }
            }
            break;
        }
        let challenge_char = challenge_char.ok_or_else(|| TransportError::Backend {
            reason: "syauth challenge characteristic not exposed by peer".to_owned(),
        })?;
        let response_char = response_char.ok_or_else(|| TransportError::Backend {
            reason: "syauth response characteristic not exposed by peer".to_owned(),
        })?;

        let notify_stream = response_char.notify().await.map_err(|err| TransportError::Backend {
            reason: format!("response.notify subscribe: {err}"),
        })?;

        Ok(Box::new(BlueZSession {
            challenge_char,
            // We're forced to box the stream because its concrete type is
            // not nameable across bluer minor versions.
            notify: Mutex::new(notify_stream.boxed()),
        }))
    }
}

/// Wait on the discovery stream for a `DeviceAdded` whose advertised
/// service UUIDs include [`SYAUTH_GATT_SERVICE_UUID`]. Returns the first
/// match. The caller wraps the future in a `tokio::time::timeout` so a
/// hung adapter falls through cleanly.
async fn find_syauth_device(
    adapter: &bluer::Adapter,
    events: &mut (impl futures::Stream<Item = AdapterEvent> + Unpin),
) -> Result<bluer::Device, TransportError> {
    // First sweep the devices already known to BlueZ from a prior scan;
    // the discovery filter we just installed will replay them as
    // DeviceAdded events but only after a property change. Walking the
    // current set up front cuts the time-to-first-frame from ~5s to a few
    // hundred ms on a warm cache.
    let known = adapter.device_addresses().await.map_err(|err| TransportError::Backend {
        reason: format!("device_addresses: {err}"),
    })?;
    for addr in known {
        if let Ok(d) = adapter.device(addr)
            && device_advertises_syauth(&d).await
        {
            return Ok(d);
        }
    }
    while let Some(ev) = events.next().await {
        if let AdapterEvent::DeviceAdded(addr) = ev
            && let Ok(d) = adapter.device(addr)
            && device_advertises_syauth(&d).await
        {
            return Ok(d);
        }
    }
    Err(TransportError::Unreachable)
}

/// True iff `device` advertises [`SYAUTH_GATT_SERVICE_UUID`] in its
/// service-UUID set as reported by bluer. A `false` from this function
/// is not an error — it's the steady-state "this is just some other BLE
/// device the adapter saw" path.
async fn device_advertises_syauth(device: &bluer::Device) -> bool {
    match device.uuids().await {
        Ok(Some(uuids)) => uuids.contains(&SYAUTH_GATT_SERVICE_UUID),
        _ => false,
    }
}

/// Compute the scan window from the caller-supplied total connect
/// timeout. Capped at [`DEFAULT_SCAN_WINDOW`] (so a 5-minute caller
/// budget doesn't burn 2.5 min on scanning) and floored at
/// [`MIN_SCAN_WINDOW`] (so a sub-second caller budget still gives the
/// adapter time to drain its kernel scan results).
fn scan_window_for(total: Duration) -> Duration {
    let half = total / 2;
    half.clamp(MIN_SCAN_WINDOW, DEFAULT_SCAN_WINDOW)
}

/// GATT-client wrapper around the two syauth characteristics. One
/// session corresponds to one PAM `authenticate` call. The notify
/// stream lives behind a `Mutex` so the `Send + Sync` bound on the
/// `Session` trait holds.
struct BlueZSession {
    challenge_char: Characteristic,
    notify: Mutex<BoxStream<'static, Vec<u8>>>,
}

#[async_trait]
impl Session for BlueZSession {
    async fn send_frame(&mut self, frame: &Frame) -> Result<(), TransportError> {
        let mut bytes = Vec::with_capacity(syauth_core::MAX_FRAME_LEN);
        frame.encode(&mut bytes).map_err(TransportError::BadFrame)?;
        // For v0.1 demo we rely on negotiated MTU >= frame.len(). Phones
        // running Android 5+ negotiate up to 517 bytes, well above the
        // ~57-byte challenge frame; if a future change pushes a larger
        // frame, switch to the existing fragment/reassemble pair.
        self.challenge_char.write(&bytes).await.map_err(|err| TransportError::Backend {
            reason: format!("challenge.write: {err}"),
        })?;
        Ok(())
    }

    async fn recv_frame(&mut self, deadline: Duration) -> Result<Frame, TransportError> {
        let mut guard = self.notify.lock().await;
        let bytes = match tokio_timeout(deadline, guard.next()).await {
            Ok(Some(b)) => b,
            Ok(None) => return Err(TransportError::Closed),
            Err(_) => return Err(TransportError::Timeout),
        };
        Frame::decode(&bytes).map_err(TransportError::BadFrame)
    }
}

// ---------------------------------------------------------------------------
// Tests — DoD-mapped, all radio-free.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // Journey: specs/journeys/JOURNEY-S-010-bluez-transport.md

    use super::*;
    use crate::{BtPeer, TransportError};

    /// Deterministic fixture bond key for the HKDF unit tests.
    const TEST_BOND_KEY: [u8; BOND_KEY_BYTES] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
        0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F, 0x20,
    ];

    /// Arbitrary "now / 60" anchor for the rotation test.
    const TEST_MINUTE_ANCHOR: i64 = 30_120_960;

    // -- TC-01 ---------------------------------------------------------------
    #[test]
    fn session_uuid_for_is_deterministic_per_minute() {
        let first = session_uuid_for(&TEST_BOND_KEY, TEST_MINUTE_ANCHOR);
        let second = session_uuid_for(&TEST_BOND_KEY, TEST_MINUTE_ANCHOR);
        assert_eq!(first, second, "same (bond_key, minute) must produce the same UUID");
        assert_ne!(first, [0u8; SESSION_UUID_BYTES], "UUID must not be all zeros");
    }

    // -- TC-02 ---------------------------------------------------------------
    #[test]
    fn session_uuid_for_rotates_each_minute() {
        let u0 = session_uuid_for(&TEST_BOND_KEY, TEST_MINUTE_ANCHOR);
        let u1 = session_uuid_for(&TEST_BOND_KEY, TEST_MINUTE_ANCHOR + 1);
        let u2 = session_uuid_for(&TEST_BOND_KEY, TEST_MINUTE_ANCHOR + 2);
        assert_ne!(u0, u1, "successive minutes must rotate");
        assert_ne!(u1, u2, "successive minutes must rotate");
        assert_ne!(u0, u2, "non-adjacent minutes must rotate");
    }

    // -- TC-03 ---------------------------------------------------------------
    #[test]
    fn reassemble_joins_two_segments_into_whole_frame() {
        let seg0: Vec<u8> = vec![FRAGMENT_MORE_BIT, 0xAA, 0xBB];
        let seg1: Vec<u8> = vec![0x00, 0xCC, 0xDD];
        let whole = reassemble(&[seg0, seg1]).expect("two-segment reassembly succeeds");
        assert_eq!(whole, vec![0xAA, 0xBB, 0xCC, 0xDD]);
    }

    // -- TC-04 ---------------------------------------------------------------
    #[test]
    fn reassemble_passes_single_segment_through() {
        let seg0: Vec<u8> = vec![0x00, 0x11, 0x22, 0x33];
        let whole = reassemble(&[seg0]).expect("single-segment reassembly succeeds");
        assert_eq!(whole, vec![0x11, 0x22, 0x33]);
    }

    // -- TC-05 ---------------------------------------------------------------
    #[test]
    fn reassemble_rejects_truncated_multi_segment() {
        // Final segment still has `more-fragments` bit set → no follow-up.
        let seg0: Vec<u8> = vec![FRAGMENT_MORE_BIT, 0x42];
        let err = reassemble(&[seg0]).expect_err("truncated multi-segment must error");
        assert_eq!(err, TransportError::IncompleteReassembly);
    }

    // -- TC-06 ---------------------------------------------------------------
    #[test]
    fn reassemble_rejects_empty_segment_slice() {
        let err = reassemble(&[]).expect_err("empty segment slice must error");
        assert_eq!(err, TransportError::IncompleteReassembly);
    }

    // Negative: a non-last segment without the more-fragments bit set.
    #[test]
    fn reassemble_rejects_non_last_segment_without_more_bit() {
        let seg0: Vec<u8> = vec![0x00, 0xAA];
        let seg1: Vec<u8> = vec![0x00, 0xBB];
        let err = reassemble(&[seg0, seg1]).expect_err("non-last must have more-bit set");
        assert_eq!(err, TransportError::IncompleteReassembly);
    }

    // Negative: a segment shorter than the fragment header.
    #[test]
    fn reassemble_rejects_sub_header_segment() {
        let seg0: Vec<u8> = vec![];
        let err = reassemble(&[seg0]).expect_err("sub-header segment must error");
        assert_eq!(err, TransportError::IncompleteReassembly);
    }

    // -- TC-07 ---------------------------------------------------------------
    #[tokio::test]
    async fn connect_rejects_when_not_paired() {
        // NotPaired never opens an adapter — this constructor is safe to call
        // on a host without BlueZ.
        let peer = BlueZBtPeer::new(DEFAULT_ADAPTER_NAME, &TEST_BOND_KEY, PairingState::NotPaired)
            .await
            .expect("NotPaired construction must succeed without a radio");
        let outcome = peer.connect(Duration::from_millis(10)).await;
        let err = match outcome {
            Err(e) => e,
            Ok(_session) => panic!("NotPaired connect must reject before touching the radio"),
        };
        assert_eq!(err, TransportError::NotPaired);
    }

    // -- TC-08 ---------------------------------------------------------------
    #[tokio::test]
    async fn suspend_resume_restarts_transport() {
        let peer = BlueZBtPeer::new(DEFAULT_ADAPTER_NAME, &TEST_BOND_KEY, PairingState::NotPaired)
            .await
            .expect("NotPaired construction must succeed without a radio");
        let peer = Arc::new(peer);
        let (tx, rx) = mpsc::channel::<bool>(4);
        let loop_handle = tokio::spawn(BlueZBtPeer::run_suspend_resume_loop(Arc::clone(&peer), rx));
        tx.send(true).await.expect("send true (prepare for sleep)");
        tx.send(false).await.expect("send false (resumed)");
        drop(tx); // closes the receiver in the loop
        loop_handle.await.expect("loop completes cleanly on channel close");
        assert_eq!(peer.restart_count(), 1, "exactly one true→false transition observed");
    }

    // Suspend/resume should not increment on a spurious lone `false`.
    #[tokio::test]
    async fn suspend_resume_ignores_lone_false() {
        let peer = BlueZBtPeer::new(DEFAULT_ADAPTER_NAME, &TEST_BOND_KEY, PairingState::NotPaired)
            .await
            .expect("NotPaired construction must succeed without a radio");
        let peer = Arc::new(peer);
        let (tx, rx) = mpsc::channel::<bool>(2);
        let loop_handle = tokio::spawn(BlueZBtPeer::run_suspend_resume_loop(Arc::clone(&peer), rx));
        tx.send(false).await.expect("send lone false");
        drop(tx);
        loop_handle.await.expect("loop completes cleanly");
        assert_eq!(peer.restart_count(), 0, "lone false must not trigger a restart");
    }

    // -- TC-09 ---------------------------------------------------------------
    #[test]
    fn adapter_missing_maps_to_typed_error() {
        let synth = bluer::Error {
            kind: bluer::ErrorKind::NotFound,
            message: "adapter hci99 not found".to_owned(),
        };
        let mapped = map_adapter_open_error("hci99", synth);
        assert_eq!(mapped, TransportError::AdapterMissing { name: "hci99".to_owned() });
    }

    // Error mapping: any non-NotFound bluer error becomes `Backend`. Picks
    // `NotReady` as a representative non-NotFound kind — semantically distinct
    // from "adapter missing" (a NotReady adapter exists but is in a bad state).
    #[test]
    fn other_bluer_errors_map_to_backend() {
        let synth = bluer::Error {
            kind: bluer::ErrorKind::NotReady,
            message: "adapter not ready".to_owned(),
        };
        let mapped = map_adapter_open_error("hci0", synth);
        match mapped {
            TransportError::Backend { reason } => {
                assert!(
                    reason.contains("not ready") || reason.to_lowercase().contains("ready"),
                    "rendered reason must mention upstream message, got: {reason}"
                );
            }
            other => panic!("expected Backend, got {other:?}"),
        }
    }

    // Sanity: the constructor stores the pairing state we passed in. Guards
    // against a future refactor that silently drops the field.
    #[tokio::test]
    async fn new_records_pairing_state() {
        let peer = BlueZBtPeer::new(DEFAULT_ADAPTER_NAME, &TEST_BOND_KEY, PairingState::NotPaired)
            .await
            .expect("NotPaired construction must succeed without a radio");
        assert_eq!(peer.pairing_state(), &PairingState::NotPaired);
        assert_eq!(peer.adapter_id(), DEFAULT_ADAPTER_NAME);
    }

    // Sanity: the method forwarder matches the free function.
    #[test]
    fn session_uuid_for_method_matches_free_function() {
        let via_method = BlueZBtPeer::session_uuid_for(&TEST_BOND_KEY, TEST_MINUTE_ANCHOR);
        let via_free = session_uuid_for(&TEST_BOND_KEY, TEST_MINUTE_ANCHOR);
        assert_eq!(via_method, via_free);
    }

    // Sanity: the instance accessor reuses the constructor's bond key.
    #[tokio::test]
    async fn current_session_uuid_uses_stored_bond_key() {
        let peer = BlueZBtPeer::new(DEFAULT_ADAPTER_NAME, &TEST_BOND_KEY, PairingState::NotPaired)
            .await
            .expect("NotPaired construction must succeed without a radio");
        let from_peer = peer.current_session_uuid(TEST_MINUTE_ANCHOR);
        let from_free = session_uuid_for(&TEST_BOND_KEY, TEST_MINUTE_ANCHOR);
        assert_eq!(from_peer, from_free);
    }
}
