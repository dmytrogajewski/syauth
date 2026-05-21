//! `Peripheral` — long-lived BLE peripheral library API for the daemon.
//!
//! S-003 ships the trait, the `PersistentPeripheral` production impl
//! over `bluer 0.17`, and the radio-free `FakePeripheral` test double.
//! See `specs/journeys/JOURNEY-S-003-peripheral-library-api.md` for
//! the design rationale.
//!
//! The trait splits the BLE peripheral role into four named operations
//! the daemon (`syauth-presenced`) consumes across many PAM calls:
//!
//! 1. [`Peripheral::add_peer`] — register a bonded peer's challenge +
//!    response characteristics with the long-lived GATT application.
//! 2. [`Peripheral::remove_peer`] — drop a peer's characteristics
//!    (used after a revoke or a `bonds.toml` diff).
//! 3. [`Peripheral::set_session_uuids`] — replace the advertised
//!    `service_uuids` set. Called by the daemon's per-minute rotation
//!    timer (S-004).
//! 4. [`Peripheral::notify_challenge`] — push challenge bytes on the
//!    per-peer challenge characteristic.
//!
//! `PersistentPeripheral` owns one `bluer::Adapter`, one
//! `bluer::adv::AdvertisementHandle` (replaceable via
//! `set_session_uuids`), one `bluer::gatt::local::ApplicationHandle`
//! (long-lived for the daemon's lifetime), and a
//! `Mutex<HashMap<peer_id, PeerCharSet>>` keyed by stable peer id. The
//! daemon's tokio orchestrator clones an `Arc<dyn Peripheral>` into
//! every per-peer task, so the trait requires `Send + Sync`.
//!
//! `BluerAdvertiser` (the per-PAM-call burst path used by `pam_syauth`
//! today, `crates/syauth-pam/src/auth.rs:575`) is intentionally NOT
//! refactored by S-003 — it remains byte-identical until S-009 deletes
//! it. The library API is a strict superset.

use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use bluer::{
    Uuid,
    adv::Advertisement,
    agent::{Agent, AgentHandle, RequestConfirmation},
    gatt::{
        CharacteristicReader, CharacteristicWriter,
        local::{
            Application, ApplicationHandle, Characteristic, CharacteristicControlEvent, CharacteristicNotify, CharacteristicNotifyMethod,
            CharacteristicWrite, CharacteristicWriteMethod, Service, characteristic_control,
        },
    },
};
use futures::StreamExt;
use thiserror::Error;
use tokio::{
    io::AsyncReadExt,
    sync::{Mutex, mpsc},
    task::JoinHandle,
};

use crate::{
    bluez::{BOND_KEY_BYTES, SYAUTH_CHALLENGE_CHAR_UUID, SYAUTH_RESPONSE_CHAR_UUID, map_adapter_open_error, session_uuid_for},
    bluez_advertise::{ADVERTISE_DISCOVERABLE, ADVERTISE_LOCAL_NAME},
    error::TransportError,
};

/// Per-peer mpsc depth for incoming response frames. Sized to absorb
/// short bursts of malformed writes without back-pressuring the GATT
/// thread; one in-flight challenge per peer (SPEC §3 #7) makes 8 frames
/// generous headroom.
const RESPONSE_READ_BUF_BYTES: usize = 512;

/// Stable service UUID per bond, derived from the bond key at minute=0.
/// The phone's GATT client discovers characteristics by UUID after the
/// connect — service UUID identity doesn't have to rotate.
fn peer_service_uuid(bond_key: &BondKey) -> Uuid {
    let bytes = session_uuid_for(bond_key, 0);
    Uuid::from_bytes(bytes)
}

// ---------------------------------------------------------------------------
// Public type aliases — keep the trait surface readable.
// ---------------------------------------------------------------------------

/// 32-byte bond key the daemon holds for one bonded peer. Mirrors the
/// width of `syauth_core::BOND_KEY_DERIVED_BYTES` /
/// [`BOND_KEY_BYTES`]. Re-exported here so callers do not have to dig
/// into the bluez module just to declare a parameter type.
pub type BondKey = [u8; BOND_KEY_BYTES];

// ---------------------------------------------------------------------------
// PeripheralError — typed surface for the trait.
// ---------------------------------------------------------------------------

/// Errors produced by [`Peripheral`] implementations.
///
/// Distinct from [`TransportError`] because the persistent-peripheral
/// surface has different failure modes than the per-PAM-call client:
/// `UnknownPeer` is a structural diff-time error the daemon must
/// surface to the operator, while `AdapterMissing` and `Backend` align
/// with their `TransportError` cousins so log lines read consistently.
#[derive(Debug, Error)]
pub enum PeripheralError {
    /// The named BlueZ adapter does not exist on this host. The
    /// operator's fix is well-defined: edit `/etc/syauth.conf` to
    /// name a real adapter (or plug one in).
    #[error("bluetooth adapter '{name}' not found")]
    AdapterMissing {
        /// The adapter id the caller asked for (e.g. `"hci0"`).
        name: String,
    },

    /// The daemon called `remove_peer` or `notify_challenge` with a
    /// `peer_id` that was not previously added via `add_peer`. Always
    /// a structural diff bug at the orchestrator layer.
    #[error("unknown peer: peer_id={peer_id}")]
    UnknownPeer {
        /// The peer_id that was not found.
        peer_id: String,
    },

    /// The daemon called `add_peer` with a `peer_id` that was already
    /// added. The orchestrator's diffing layer must reconcile against
    /// the live set; silent overwrite would leak GATT service handles.
    #[error("peer already added: peer_id={peer_id}")]
    PeerAlreadyAdded {
        /// The peer_id that collided.
        peer_id: String,
    },

    /// Opaque upstream failure from `bluer` or `dbus`. Wraps the
    /// rendered upstream `Display` so the upstream type never escapes
    /// this crate's public API.
    #[error("peripheral backend error: {reason}")]
    Backend {
        /// Human-readable description of the upstream failure.
        reason: String,
    },

    /// `wait_for_response(peer_id, deadline)` reached its deadline
    /// without observing a write on the per-peer response
    /// characteristic. Distinct from `Backend` so the orchestrator's
    /// challenge state machine can map the timeout to the SPEC §6
    /// `TimedOut → AuthInfoUnavail(reason=response-timeout)`
    /// transition without parsing an error string.
    #[error("response timed out: peer_id={peer_id} deadline={deadline_ms}ms")]
    ResponseTimeout {
        /// The peer_id the orchestrator was waiting on.
        peer_id: String,
        /// The deadline that elapsed, in milliseconds, so the audit
        /// row carries the budget that was applied.
        deadline_ms: u64,
    },
}

impl From<TransportError> for PeripheralError {
    fn from(err: TransportError) -> Self {
        match err {
            TransportError::AdapterMissing { name } => PeripheralError::AdapterMissing { name },
            other => PeripheralError::Backend { reason: other.to_string() },
        }
    }
}

// ---------------------------------------------------------------------------
// Peripheral trait — the daemon's stable contract.
// ---------------------------------------------------------------------------

/// Long-lived BLE peripheral the daemon holds across many PAM calls.
///
/// All methods are `async`. Implementations must be `Send + Sync` so
/// the daemon's tokio orchestrator can share one instance behind
/// `Arc<dyn Peripheral>` across per-peer tasks. Object-safety is
/// load-bearing: the orchestrator stores a `Arc<dyn Peripheral>`
/// field, not a generic parameter.
///
/// See the journey doc for the four-phase CJM that motivates each
/// method.
#[async_trait]
pub trait Peripheral: Send + Sync {
    /// Register a bonded peer's challenge + response characteristics
    /// with the long-lived GATT application.
    ///
    /// Returns [`PeripheralError::PeerAlreadyAdded`] if `peer_id` was
    /// already added — silent re-add would leak handles across diff
    /// cycles in the orchestrator.
    async fn add_peer(&self, peer_id: &str, bond_key: &BondKey) -> Result<(), PeripheralError>;

    /// Drop a peer's characteristics from the GATT application.
    ///
    /// Returns [`PeripheralError::UnknownPeer`] if `peer_id` was never
    /// added — diff bugs surface loud, not silent.
    async fn remove_peer(&self, peer_id: &str) -> Result<(), PeripheralError>;

    /// Replace the advertised `service_uuids` set. The previous
    /// advertisement is torn down before the new one is registered so
    /// a passive observer never sees both UUID sets simultaneously.
    async fn set_session_uuids(&self, uuids: std::collections::HashSet<Uuid>) -> Result<(), PeripheralError>;

    /// Push challenge bytes on the per-peer challenge characteristic.
    /// Returns [`PeripheralError::UnknownPeer`] if `peer_id` was never
    /// added.
    async fn notify_challenge(&self, peer_id: &str, frame: &[u8]) -> Result<(), PeripheralError>;

    /// Await a single GATT-write on the per-peer response
    /// characteristic, returning the buffered bytes. Returns
    /// [`PeripheralError::ResponseTimeout`] if `deadline` elapses
    /// before a write arrives, or [`PeripheralError::UnknownPeer`]
    /// if `peer_id` was never added.
    ///
    /// S-006 contract: the production [`PersistentPeripheral`]
    /// subscribes once (in `add_peer`) to the response
    /// characteristic's GATT-WRITE events and buffers them in a
    /// per-peer `mpsc::Receiver<Vec<u8>>`. The fake exposes
    /// `inject_response(peer_id, bytes)` so tests queue a synthetic
    /// response without touching a radio.
    async fn wait_for_response(&self, peer_id: &str, deadline: Duration) -> Result<Vec<u8>, PeripheralError>;
}

// ---------------------------------------------------------------------------
// PersistentPeripheral — bluer 0.17 production impl.
// ---------------------------------------------------------------------------

/// Per-peer characteristic state owned by [`PersistentPeripheral`].
///
/// S-006 adds the per-peer `tokio::sync::mpsc::Receiver<Vec<u8>>`
/// buffer that backs `wait_for_response(peer_id, deadline)`. The
/// production sender side is fed by the GATT WRITE callback on the
/// response characteristic, but the SPEC keeps the bluez-side
/// subscription wiring as a GAP — see the trait doc on
/// `wait_for_response`.
///
/// GAP: bluez-side GATT WRITE → mpsc::Sender bridge — closure plan
/// is the S-006 response-characteristic registration (this S-006 row
/// ships the trait method + buffer; the BlueZ subscription that
/// pushes onto `response_tx` lives behind the same field name and
/// closes in a follow-on row).
struct PeerCharSet {
    /// Receiver side of the per-peer response buffer. `Mutex` so the
    /// trait method can take exclusive access to a single shared
    /// receiver without requiring `&mut self`.
    response_rx: Mutex<mpsc::Receiver<Vec<u8>>>,
    /// Sender side. Held alongside the receiver so the peer's
    /// channel stays alive across the call lifecycle of the GATT
    /// WRITE callback.
    _response_tx: mpsc::Sender<Vec<u8>>,
    /// Per-peer challenge notifier — populated by the bluer control
    /// loop when the phone subscribes to the challenge characteristic
    /// (CCCD write). `notify_challenge` reads this slot and writes the
    /// frame bytes. `None` until the phone subscribes.
    notifier_slot: Arc<Mutex<Option<CharacteristicWriter>>>,
    /// JoinHandle for the per-peer control loop (challenge subscribe +
    /// response write reader). Aborted on `remove_peer`.
    task_handle: Mutex<Option<JoinHandle<()>>>,
    /// Bond key for this peer. Cached so we can rebuild the GATT
    /// application registration on dead-writer detection without
    /// having to plumb the bond store all the way down. The bond
    /// key is the input to `peer_service_uuid(bond_key)`, which the
    /// rebuild path needs to construct the service tree.
    bond_key: BondKey,
}

/// Per-peer response buffer depth for the `PersistentPeripheral`'s
/// `mpsc::channel`. Sized so a malformed phone that batches a burst
/// of WRITEs in 1 s does not back-pressure the GATT thread to a
/// halt; one in-flight challenge per peer is the SPEC §3 scope item
/// #7 contract, so a depth of 8 leaves headroom for transient
/// timing skew without unbounded growth.
const RESPONSE_BUFFER_DEPTH: usize = 8;

/// Production peripheral backed by `bluer 0.17`.
///
/// Owns one `bluer::Session`, one `bluer::Adapter`, one long-lived
/// `ApplicationHandle`, and an in-memory map of `PeerCharSet`. The
/// `AdvertisementHandle` lives in a `Mutex` slot so
/// `set_session_uuids` can replace it without taking `&mut self`.
pub struct PersistentPeripheral {
    /// BlueZ session (a `bluer::Session` is the DBus client connection
    /// to `bluetoothd`). Held so the adapter and the application stay
    /// alive for the lifetime of the daemon.
    _session: bluer::Session,
    /// Adapter the peripheral operates on (e.g. `hci0`).
    adapter: bluer::Adapter,
    /// Live GATT application registration. Replaced on every
    /// `add_peer` / `remove_peer` because bluer's `Application` is a
    /// snapshot — services cannot be appended after registration.
    /// `None` until the first peer is added.
    app_handle: Mutex<Option<ApplicationHandle>>,
    /// Currently-published advertisement. Replaced by
    /// `set_session_uuids`. Optional because the daemon may construct
    /// the peripheral before any UUIDs are known (cold-start path).
    adv_slot: Mutex<Option<bluer::adv::AdvertisementHandle>>,
    /// BlueZ agent handle, held for the lifetime of the daemon.
    /// Dropping it unregisters the agent; we never drop it because
    /// the daemon is the sole pairing responder on the desktop.
    ///
    /// Registered at `new()` so any phone-initiated LESC pairing
    /// against this adapter dispatches `request_confirmation` to
    /// our handler instead of falling back to the system default
    /// (which rejects numeric comparison and forces the bond to
    /// time out with `HCI_ERR_AUTH_FAILURE`). The callback
    /// auto-accepts: the user's trust signal is the CDM-picker
    /// selection on the phone, not a desktop-side prompt.
    #[allow(dead_code)]
    agent_handle: AgentHandle,
    /// Per-peer characteristic state. Keyed by stable peer_id.
    peers: Mutex<HashMap<String, PeerCharSet>>,
}

impl PersistentPeripheral {
    /// Construct a `PersistentPeripheral` bound to `adapter_id`.
    ///
    /// Opens the BlueZ adapter, powers it on, and registers an empty
    /// long-lived GATT application. The advertisement slot is empty
    /// until the caller invokes [`Peripheral::set_session_uuids`].
    ///
    /// # Errors
    ///
    /// Returns [`PeripheralError::AdapterMissing`] when the named
    /// adapter is unknown to BlueZ, or [`PeripheralError::Backend`]
    /// for any other upstream failure.
    pub async fn new(adapter_id: &str) -> Result<Arc<Self>, PeripheralError> {
        let session = bluer::Session::new()
            .await
            .map_err(|err| PeripheralError::from(map_adapter_open_error(adapter_id, err)))?;
        let adapter = session
            .adapter(adapter_id)
            .map_err(|err| PeripheralError::from(map_adapter_open_error(adapter_id, err)))?;
        adapter.set_powered(true).await.map_err(|err| PeripheralError::Backend {
            reason: format!("adapter set_powered: {err}"),
        })?;
        // Make the adapter ready to accept phone-initiated LESC pairing
        // at any time, without requiring a separate `syauth pair` process
        // to flip these flags. The daemon is the sole BlueZ client on the
        // desktop; it owns these settings for its lifetime.
        adapter
            .set_discoverable(true)
            .await
            .map_err(|err| PeripheralError::Backend {
                reason: format!("adapter set_discoverable: {err}"),
            })?;
        adapter.set_pairable(true).await.map_err(|err| PeripheralError::Backend {
            reason: format!("adapter set_pairable: {err}"),
        })?;
        // Register a system-wide BlueZ pairing agent so phone-initiated
        // LESC numeric-comparison bonding attempts dispatch their
        // `request_confirmation` callback to us. `request_default = true`
        // makes us the system default agent for the daemon's lifetime;
        // without this BlueZ would route to whatever Just-Works fallback
        // it has and the bond would fail with HCI_ERR_AUTH_FAILURE.
        //
        // The handler auto-accepts: the trust signal is the user's
        // CDM-picker tap on the phone selecting this desktop. The desktop
        // never prompts; that's the UX contract.
        let agent = Agent {
            request_default: true,
            request_confirmation: Some(Box::new(|_req: RequestConfirmation| Box::pin(async move { Ok(()) }))),
            ..Default::default()
        };
        let agent_handle = session.register_agent(agent).await.map_err(|err| PeripheralError::Backend {
            reason: format!("register_agent: {err}"),
        })?;
        // Defer `serve_gatt_application` until the first peer is added.
        // BlueZ on Fedora 43 rejects an empty `Application::default()`
        // with "No object received"; the application is built with
        // real services in `add_peer` and re-registered on every diff.
        Ok(Arc::new(Self {
            _session: session,
            adapter,
            app_handle: Mutex::new(None),
            adv_slot: Mutex::new(None),
            agent_handle,
            peers: Mutex::new(HashMap::new()),
        }))
    }

    /// Disconnect every LE peer currently connected to our BlueZ
    /// adapter. Returns `Ok(())` when every disconnect call succeeds;
    /// surface-level failures (peer unknown, dbus error) are swallowed
    /// behind a `warn` so a single stuck device cannot block the
    /// caller. Called after every fresh `serve_gatt_application` so a
    /// phone whose CCCD subscription is bound to the previous
    /// Application registration is forced to re-handshake.
    async fn kick_connected_peers(&self) -> Result<(), PeripheralError> {
        let addrs = self.adapter.device_addresses().await.map_err(|err| PeripheralError::Backend {
            reason: format!("device_addresses: {err}"),
        })?;
        for addr in addrs {
            let device = match self.adapter.device(addr) {
                Ok(d) => d,
                Err(err) => {
                    tracing::warn!(
                        target: "syauth_transport",
                        addr = %addr,
                        error = %err,
                        "kick_connected_peers: device handle unavailable"
                    );
                    continue;
                }
            };
            let connected = device.is_connected().await.unwrap_or(false);
            if !connected {
                continue;
            }
            match device.disconnect().await {
                Ok(()) => {
                    tracing::info!(
                        target: "syauth_transport",
                        addr = %addr,
                        "kick_connected_peers: disconnected stale peer"
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        target: "syauth_transport",
                        addr = %addr,
                        error = %err,
                        "kick_connected_peers: Device::disconnect failed"
                    );
                }
            }
        }
        Ok(())
    }

    /// Helper: build a `bluer` advertisement object from a UUID set.
    /// Pure synchronous factory so the unit tests in this module can
    /// inspect the constructed structure without an adapter.
    fn build_advertisement(uuids: std::collections::HashSet<Uuid>) -> Advertisement {
        // bluer's `Advertisement::service_uuids` is a `BTreeSet`, so
        // we collect once into the destination shape.
        let service_uuids: std::collections::BTreeSet<Uuid> = uuids.into_iter().collect();
        Advertisement {
            service_uuids,
            discoverable: Some(ADVERTISE_DISCOVERABLE),
            local_name: Some(ADVERTISE_LOCAL_NAME.to_owned()),
            ..Default::default()
        }
    }
}

// `Send + Sync` audit: every field is `Send + Sync` —
// `bluer::Session`, `bluer::Adapter`, `ApplicationHandle`,
// `Mutex<Option<AdvertisementHandle>>`, `Mutex<HashMap<..>>`.
// The auto-derived bounds suffice.

impl PersistentPeripheral {
    /// Build a fresh Service+Characteristic tree for one peer and
    /// register it. Returns the per-peer state (notifier slot,
    /// response channel, control-loop task) that `add_peer` stashes
    /// into `PeerCharSet`. Called fresh on every `add_peer` because
    /// bluer's `Application` is a snapshot — control_handles are
    /// consumed by registration and cannot be reused.
    async fn build_and_register_peer(
        &self,
        bond_key: &BondKey,
    ) -> Result<
        (
            Arc<Mutex<Option<CharacteristicWriter>>>,
            mpsc::Sender<Vec<u8>>,
            mpsc::Receiver<Vec<u8>>,
            JoinHandle<()>,
        ),
        PeripheralError,
    > {
        let (mut chal_control, chal_handle) = characteristic_control();
        let (mut resp_control, resp_handle) = characteristic_control();
        let service_uuid = peer_service_uuid(bond_key);
        let app = Application {
            services: vec![Service {
                uuid: service_uuid,
                primary: true,
                characteristics: vec![
                    Characteristic {
                        uuid: SYAUTH_CHALLENGE_CHAR_UUID,
                        notify: Some(CharacteristicNotify {
                            notify: true,
                            method: CharacteristicNotifyMethod::Io,
                            ..Default::default()
                        }),
                        control_handle: chal_handle,
                        ..Default::default()
                    },
                    Characteristic {
                        uuid: SYAUTH_RESPONSE_CHAR_UUID,
                        write: Some(CharacteristicWrite {
                            write: true,
                            write_without_response: true,
                            method: CharacteristicWriteMethod::Io,
                            ..Default::default()
                        }),
                        control_handle: resp_handle,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        tracing::info!(target: "syauth_transport", uuid=%service_uuid, "registering GATT app for peer");
        let new_handle = self
            .adapter
            .serve_gatt_application(app)
            .await
            .map_err(|err| PeripheralError::Backend {
                reason: format!("serve_gatt_application: {err}"),
            })?;
        tracing::info!(target: "syauth_transport", "GATT app registration accepted by BlueZ");
        // Replace the live application registration.
        let mut slot = self.app_handle.lock().await;
        *slot = Some(new_handle);
        drop(slot);

        // Kick any LE peer that survived the previous daemon process
        // / Application registration. A phone running PersistentGattClient
        // with `autoConnect=true` keeps the LE link alive across our
        // `serve_gatt_application` swap, but its CCCD subscription is
        // bound to the dead application registration — every
        // `notify_challenge` against it lands on notifier_slot=None.
        // Forcing a Device::disconnect() drops the link cleanly; the
        // phone's autoConnect re-establishes it within seconds and
        // (per the phone-side fix) calls `gatt.refresh()` +
        // `discoverServices()` so the fresh app's CCCD subscription
        // takes effect.
        if let Err(err) = self.kick_connected_peers().await {
            tracing::warn!(
                target: "syauth_transport",
                error = %err,
                "kick_connected_peers failed; phones with stale subscriptions may need a manual BT cycle"
            );
        }

        let notifier_slot: Arc<Mutex<Option<CharacteristicWriter>>> = Arc::new(Mutex::new(None));
        let (response_tx, response_rx) = mpsc::channel::<Vec<u8>>(RESPONSE_BUFFER_DEPTH);
        let notifier_slot_for_task = notifier_slot.clone();
        let response_tx_for_task = response_tx.clone();
        let task = tokio::spawn(async move {
            let mut reader_opt: Option<CharacteristicReader> = None;
            loop {
                tokio::select! {
                    // Phone subscribes / unsubscribes to challenge notifications.
                    chal_evt = chal_control.next() => {
                        match chal_evt {
                            Some(CharacteristicControlEvent::Notify(writer)) => {
                                tracing::info!(target: "syauth_transport", "chal_control: Notify event — phone subscribed");
                                *notifier_slot_for_task.lock().await = Some(writer);
                            }
                            Some(CharacteristicControlEvent::Write(_)) => {
                                tracing::warn!(target: "syauth_transport", "chal_control: unexpected Write event");
                            }
                            None => {
                                tracing::warn!(target: "syauth_transport", "chal_control: stream ended, task exiting");
                                break;
                            }
                        }
                    }
                    // Phone writes a response frame; accept and drain.
                    resp_evt = resp_control.next() => {
                        match resp_evt {
                            Some(CharacteristicControlEvent::Write(req)) => {
                                tracing::info!(target: "syauth_transport", "resp_control: Write event — phone writing response");
                                match req.accept() {
                                    Ok(reader) => { reader_opt = Some(reader); }
                                    Err(err) => {
                                        tracing::warn!(target: "syauth_transport", error=%err, "resp_control: req.accept failed");
                                        continue;
                                    }
                                }
                            }
                            Some(CharacteristicControlEvent::Notify(_)) => {
                                tracing::warn!(target: "syauth_transport", "resp_control: unexpected Notify event");
                            }
                            None => {
                                tracing::warn!(target: "syauth_transport", "resp_control: stream ended, task exiting");
                                break;
                            }
                        }
                    }
                    // Drain bytes from the response reader, if active.
                    read_res = async {
                        match &mut reader_opt {
                            Some(reader) => {
                                let mut buf = vec![0u8; RESPONSE_READ_BUF_BYTES];
                                let n = reader.read(&mut buf).await?;
                                buf.truncate(n);
                                Ok::<Vec<u8>, std::io::Error>(buf)
                            }
                            None => std::future::pending().await,
                        }
                    } => {
                        match read_res {
                            Ok(bytes) if !bytes.is_empty() => {
                                let _ = response_tx_for_task.send(bytes).await;
                            }
                            Ok(_) | Err(_) => {
                                // Reader closed or errored; drop it, wait for next Write event.
                                reader_opt = None;
                            }
                        }
                    }
                }
            }
        });
        Ok((notifier_slot, response_tx, response_rx, task))
    }

    /// Rebuild the GATT application registration for a single peer.
    ///
    /// Why this is needed: a `Device::disconnect()` (our
    /// `kick_connected_peers`) drops the LE link but **does not** force
    /// BlueZ to emit a fresh `CharacteristicControlEvent::Notify` when
    /// the phone re-subscribes against the same `Application` object.
    /// BlueZ remembers the per-characteristic subscription state across
    /// link transitions, so a re-subscribe is silently merged into the
    /// existing one. The `CharacteristicWriter` cached in
    /// `notifier_slot` stays dead forever, and `notify_challenge`
    /// audits `transport-error` until the daemon is restarted.
    ///
    /// The only kick that reliably triggers a fresh Notify event is
    /// unregistering the application entirely and re-registering it,
    /// which discards BlueZ's subscription state. We do that here:
    /// abort the old chal_control/resp_control task, drop the old
    /// `ApplicationHandle`, kick any LE link so the phone reconnects
    /// against the fresh application, then `build_and_register_peer`
    /// with the cached bond key and swap the new state into the
    /// `PeerCharSet`.
    ///
    /// Lock ordering: we never hold a `notifier_slot` lock across this
    /// call — `notify_challenge` releases it before invoking us. We
    /// take the `peers` lock briefly to read `bond_key`, drop it,
    /// touch `app_handle`, then take `peers` again to swap the entry.
    async fn rebuild_peer_registration(&self, peer_id: &str) -> Result<(), PeripheralError> {
        let bond_key: BondKey = {
            let peers = self.peers.lock().await;
            let entry = peers.get(peer_id).ok_or_else(|| PeripheralError::UnknownPeer {
                peer_id: peer_id.to_owned(),
            })?;
            entry.bond_key
        };
        // Abort the old per-peer control task. After this the previous
        // chal_control / resp_control characteristic_control streams
        // are owned by a dead task; dropping the `ApplicationHandle`
        // below will close them on the BlueZ side.
        {
            let peers = self.peers.lock().await;
            if let Some(entry) = peers.get(peer_id)
                && let Some(handle) = entry.task_handle.lock().await.take()
            {
                handle.abort();
            }
        }
        // Drop the old `ApplicationHandle`. bluer issues
        // `UnregisterApplication` on Drop, which prompts BlueZ to
        // forget every CCCD subscription bound to that application.
        {
            let mut slot = self.app_handle.lock().await;
            *slot = None;
        }
        // Kick any LE link so the phone reconnects against the fresh
        // application. Without this the phone's GATT cache still
        // points at the old application's characteristic handles —
        // the phone will write CCCD on a handle that no longer
        // belongs to anything, and BlueZ silently drops the write.
        if let Err(err) = self.kick_connected_peers().await {
            tracing::warn!(
                target: "syauth_transport",
                error = %err,
                "rebuild_peer_registration: kick_connected_peers failed"
            );
        }
        // Build + register a fresh application for this peer.
        let (notifier_slot, response_tx, response_rx, task) =
            self.build_and_register_peer(&bond_key).await?;
        // Swap the new state into the peer entry. `bond_key` is
        // copied (it is a `[u8; N]`) so the new entry stays
        // self-contained.
        {
            let mut peers = self.peers.lock().await;
            if let Some(entry) = peers.get_mut(peer_id) {
                *entry.notifier_slot.lock().await = None;
                entry.notifier_slot = notifier_slot;
                entry.response_rx = Mutex::new(response_rx);
                entry._response_tx = response_tx;
                *entry.task_handle.lock().await = Some(task);
            }
        }
        tracing::info!(
            target: "syauth_transport",
            peer_id = %peer_id,
            "rebuild_peer_registration: fresh GATT application registered, awaiting re-subscribe"
        );
        Ok(())
    }
}

#[async_trait]
impl Peripheral for PersistentPeripheral {
    async fn add_peer(&self, peer_id: &str, bond_key: &BondKey) -> Result<(), PeripheralError> {
        let peers = self.peers.lock().await;
        if peers.contains_key(peer_id) {
            return Err(PeripheralError::PeerAlreadyAdded {
                peer_id: peer_id.to_owned(),
            });
        }
        // Drop the existing peer-handle (if any) BEFORE serve_gatt_application
        // — bluer rejects a second registration while the first is live.
        drop(peers);
        {
            let mut slot = self.app_handle.lock().await;
            *slot = None;
        }
        let (notifier_slot, response_tx, response_rx, task) = self.build_and_register_peer(bond_key).await?;
        let mut peers = self.peers.lock().await;
        peers.insert(
            peer_id.to_owned(),
            PeerCharSet {
                response_rx: Mutex::new(response_rx),
                _response_tx: response_tx,
                notifier_slot,
                task_handle: Mutex::new(Some(task)),
                bond_key: *bond_key,
            },
        );
        Ok(())
    }

    async fn remove_peer(&self, peer_id: &str) -> Result<(), PeripheralError> {
        let mut peers = self.peers.lock().await;
        let Some(entry) = peers.remove(peer_id) else {
            return Err(PeripheralError::UnknownPeer {
                peer_id: peer_id.to_owned(),
            });
        };
        if let Some(handle) = entry.task_handle.lock().await.take() {
            handle.abort();
        }
        // Drop application registration; daemon will rebuild on next add.
        let mut slot = self.app_handle.lock().await;
        *slot = None;
        Ok(())
    }

    async fn set_session_uuids(&self, uuids: std::collections::HashSet<Uuid>) -> Result<(), PeripheralError> {
        let mut slot = self.adv_slot.lock().await;
        // Tear down the previous advertisement first so a passive
        // observer never sees both UUID sets simultaneously.
        slot.take();
        let advertisement = Self::build_advertisement(uuids);
        let handle = self
            .adapter
            .advertise(advertisement)
            .await
            .map_err(|err| PeripheralError::Backend {
                reason: format!("advertise: {err}"),
            })?;
        *slot = Some(handle);
        Ok(())
    }

    async fn notify_challenge(&self, peer_id: &str, frame: &[u8]) -> Result<(), PeripheralError> {
        let peers = self.peers.lock().await;
        let peer = peers.get(peer_id).ok_or_else(|| PeripheralError::UnknownPeer {
            peer_id: peer_id.to_owned(),
        })?;
        let notifier_slot = peer.notifier_slot.clone();
        // Drain any stale response from a previous timed-out challenge.
        // Without this, the next wait_for_response would grab the old
        // bytes and verify them against the new nonce → bad-signature.
        {
            let mut rx = peer.response_rx.lock().await;
            while rx.try_recv().is_ok() {}
        }
        drop(peers);
        let mut slot = notifier_slot.lock().await;
        let Some(writer) = slot.as_mut() else {
            tracing::warn!(target: "syauth_transport", peer_id=%peer_id, "notify_challenge: notifier_slot=None — phone never subscribed (or task missed event)");
            return Err(PeripheralError::Backend {
                reason: format!("no active GATT subscription for peer_id={peer_id}"),
            });
        };
        use tokio::io::AsyncWriteExt;
        tracing::info!(target: "syauth_transport", peer_id=%peer_id, bytes=frame.len(), "notify_challenge: writing frame");
        match writer.write_all(frame).await {
            Ok(()) => Ok(()),
            Err(err) => {
                // The cached BlueZ CharacteristicWriter is dead — the phone's
                // CCCD subscription went stale (out-of-range, suspend/resume,
                // app restart, radio glitch). Field testing showed that
                // simply Device::disconnect()ing the LE link is NOT enough:
                // BlueZ keeps the per-characteristic subscription state across
                // link transitions, so when the phone reconnects and writes
                // CCCD again, BlueZ silently merges that into the existing
                // subscription and never emits a fresh
                // `CharacteristicControlEvent::Notify` to our application.
                // The cached writer in `notifier_slot` stays dead forever
                // until the daemon restarts.
                //
                // Real recovery: unregister and re-register the entire
                // per-peer application via `rebuild_peer_registration`.
                // bluer issues `UnregisterApplication` on `ApplicationHandle`
                // drop, which discards BlueZ's subscription state. The fresh
                // registration's chal_control stream then emits `Notify` the
                // first time the (kicked) phone re-subscribes against it.
                // This call still fails (FIDO takes over once), but the
                // next challenge after the phone watchdog reconnects lands
                // on a healthy writer.
                tracing::warn!(
                    target: "syauth_transport",
                    peer_id = %peer_id,
                    error = %err,
                    "notify_challenge: cached writer dead — rebuilding GATT application"
                );
                *slot = None;
                drop(slot);
                if let Err(rebuild_err) = self.rebuild_peer_registration(peer_id).await {
                    tracing::warn!(
                        target: "syauth_transport",
                        peer_id = %peer_id,
                        error = %rebuild_err,
                        "notify_challenge recovery: rebuild_peer_registration failed"
                    );
                }
                Err(PeripheralError::Backend {
                    reason: format!("notify_challenge write: {err}"),
                })
            }
        }
    }

    async fn wait_for_response(&self, peer_id: &str, deadline: Duration) -> Result<Vec<u8>, PeripheralError> {
        // We hold the outer peers-map lock across the await on
        // `rx.recv()`. S-003/S-006 do not exercise concurrent
        // challenges on the SAME peer — SPEC §3 scope item #7 caps
        // in-flight challenges at one per peer — so the lock-across-
        // await pattern is correct for the persistent peripheral.
        // The fake exposes a more parallel-friendly shape because
        // its tests are the only place that exercise multi-peer
        // concurrency in CI.
        let peers = self.peers.lock().await;
        let peer = peers.get(peer_id).ok_or_else(|| PeripheralError::UnknownPeer {
            peer_id: peer_id.to_owned(),
        })?;
        let mut rx = peer.response_rx.lock().await;
        match tokio::time::timeout(deadline, rx.recv()).await {
            Ok(Some(bytes)) => Ok(bytes),
            Ok(None) => Err(PeripheralError::Backend {
                reason: format!("response channel closed for peer_id={peer_id}"),
            }),
            Err(_) => Err(PeripheralError::ResponseTimeout {
                peer_id: peer_id.to_owned(),
                deadline_ms: u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// FakePeripheral — radio-free test double.
// ---------------------------------------------------------------------------

/// Test-only state recorded by [`FakePeripheral`]. Held behind a
/// `Mutex` so the fake stays `Send + Sync` like the production impl.
#[cfg(any(test, feature = "test-fake"))]
#[derive(Default)]
struct FakeState {
    /// Peers in insertion order so tests can assert on the order
    /// after a `remove_peer` in the middle of the sequence.
    peers_in_order: Vec<String>,
    /// Bond keys keyed by peer_id, mirroring the production state.
    peer_keys: HashMap<String, BondKey>,
    /// Every `set_session_uuids` argument, recorded in call order.
    session_uuid_calls: Vec<std::collections::HashSet<Uuid>>,
    /// Every `notify_challenge` argument, recorded in call order, so
    /// later daemon tests (S-006) can assert on the sequence.
    notify_calls: Vec<(String, Vec<u8>)>,
    /// Per-peer FIFO of queued response bytes. `inject_response`
    /// pushes onto the back; `wait_for_response` pops from the
    /// front. An empty FIFO when `wait_for_response` is called
    /// means "the response never arrived" — the fake returns
    /// `PeripheralError::ResponseTimeout` after the deadline.
    response_queue: HashMap<String, std::collections::VecDeque<Vec<u8>>>,
}

/// Radio-free `Peripheral` for tests and CI.
///
/// Records every call in order so test assertions read like
/// requirements. The internal state is held behind a `std::sync::Mutex`
/// (not a `tokio::sync::Mutex`) so the synchronous getters
/// (`peers`, `session_uuid_calls`, `notify_calls`) can be called from
/// inside a `#[tokio::test]` body without panicking on
/// `block_on_runtime`. The lock is held briefly and never across an
/// `await`, so a `std::sync::Mutex` is the right primitive.
#[cfg(any(test, feature = "test-fake"))]
pub struct FakePeripheral {
    state: std::sync::Mutex<FakeState>,
}

#[cfg(any(test, feature = "test-fake"))]
impl FakePeripheral {
    /// Construct a fresh `FakePeripheral` with no peers, no advertised
    /// UUIDs, and no recorded calls.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Snapshot of the currently-registered peers in insertion order
    /// (with the natural gap when a middle peer was removed).
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned by a prior thread
    /// panic. Tests never share `FakePeripheral` across panicking
    /// tasks, so this is unreachable in normal operation.
    #[must_use]
    pub fn peers(&self) -> Vec<String> {
        match self.state.lock() {
            Ok(g) => g.peers_in_order.clone(),
            Err(poisoned) => poisoned.into_inner().peers_in_order.clone(),
        }
    }

    /// Snapshot of every `set_session_uuids` argument in call order.
    /// Tests assert on the full sequence so a regression that drops or
    /// merges intermediate calls is mechanically visible.
    #[must_use]
    pub fn session_uuid_calls(&self) -> Vec<std::collections::HashSet<Uuid>> {
        match self.state.lock() {
            Ok(g) => g.session_uuid_calls.clone(),
            Err(poisoned) => poisoned.into_inner().session_uuid_calls.clone(),
        }
    }

    /// Snapshot of every `notify_challenge` argument in call order.
    #[must_use]
    pub fn notify_calls(&self) -> Vec<(String, Vec<u8>)> {
        match self.state.lock() {
            Ok(g) => g.notify_calls.clone(),
            Err(poisoned) => poisoned.into_inner().notify_calls.clone(),
        }
    }

    /// Queue a synthetic response for the next
    /// [`Peripheral::wait_for_response`] call on `peer_id`. Tests
    /// inject a valid signed response for the success path and
    /// garbage bytes for the bad-signature path. A peer that was
    /// never `add_peer`'d still accepts queued responses — the
    /// trait call surfaces the `UnknownPeer` error from
    /// `wait_for_response`, not from `inject_response`.
    pub fn inject_response(&self, peer_id: &str, bytes: Vec<u8>) {
        let mut state = self.lock_state();
        state.response_queue.entry(peer_id.to_owned()).or_default().push_back(bytes);
    }

    /// Acquire the inner lock, transparently recovering from any
    /// poisoning. Used by the trait methods so they never propagate a
    /// `PoisonError` outside the fake.
    fn lock_state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        match self.state.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[cfg(any(test, feature = "test-fake"))]
impl Default for FakePeripheral {
    fn default() -> Self {
        Self {
            state: std::sync::Mutex::new(FakeState::default()),
        }
    }
}

#[cfg(any(test, feature = "test-fake"))]
#[async_trait]
impl Peripheral for FakePeripheral {
    async fn add_peer(&self, peer_id: &str, bond_key: &BondKey) -> Result<(), PeripheralError> {
        let mut state = self.lock_state();
        if state.peer_keys.contains_key(peer_id) {
            return Err(PeripheralError::PeerAlreadyAdded {
                peer_id: peer_id.to_owned(),
            });
        }
        state.peers_in_order.push(peer_id.to_owned());
        state.peer_keys.insert(peer_id.to_owned(), *bond_key);
        Ok(())
    }

    async fn remove_peer(&self, peer_id: &str) -> Result<(), PeripheralError> {
        let mut state = self.lock_state();
        if state.peer_keys.remove(peer_id).is_none() {
            return Err(PeripheralError::UnknownPeer {
                peer_id: peer_id.to_owned(),
            });
        }
        state.peers_in_order.retain(|p| p != peer_id);
        Ok(())
    }

    async fn set_session_uuids(&self, uuids: std::collections::HashSet<Uuid>) -> Result<(), PeripheralError> {
        let mut state = self.lock_state();
        state.session_uuid_calls.push(uuids);
        Ok(())
    }

    async fn notify_challenge(&self, peer_id: &str, frame: &[u8]) -> Result<(), PeripheralError> {
        let mut state = self.lock_state();
        if !state.peer_keys.contains_key(peer_id) {
            return Err(PeripheralError::UnknownPeer {
                peer_id: peer_id.to_owned(),
            });
        }
        state.notify_calls.push((peer_id.to_owned(), frame.to_vec()));
        Ok(())
    }

    async fn wait_for_response(&self, peer_id: &str, deadline: Duration) -> Result<Vec<u8>, PeripheralError> {
        // Membership check before any wait so the typed UnknownPeer
        // branch fires deterministically when callers pass a
        // peer_id that was never `add_peer`'d.
        {
            let state = self.lock_state();
            if !state.peer_keys.contains_key(peer_id) {
                return Err(PeripheralError::UnknownPeer {
                    peer_id: peer_id.to_owned(),
                });
            }
        }
        let outcome = tokio::time::timeout(deadline, async {
            loop {
                {
                    let mut state = self.lock_state();
                    if let Some(queue) = state.response_queue.get_mut(peer_id)
                        && let Some(bytes) = queue.pop_front()
                    {
                        return bytes;
                    }
                }
                tokio::time::sleep(FAKE_RESPONSE_POLL_INTERVAL).await;
            }
        })
        .await;
        match outcome {
            Ok(bytes) => Ok(bytes),
            Err(_) => Err(PeripheralError::ResponseTimeout {
                peer_id: peer_id.to_owned(),
                deadline_ms: u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX),
            }),
        }
    }
}

/// Cadence at which [`FakePeripheral::wait_for_response`] polls its
/// injected-response FIFO inside the `tokio::time::timeout` wrapper.
/// A 10 ms cadence under `tokio::test(start_paused = true)` consumes
/// negligible virtual time; under wall-clock tests it makes the
/// success path return well within a millisecond of `inject_response`.
#[cfg(any(test, feature = "test-fake"))]
const FAKE_RESPONSE_POLL_INTERVAL: Duration = Duration::from_millis(10);

// ---------------------------------------------------------------------------
// Tests — unit tests for the shared builder + the trait's object safety.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // Journey: specs/journeys/JOURNEY-S-003-peripheral-library-api.md
    use super::*;

    /// `PersistentPeripheral::build_advertisement` carries the
    /// daemon-side defaults (local name, discoverable, the requested
    /// UUID set). Pure-function so this test runs without an adapter.
    #[test]
    fn build_advertisement_carries_local_name_and_uuids() {
        let uuid = Uuid::from_u128(0x5a4e_8e3c_1c4c_4a17_9c81_d518_a55a_3001);
        let uuids: std::collections::HashSet<Uuid> = [uuid].into_iter().collect();
        let adv = PersistentPeripheral::build_advertisement(uuids);
        assert_eq!(adv.local_name.as_deref(), Some(ADVERTISE_LOCAL_NAME));
        assert_eq!(adv.discoverable, Some(ADVERTISE_DISCOVERABLE));
        let expected: std::collections::BTreeSet<Uuid> = [uuid].into_iter().collect();
        assert_eq!(adv.service_uuids, expected);
    }

    /// Trait is object-safe and the bounds (`Send + Sync`) let an
    /// `Arc<dyn Peripheral>` compile. This is the daemon's actual
    /// usage pattern; pinning it here prevents an accidental
    /// non-object-safe extension in future steps.
    #[test]
    fn trait_is_object_safe_and_send_sync() {
        let fake = FakePeripheral::new();
        let dyn_ref: Arc<dyn Peripheral> = fake;
        // Force `Send + Sync` checks at compile time.
        fn assert_send_sync<T: Send + Sync + ?Sized>(_: &T) {}
        assert_send_sync(&*dyn_ref);
    }

    /// `From<TransportError>` mapping: `AdapterMissing` keeps its
    /// structural variant; anything else collapses to `Backend`.
    #[test]
    fn transport_error_maps_to_peripheral_error() {
        let mapped = PeripheralError::from(TransportError::AdapterMissing { name: "hci99".to_owned() });
        match mapped {
            PeripheralError::AdapterMissing { name } => assert_eq!(name, "hci99"),
            other => panic!("expected AdapterMissing, got {other:?}"),
        }
        let mapped = PeripheralError::from(TransportError::Closed);
        match mapped {
            PeripheralError::Backend { reason } => assert!(reason.contains("closed")),
            other => panic!("expected Backend, got {other:?}"),
        }
    }

    /// FakePeripheral records both add and notify call sequences.
    #[tokio::test]
    async fn fake_peripheral_records_notify_calls() {
        let fake = FakePeripheral::new();
        fake.add_peer("a", &[0xAA; BOND_KEY_BYTES]).await.expect("add a");
        fake.notify_challenge("a", &[0x01, 0x02, 0x03]).await.expect("notify a");
        let calls = fake.notify_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "a");
        assert_eq!(calls[0].1, vec![0x01, 0x02, 0x03]);
    }

    /// FakePeripheral.inject_response → wait_for_response returns the
    /// injected bytes.
    #[tokio::test]
    async fn fake_peripheral_wait_for_response_returns_injected_bytes() {
        let fake = FakePeripheral::new();
        fake.add_peer("a", &[0xAA; BOND_KEY_BYTES]).await.expect("add a");
        fake.inject_response("a", vec![0x10, 0x20, 0x30]);
        let bytes = fake
            .wait_for_response("a", Duration::from_millis(50))
            .await
            .expect("response present");
        assert_eq!(bytes, vec![0x10, 0x20, 0x30]);
    }

    /// FakePeripheral.wait_for_response on an empty queue returns
    /// `ResponseTimeout` after the deadline. Wall-clock budget is
    /// the deadline value (50 ms) since this unit test does not
    /// pull tokio's `test-util` feature.
    #[tokio::test]
    async fn fake_peripheral_wait_for_response_times_out_when_empty() {
        let fake = FakePeripheral::new();
        fake.add_peer("a", &[0xAA; BOND_KEY_BYTES]).await.expect("add a");
        let deadline = Duration::from_millis(50);
        let result = fake.wait_for_response("a", deadline).await;
        match result {
            Err(PeripheralError::ResponseTimeout { peer_id, deadline_ms }) => {
                assert_eq!(peer_id, "a");
                assert_eq!(deadline_ms, 50);
            }
            other => panic!("expected ResponseTimeout, got {other:?}"),
        }
    }

    /// FakePeripheral.wait_for_response on an unknown peer returns
    /// `UnknownPeer` deterministically (no wait).
    #[tokio::test]
    async fn fake_peripheral_wait_for_response_rejects_unknown_peer() {
        let fake = FakePeripheral::new();
        let result = fake.wait_for_response("ghost", Duration::from_millis(50)).await;
        match result {
            Err(PeripheralError::UnknownPeer { peer_id }) => assert_eq!(peer_id, "ghost"),
            other => panic!("expected UnknownPeer, got {other:?}"),
        }
    }

    /// FakePeripheral refuses to add the same peer twice.
    #[tokio::test]
    async fn fake_peripheral_rejects_duplicate_add() {
        let fake = FakePeripheral::new();
        fake.add_peer("a", &[0xAA; BOND_KEY_BYTES]).await.expect("first");
        let err = fake.add_peer("a", &[0xBB; BOND_KEY_BYTES]).await.expect_err("dup");
        match err {
            PeripheralError::PeerAlreadyAdded { peer_id } => assert_eq!(peer_id, "a"),
            other => panic!("expected PeerAlreadyAdded, got {other:?}"),
        }
    }
}
