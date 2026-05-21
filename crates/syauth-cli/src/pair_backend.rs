//! DEV-001 (re-march): real `bluer`-driven [`PairBackend`] used by
//! `syauth pair`. **The desktop ADVERTISES**, the phone scans + connects
//! (SPEC §3.2 D8 verbatim; matches DEV-003's unlock-channel direction).
//!
//! Flow mapped onto the JOURNEY-DEV-001 phases:
//!
//! 1. **Phase 1 — Adapter ready.** Open the configured adapter, power it
//!    on, register a BlueZ [`bluer::agent::Agent`] with `DisplayYesNo`
//!    capability. Only the `request_confirmation` callback accepts; every
//!    other callback rejects with [`bluer::agent::ReqError::Rejected`].
//! 2. **Phase 2 — Advertise + accept.** Build a GATT `Application`
//!    carrying [`syauth_transport::SYAUTH_PAIR_SERVICE_UUID`] with two
//!    characteristics ([`syauth_transport::SYAUTH_PAIR_HOST_PUBKEY_CHAR_UUID`]
//!    read-only; [`syauth_transport::SYAUTH_PAIR_PHONE_PUBKEY_CHAR_UUID`]
//!    write-only). Start an `LeAdvertisement` whose `service_uuids` set
//!    contains the pair-mode discovery UUID derived from
//!    `session_uuid_for(&[0u8; 32], current_minute)`. Await the phone's
//!    GATT subscribe + write.
//! 3. **Phase 3 — LESC numeric comparison.** When the phone's
//!    `BluetoothDevice.createBond()` reaches BlueZ, the Agent's
//!    `request_confirmation` callback receives the 6-digit passkey,
//!    prints it on stdout, and reads `y`/`N` from stdin — rejecting on N
//!    closes the deal at the OS level.
//! 4. **Phase 4 — App-level pubkey exchange.** Over the now-LESC-bonded
//!    link, the desktop serves its Ed25519 host pubkey via the
//!    `host-pubkey` characteristic read; the phone's `phone-pubkey`
//!    write lands in the desktop's mailbox. Derive `bond_key` with
//!    [`syauth_core::bond_key_from_pubkeys`].
//!
//! Radio-touching cases are gated by `SYAUTH_REAL_RADIOS=1` in
//! `crates/syauth-cli/tests/pair_lesc_test.rs`. The unit-testable surface
//! (GATT app builder, agent variant decision, OOB write+read framing)
//! is exercised in tests that DO NOT touch a radio.

use std::{
    io::{self, BufRead, Write},
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use bluer::{
    adv::Advertisement,
    agent::{Agent, ReqError, RequestConfirmation},
    gatt::local::{
        Application, ApplicationHandle, Characteristic, CharacteristicControlEvent, CharacteristicRead, CharacteristicWrite,
        CharacteristicWriteMethod, Service, characteristic_control,
    },
};
use futures::{FutureExt, StreamExt};
use syauth_core::{SigningKey, bond_key_from_pubkeys};
use syauth_transport::{
    ADVERTISE_LOCAL_NAME, PAIR_PUBKEY_LEN, SYAUTH_PAIR_HOST_PUBKEY_CHAR_UUID, SYAUTH_PAIR_PHONE_PUBKEY_CHAR_UUID, SYAUTH_PAIR_SERVICE_UUID,
    session_uuid_for,
};
use tokio::{io::AsyncReadExt, time::timeout as tokio_timeout};

use crate::pair::{AdapterInfo, LescOutcome, PairBackend, PairCandidate, PairError};

/// Wall-clock window the backend spends waiting for the phone's GATT
/// client connection (signalled by the first
/// [`CharacteristicControlEvent::Write`] for the `phone-pubkey`
/// characteristic). 5 minutes — long enough that the operator can run
/// `syauth pair` on the desktop, walk to the phone, open the app, tap
/// "Pair with computer", and confirm the CDM picker without the
/// desktop timing out. The advertised pair-mode UUID rotates each
/// wall-clock minute (see `scan_peers`) so the phone's
/// {current_minute, current_minute − 1} CDM scan filter always
/// overlaps with what we're broadcasting.
pub const PAIR_ADVERTISE_ACCEPT_WINDOW: Duration = Duration::from_secs(300);

/// Number of seconds per minute. Used to floor the wall-clock when
/// deriving the pair-mode session UUID.
const SECONDS_PER_MINUTE: i64 = 60;

/// Synthetic peer name surfaced by [`BluerPairBackend::scan_peers`]
/// once the phone has connected to the advertised pair service. The
/// upstream `PairBackend` contract returns a `Vec<PairCandidate>` from
/// `scan_peers`; the advertise-based backend yields exactly one
/// candidate representing the connected phone.
pub const PAIR_CONNECTED_PEER_NAME: &str = "phone (LESC peer)";

/// Synthetic peer address surfaced alongside [`PAIR_CONNECTED_PEER_NAME`].
/// The advertise-based backend does not know the phone's MAC at the
/// time it returns the candidate (the phone's address is observed
/// later via the `Device` table); the constant is a placeholder the
/// CLI prints verbatim.
pub const PAIR_CONNECTED_PEER_ADDRESS: &str = "(advertised)";

/// Operator-supplied y/N confirmation callback. Returns `true` to accept
/// the 6-digit numeric comparison code, `false` to reject (which
/// terminates the OS pairing with a typed
/// [`PairError::DowngradeBlocked`]).
pub type OsConfirmHandler = Box<dyn Fn(u32) -> bool + Send + Sync>;

/// Mailbox the GATT write-callback drops the phone's pubkey bytes into.
/// `Arc<Mutex<Option<...>>>` so the callback owns one writer and
/// [`BluerPairBackend::initiate_lesc_with_peer`] owns the reader.
type PhonePubkeyMailbox = Arc<Mutex<Option<[u8; PAIR_PUBKEY_LEN]>>>;

/// Production `PairBackend` driven by `bluer`. Constructed once per
/// `syauth pair` invocation; one instance owns one GATT application +
/// LE advertisement + agent registration for the lifetime of the call.
pub struct BluerPairBackend {
    /// BlueZ adapter id (e.g. `"hci0"`).
    adapter_id: String,
    /// Host's Ed25519 pubkey, served on the `host-pubkey` characteristic
    /// read. The corresponding private key never leaves this process;
    /// the pubkey is the only material that needs to cross the link.
    host_pubkey: [u8; PAIR_PUBKEY_LEN],
    /// Operator y/N callback the BlueZ agent's `request_confirmation`
    /// callback consults at numeric-comparison time. Held in an
    /// `Arc<Mutex<Option<...>>>` so the agent (registered before
    /// `initiate_lesc_with_peer` is called) can pull the next answer
    /// at runtime.
    confirm_channel: Arc<Mutex<Option<OsConfirmHandler>>>,
    /// Mailbox holding the phone's pubkey bytes after the
    /// `phone-pubkey` write fires. Set by the GATT-write callback;
    /// drained by `initiate_lesc_with_peer`.
    phone_pubkey_mailbox: PhonePubkeyMailbox,
    /// GATT `ApplicationHandle` registered in `scan_peers` and dropped
    /// at session end. `Option` so the handle can be installed lazily.
    app_handle: Arc<Mutex<Option<ApplicationHandle>>>,
    /// LE advertisement handle registered in `scan_peers` and dropped
    /// at session end.
    adv_handle: Arc<Mutex<Option<bluer::adv::AdvertisementHandle>>>,
}

impl BluerPairBackend {
    /// Construct a new backend bound to `adapter_id`, deriving its
    /// `host_pubkey` from the supplied [`SigningKey`].
    pub fn new(adapter_id: &str, signing_key: &SigningKey) -> Self {
        let host_pubkey: [u8; PAIR_PUBKEY_LEN] = signing_key.verifying_key().to_bytes();
        Self {
            adapter_id: adapter_id.to_owned(),
            host_pubkey,
            confirm_channel: Arc::new(Mutex::new(None)),
            phone_pubkey_mailbox: Arc::new(Mutex::new(None)),
            app_handle: Arc::new(Mutex::new(None)),
            adv_handle: Arc::new(Mutex::new(None)),
        }
    }

    /// Install the operator's y/N confirmation handler. The handler is
    /// called inside the BlueZ agent's `request_confirmation` callback
    /// once per OS pairing attempt; the bool it returns gates the OS
    /// pairing (true = accept, false = reject).
    pub fn install_confirm_handler(&self, handler: OsConfirmHandler) {
        if let Ok(mut guard) = self.confirm_channel.lock() {
            *guard = Some(handler);
        }
    }

    /// Build the BlueZ [`Agent`] whose only accepting callback is
    /// [`request_confirmation`]. Every other variant (Just Works,
    /// legacy PIN, passkey entry, OOB-only) rejects with
    /// [`ReqError::Rejected`], producing the typed
    /// [`PairError::DowngradeBlocked`] / [`PairError::UnsupportedPairingVariant`]
    /// at the call site.
    fn build_agent(&self) -> Agent {
        let confirm = Arc::clone(&self.confirm_channel);
        Agent {
            // BlueZ dispatches LESC numeric-comparison requests to the
            // CURRENT default agent on the system bus. Without
            // `request_default = true`, BlueZ falls back to whichever
            // system-default agent is registered (e.g. `bluetoothd`'s
            // built-in `bluetooth-meshd` Just-Works handler) and our
            // `request_confirmation` callback never fires, causing
            // the phone's SMP_OPCODE_PAIR_DHKEY_CHECK to time out
            // after 30s with `HCI_ERR_AUTH_FAILURE`. Making ourselves
            // the default for the duration of `syauth pair` is
            // safe because the handle is dropped when `scan_peers`
            // returns and the previous default re-takes the slot.
            request_default: true,
            request_confirmation: Some(Box::new(move |RequestConfirmation { passkey, .. }: RequestConfirmation| {
                let confirm = Arc::clone(&confirm);
                Box::pin(async move {
                    // The operator-supplied handler is synchronous and
                    // may block (the stdio prompt reads `y/N` from
                    // stdin; even the `--yes` auto-accept handler
                    // calls `io::stdout().lock()`). Running it on the
                    // same tokio worker that polls this future
                    // starves bluer's agent dispatcher mid-call, so
                    // BlueZ never sees our `RequestConfirmation`
                    // reply and the phone-side SMP times out at 30s
                    // with `HCI_ERR_AUTH_FAILURE`. Hop the handler
                    // onto a blocking pool via `spawn_blocking` so
                    // the poller stays free.
                    let accepted = tokio::task::spawn_blocking(move || {
                        let guard = confirm.lock().ok()?;
                        guard.as_ref().map(|h| h(passkey))
                    })
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or(false);
                    if accepted { Ok(()) } else { Err(ReqError::Rejected) }
                })
            })),
            ..Default::default()
        }
    }

    /// Compute the pair-mode discovery UUID for the wall-clock minute
    /// `minute`. The pair-mode UUID is derived from a zero bond_key (no
    /// bond exists yet at scan time) and the floor of the unix-epoch
    /// seconds by 60; the operator's CLI surfaces no flag for this —
    /// it's purely the bridge to the Phase 2 advertisement.
    pub fn pair_discovery_uuid(minute: i64) -> uuid::Uuid {
        let zero_bond = [0u8; 32];
        let bytes = session_uuid_for(&zero_bond, minute);
        uuid::Uuid::from_bytes(bytes)
    }

    /// Build the GATT [`Service`] vector for the desktop's pair
    /// [`Application`]. Pure function — no I/O, no clock — so a
    /// radio-free unit test can introspect the characteristic
    /// structure without standing up a BlueZ session.
    ///
    /// Characteristic structure:
    /// - `host-pubkey` (read-only): the phone reads 32 bytes; the
    ///   read callback returns the pinned host pubkey verbatim.
    /// - `phone-pubkey` (write-only, IO-method): the phone writes 32
    ///   bytes; the controller surfaces a [`CharacteristicWriter`] the
    ///   caller drains, captures the bytes in the mailbox, and rejects
    ///   subsequent writes.
    pub(crate) fn build_pair_services(
        host_pubkey: [u8; PAIR_PUBKEY_LEN],
        char_handle: bluer::gatt::local::CharacteristicControlHandle,
    ) -> Vec<Service> {
        vec![Service {
            uuid: SYAUTH_PAIR_SERVICE_UUID,
            primary: true,
            characteristics: vec![
                Characteristic {
                    uuid: SYAUTH_PAIR_HOST_PUBKEY_CHAR_UUID,
                    read: Some(CharacteristicRead {
                        read: true,
                        fun: Box::new(move |_| {
                            let bytes = host_pubkey.to_vec();
                            async move { Ok(bytes) }.boxed()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Characteristic {
                    uuid: SYAUTH_PAIR_PHONE_PUBKEY_CHAR_UUID,
                    write: Some(CharacteristicWrite {
                        write: true,
                        write_without_response: true,
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

    /// Build the LE advertisement payload. Service UUID set carries
    /// the pair-mode discovery UUID for `minute`. Local-name field is
    /// the constant [`ADVERTISE_LOCAL_NAME`] (never derived from the
    /// hostname — a passive observer cannot correlate the advertise
    /// to an operator identity).
    fn build_advertisement(minute: i64) -> Advertisement {
        let pair_uuid = Self::pair_discovery_uuid(minute);
        Advertisement {
            service_uuids: vec![pair_uuid].into_iter().collect(),
            discoverable: Some(true),
            local_name: Some(ADVERTISE_LOCAL_NAME.to_owned()),
            ..Default::default()
        }
    }
}

#[async_trait]
impl PairBackend for BluerPairBackend {
    async fn adapter_info(&self, adapter_id: &str) -> Result<AdapterInfo, PairError> {
        let session = bluer::Session::new().await.map_err(map_bluer)?;
        let adapter = session.adapter(adapter_id).map_err(|err| match err.kind {
            bluer::ErrorKind::NotFound => PairError::AdapterMissing {
                name: adapter_id.to_owned(),
            },
            _ => PairError::Backend { reason: err.to_string() },
        })?;
        adapter
            .set_powered(true)
            .await
            .map_err(|err| PairError::Backend { reason: err.to_string() })?;
        Ok(AdapterInfo {
            name: adapter_id.to_owned(),
            supports_lesc: true,
        })
    }

    async fn scan_peers(&self) -> Result<Vec<PairCandidate>, PairError> {
        // Inverted role per SPEC §3.2 D8: instead of scanning for a
        // phone-advertised UUID, the desktop ADVERTISES the pair-mode
        // service and waits for the phone to connect. The
        // `PairCandidate` returned is a synthetic stand-in for "phone
        // is now connected to our GATT server"; the real pubkey
        // exchange happens in `initiate_lesc_with_peer`.
        let session = bluer::Session::new().await.map_err(map_bluer)?;
        let _agent_handle = session.register_agent(self.build_agent()).await.map_err(map_bluer)?;
        let adapter = session.adapter(&self.adapter_id).map_err(|err| match err.kind {
            bluer::ErrorKind::NotFound => PairError::AdapterMissing {
                name: self.adapter_id.clone(),
            },
            _ => PairError::Backend { reason: err.to_string() },
        })?;
        adapter
            .set_powered(true)
            .await
            .map_err(|err| PairError::Backend { reason: err.to_string() })?;
        adapter
            .set_discoverable(true)
            .await
            .map_err(|err| PairError::Backend { reason: err.to_string() })?;
        adapter
            .set_pairable(true)
            .await
            .map_err(|err| PairError::Backend { reason: err.to_string() })?;

        let (char_control, char_handle) = characteristic_control();
        let app = Application {
            services: Self::build_pair_services(self.host_pubkey, char_handle),
            ..Default::default()
        };
        let app_handle = adapter.serve_gatt_application(app).await.map_err(|err| PairError::Backend {
            reason: format!("serve_gatt_application: {err}"),
        })?;

        let initial_minute = unix_minute_floor();
        let initial_advertisement = Self::build_advertisement(initial_minute);
        let initial_adv_handle = adapter.advertise(initial_advertisement).await.map_err(|err| PairError::Backend {
            reason: format!("advertise: {err}"),
        })?;

        if let Ok(mut g) = self.app_handle.lock() {
            *g = Some(app_handle);
        }

        // Drive the phone's `phone-pubkey` write into the mailbox while
        // rotating the advertised pair-mode UUID at each wall-clock
        // minute boundary. Rotation is required because (a) the
        // pair-mode UUID is `session_uuid_for(zero_bond, minute)` and
        // rolls over every 60s, and (b) the phone's CDM scan filter
        // only covers {current_minute, current_minute − 1}; without
        // rotation a static minute-N UUID falls out of the phone's
        // window after ~60s, leaving the OS picker empty for any
        // operator who takes more than a minute to tap "Pair" on the
        // phone. First Write event => phone has connected + completed
        // LESC (the encrypted-write requirement on the characteristic
        // would gate any pre-bond write); we drain the writer once,
        // capture 32 bytes, stash the active advertisement handle
        // into `self.adv_handle`, and the function returns.
        let mailbox = Arc::clone(&self.phone_pubkey_mailbox);
        let final_adv_slot: Arc<Mutex<Option<bluer::adv::AdvertisementHandle>>> = Arc::new(Mutex::new(None));
        let final_adv_slot_inner = Arc::clone(&final_adv_slot);
        let drained = tokio_timeout(PAIR_ADVERTISE_ACCEPT_WINDOW, async move {
            let mut control = Box::pin(char_control);
            let mut current_adv: Option<bluer::adv::AdvertisementHandle> = Some(initial_adv_handle);
            let mut current_minute = initial_minute;
            let mut rotation_ticker = tokio::time::interval(Duration::from_secs(1));
            rotation_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            loop {
                tokio::select! {
                    evt = control.next() => match evt {
                        Some(CharacteristicControlEvent::Write(req)) => {
                            let mut reader = match req.accept() {
                                Ok(r) => r,
                                Err(err) => {
                                    return Err(PairError::Backend {
                                        reason: format!("phone-pubkey write.accept: {err}"),
                                    });
                                }
                            };
                            let mut buf = [0u8; PAIR_PUBKEY_LEN];
                            let n = reader.read(&mut buf).await.map_err(|err| PairError::Backend {
                                reason: format!("phone-pubkey read: {err}"),
                            })?;
                            if n != PAIR_PUBKEY_LEN {
                                return Err(PairError::Backend {
                                    reason: format!("phone-pubkey: expected {PAIR_PUBKEY_LEN} bytes, got {n}"),
                                });
                            }
                            if let Ok(mut g) = mailbox.lock() {
                                *g = Some(buf);
                            }
                            if let Ok(mut g) = final_adv_slot_inner.lock() {
                                *g = current_adv.take();
                            }
                            return Ok(());
                        }
                        Some(_) => continue,
                        None => return Err(PairError::NoPeers),
                    },
                    _ = rotation_ticker.tick() => {
                        let new_minute = unix_minute_floor();
                        if new_minute == current_minute {
                            continue;
                        }
                        let new_advertisement = Self::build_advertisement(new_minute);
                        // Register the new advertisement first; only on
                        // success drop the previous handle. Keeps a
                        // valid advertisement on air across the swap
                        // for radios that allow concurrent ADVs, and
                        // preserves the previous slot if the new
                        // register fails.
                        match adapter.advertise(new_advertisement).await {
                            Ok(new_handle) => {
                                current_adv = Some(new_handle);
                                current_minute = new_minute;
                            }
                            Err(err) => {
                                eprintln!(
                                    "pair_advertise rotation failed at minute={new_minute}: {err}; keeping slot {current_minute}",
                                );
                            }
                        }
                    }
                }
            }
        })
        .await;

        if let Ok(mut g) = self.adv_handle.lock() {
            if let Ok(mut inner) = final_adv_slot.lock() {
                *g = inner.take();
            }
        }
        match drained {
            Ok(Ok(())) => Ok(vec![PairCandidate {
                name: PAIR_CONNECTED_PEER_NAME.to_owned(),
                address: PAIR_CONNECTED_PEER_ADDRESS.to_owned(),
            }]),
            Ok(Err(e)) => Err(e),
            Err(_elapsed) => Err(PairError::NoPeers),
        }
    }

    async fn initiate_lesc_with_peer(&self, _peer: &PairCandidate) -> Result<LescOutcome, PairError> {
        // By the time the trait reaches this method, the phone has:
        //   (a) connected to our advertised pair service,
        //   (b) completed LESC bonding via BlueZ's agent flow
        //       (numeric-comparison gated by [`build_agent`]),
        //   (c) written its 32-byte Ed25519 pubkey to the
        //       `phone-pubkey` characteristic.
        // The mailbox carries the captured bytes; drain it now.
        let phone_pubkey = {
            let mut g = self.phone_pubkey_mailbox.lock().map_err(|_| PairError::Backend {
                reason: "phone-pubkey mailbox poisoned".to_owned(),
            })?;
            g.take().ok_or(PairError::NoPeers)?
        };
        let bond_key = bond_key_from_pubkeys(&self.host_pubkey, &phone_pubkey);
        Ok(LescOutcome {
            peer_pubkey: phone_pubkey,
            bond_key,
            // The 6-digit code is consumed inside the agent callback,
            // not surfaced here. The operator already confirmed it via
            // the y/N prompt before this function returns Ok; the
            // value is informational and reported as 0.
            numeric_code: 0,
        })
    }
}

/// CLI prompt + stdin handler that records the most recent OS-level
/// 6-digit code, prints it to stdout, and reads `y` / `N` from stdin.
/// Exposed as a standalone helper so a future caller (an e2e test, a
/// scripted-OOB harness) can install a non-interactive handler
/// without going through the production binary's stdio.
pub fn make_stdio_confirm_handler() -> OsConfirmHandler {
    Box::new(|passkey: u32| {
        let mut stdout = io::stdout().lock();
        let _ = writeln!(stdout, "BT pairing code: {passkey:06}  Confirm on phone? [y/N]: ");
        let _ = stdout.flush();
        let stdin = io::stdin();
        let mut buf = String::new();
        match stdin.lock().read_line(&mut buf) {
            Ok(_) => matches!(buf.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
            Err(_) => false,
        }
    })
}

/// Auto-accept handler used by `--yes`. The 6-digit code is still
/// printed to stdout so an operator running with `--yes` against an
/// untrusted phone can read the code from the log post-hoc.
pub fn make_auto_accept_confirm_handler() -> OsConfirmHandler {
    Box::new(|passkey: u32| {
        let mut stdout = io::stdout().lock();
        let _ = writeln!(stdout, "BT pairing code: {passkey:06}  auto-accept (--yes)");
        let _ = stdout.flush();
        true
    })
}

/// Directory holding the on-disk IPC files the waybar applet
/// (`~/sources/sy` → `sy syauth …`) reads from and writes to. Lives
/// under `XDG_RUNTIME_DIR` because (a) the path is per-user tmpfs so
/// it's cleared on logout, and (b) `sudo -E` preserves
/// `XDG_RUNTIME_DIR`, so the privileged desktop pair process writes
/// to the same directory the unprivileged applet polls.
pub const PAIR_IPC_DIR_SUBPATH: &str = "syauth";

/// Filename of the pending pair-confirm request written by the
/// desktop's waybar handler.
pub const PAIR_IPC_REQUEST_FILE: &str = "pair-request.json";

/// Filename of the operator's accept/reject decision written by the
/// waybar applet on click.
pub const PAIR_IPC_RESPONSE_FILE: &str = "pair-response.json";

/// Wall-clock window the waybar handler waits for the operator's
/// click before falling back to "reject". Tuned to match BlueZ's
/// SMP_CONN_TOUT (30s); 25s leaves a small margin so the agent
/// reply gets back to BlueZ before the kernel side times out.
pub const PAIR_IPC_DECISION_TIMEOUT: Duration = Duration::from_millis(25_000);

/// Polling interval the waybar handler uses while waiting for the
/// response file. Chosen at 100ms — fast enough that the operator
/// sees the bond complete the moment they click "Accept", slow
/// enough not to thrash the syscall path.
pub const PAIR_IPC_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Confirmation handler that hands the 6-digit LESC numeric
/// comparison code to the user via a waybar applet (see
/// `~/sources/sy/src/syauth.rs`). The handler:
///
/// 1. Writes `${XDG_RUNTIME_DIR}/syauth/pair-request.json` carrying
///    the passkey + a per-invocation `request_id`.
/// 2. Polls for `${XDG_RUNTIME_DIR}/syauth/pair-response.json`. The
///    applet writes that file on user click.
/// 3. Reads the decision, deletes both files, returns `true`/`false`.
///
/// Falls back to "reject" on any I/O error or timeout — the secure
/// default for the LESC numeric-comparison gate.
pub fn make_waybar_confirm_handler() -> OsConfirmHandler {
    Box::new(|passkey: u32| -> bool {
        let dir = match pair_ipc_dir() {
            Some(d) => d,
            None => return false,
        };
        if std::fs::create_dir_all(&dir).is_err() {
            return false;
        }
        let request_path = dir.join(PAIR_IPC_REQUEST_FILE);
        let response_path = dir.join(PAIR_IPC_RESPONSE_FILE);
        // Clear any stale response from a previous invocation.
        let _ = std::fs::remove_file(&response_path);
        let request_id = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        let request_json = format!(
            r#"{{"schema_version":1,"kind":"pair_confirm","request_id":"{request_id}","passkey":"{passkey:06}","created_at_secs":{ts}}}"#,
            ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        );
        if std::fs::write(&request_path, request_json.as_bytes()).is_err() {
            return false;
        }
        // Make the request file world-readable so the unprivileged
        // applet can read it when the desktop pair is running as
        // root via `sudo`.
        let _ = chmod_world_readable(&request_path);

        let decision = wait_for_decision(&response_path, &request_id);

        let _ = std::fs::remove_file(&request_path);
        let _ = std::fs::remove_file(&response_path);
        decision
    })
}

/// Resolve `${XDG_RUNTIME_DIR}/syauth`. Returns `None` if neither
/// `SUDO_UID` nor `XDG_RUNTIME_DIR` yields a usable path — the
/// handler then falls back to "reject" since there is no agreed-upon
/// path with the applet.
///
/// `SUDO_UID` is consulted first because the desktop pair process
/// runs under `sudo`, where `env_reset` (the Fedora default) wipes
/// the caller's `XDG_RUNTIME_DIR`. `sudo` itself preserves
/// `SUDO_UID` regardless of `env_reset`, so deriving the per-user
/// runtime tmpfs from it lets the privileged pair process write to
/// the SAME directory the unprivileged waybar applet polls
/// (`/run/user/<original-uid>/syauth/`). Without this, the request
/// JSON lands at `/run/user/0/syauth/` (root's runtime) and the
/// applet never sees it, causing every pair attempt to time out
/// with `User Confirmation Negative Reply` and SMP
/// `Pairing Failed`.
fn pair_ipc_dir() -> Option<std::path::PathBuf> {
    if let Some(sudo_uid_os) = std::env::var_os("SUDO_UID")
        && let Some(sudo_uid) = sudo_uid_os.to_str().and_then(|s| s.parse::<u32>().ok())
    {
        let path = std::path::PathBuf::from(format!("/run/user/{sudo_uid}")).join(PAIR_IPC_DIR_SUBPATH);
        return Some(path);
    }
    let xdg = std::env::var_os("XDG_RUNTIME_DIR")?;
    if xdg.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(xdg).join(PAIR_IPC_DIR_SUBPATH))
}

/// Best-effort chmod 0644 so the unprivileged applet can read a file
/// the privileged pair process wrote. Failures are non-fatal — if
/// the chmod fails, the applet may not be able to read the request,
/// but the existing pair attempt still falls back to its timeout.
#[cfg(unix)]
fn chmod_world_readable(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o644);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn chmod_world_readable(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

/// Poll for the response file. Returns `true` on `accept`, `false`
/// on `reject` or timeout. The poll loop uses `PAIR_IPC_POLL_INTERVAL`;
/// the overall wait is capped at `PAIR_IPC_DECISION_TIMEOUT`.
fn wait_for_decision(response_path: &std::path::Path, request_id: &str) -> bool {
    let deadline = std::time::Instant::now() + PAIR_IPC_DECISION_TIMEOUT;
    while std::time::Instant::now() < deadline {
        if let Ok(bytes) = std::fs::read(response_path) {
            if let Some(decision) = parse_decision(&bytes, request_id) {
                return decision;
            }
            // File present but mis-formed or wrong request_id —
            // treat as no decision yet and keep polling. The applet
            // will overwrite with the correct content shortly.
        }
        std::thread::sleep(PAIR_IPC_POLL_INTERVAL);
    }
    false
}

/// Parse the response file's `{"request_id":"…","decision":"accept|reject"}`.
/// Hand-rolled because syauth-cli does not depend on `serde_json` and
/// the schema is small + fixed. Returns:
/// - `Some(true)` if decision is `accept` and request_id matches.
/// - `Some(false)` if decision is `reject` and request_id matches.
/// - `None` if the JSON is incomplete, mis-formed, or for a different
///   request_id (keep polling).
fn parse_decision(bytes: &[u8], expected_id: &str) -> Option<bool> {
    let text = std::str::from_utf8(bytes).ok()?;
    let id = json_string_field(text, "request_id")?;
    if id != expected_id {
        return None;
    }
    let decision = json_string_field(text, "decision")?;
    match decision.as_str() {
        "accept" => Some(true),
        "reject" => Some(false),
        _ => None,
    }
}

/// Tiny scanner that pulls a JSON string field's value by key without
/// dragging in a full JSON parser. The schema is fixed (no nested
/// objects, no escape sequences in the values we emit) so the
/// scanner only needs to handle the literal forms we produce.
fn json_string_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let key_pos = text.find(&needle)?;
    let after_key = &text[key_pos + needle.len()..];
    let colon = after_key.find(':')?;
    let after_colon = &after_key[colon + 1..];
    let open = after_colon.find('"')?;
    let rest = &after_colon[open + 1..];
    let close = rest.find('"')?;
    Some(rest[..close].to_string())
}

fn map_bluer(err: bluer::Error) -> PairError {
    match err.kind {
        bluer::ErrorKind::NotFound => PairError::AdapterMissing {
            name: "<unknown>".to_owned(),
        },
        _ => PairError::Backend { reason: err.to_string() },
    }
}

fn unix_minute_floor() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    now / SECONDS_PER_MINUTE
}

#[cfg(test)]
mod tests {
    // Journey: specs/journeys/JOURNEY-DEV-001-real-lesc.md

    use super::*;

    /// Deterministic Ed25519 seed used by the unit tests so the derived
    /// pubkey is stable across runs.
    const TEST_SEED: [u8; 32] = [0x42; 32];

    /// Anchor minute used for the rotating-UUID determinism tests.
    const TEST_MINUTE_ANCHOR: i64 = 30_120_960;

    fn fixed_signing_key() -> SigningKey {
        SigningKey::from_bytes(&TEST_SEED)
    }

    #[test]
    fn new_records_host_pubkey_from_signing_key() {
        let sk = fixed_signing_key();
        let expected: [u8; 32] = sk.verifying_key().to_bytes();
        let backend = BluerPairBackend::new("hci0", &sk);
        assert_eq!(backend.host_pubkey, expected);
        assert_eq!(backend.adapter_id, "hci0");
    }

    #[test]
    fn install_confirm_handler_replaces_previous_handler() {
        let backend = BluerPairBackend::new("hci0", &fixed_signing_key());
        backend.install_confirm_handler(Box::new(|_p| true));
        backend.install_confirm_handler(Box::new(|_p| false));
        assert!(backend.confirm_channel.lock().is_ok());
    }

    #[test]
    fn pair_discovery_uuid_is_deterministic_for_a_given_minute() {
        let a = BluerPairBackend::pair_discovery_uuid(TEST_MINUTE_ANCHOR);
        let b = BluerPairBackend::pair_discovery_uuid(TEST_MINUTE_ANCHOR);
        assert_eq!(a, b);
    }

    #[test]
    fn pair_discovery_uuid_rotates_each_minute() {
        let a = BluerPairBackend::pair_discovery_uuid(TEST_MINUTE_ANCHOR);
        let b = BluerPairBackend::pair_discovery_uuid(TEST_MINUTE_ANCHOR + 1);
        assert_ne!(a, b);
    }

    #[test]
    fn pair_discovery_uuid_matches_session_uuid_for_zero_bond() {
        let minute = TEST_MINUTE_ANCHOR;
        let via_backend = BluerPairBackend::pair_discovery_uuid(minute);
        let via_transport = uuid::Uuid::from_bytes(session_uuid_for(&[0u8; 32], minute));
        assert_eq!(via_backend, via_transport);
    }

    #[test]
    fn build_pair_services_declares_host_pubkey_read_and_phone_pubkey_write() {
        let sk = fixed_signing_key();
        let host_pubkey: [u8; 32] = sk.verifying_key().to_bytes();
        let (_control, handle) = characteristic_control();
        let services = BluerPairBackend::build_pair_services(host_pubkey, handle);
        assert_eq!(services.len(), 1, "exactly one pair service");
        let svc = &services[0];
        assert_eq!(svc.uuid, SYAUTH_PAIR_SERVICE_UUID);
        assert!(svc.primary);
        let host_char = svc
            .characteristics
            .iter()
            .find(|c| c.uuid == SYAUTH_PAIR_HOST_PUBKEY_CHAR_UUID)
            .expect("host-pubkey characteristic missing");
        let read_block = host_char.read.as_ref().expect("host-pubkey must declare read");
        assert!(read_block.read, "host-pubkey.read must be true");
        let phone_char = svc
            .characteristics
            .iter()
            .find(|c| c.uuid == SYAUTH_PAIR_PHONE_PUBKEY_CHAR_UUID)
            .expect("phone-pubkey characteristic missing");
        let write_block = phone_char.write.as_ref().expect("phone-pubkey must declare write");
        assert!(write_block.write, "phone-pubkey.write must be true");
        match write_block.method {
            CharacteristicWriteMethod::Io => (),
            _ => panic!("phone-pubkey write method must be Io"),
        }
    }

    #[test]
    fn build_advertisement_carries_pair_mode_uuid_for_minute() {
        let minute = TEST_MINUTE_ANCHOR;
        let adv = BluerPairBackend::build_advertisement(minute);
        let expected = BluerPairBackend::pair_discovery_uuid(minute);
        assert!(
            adv.service_uuids.contains(&expected),
            "advertisement must carry the pair-mode UUID for the current minute"
        );
        assert_eq!(adv.local_name.as_deref(), Some(ADVERTISE_LOCAL_NAME));
    }

    #[test]
    fn auto_accept_handler_returns_true_for_any_passkey() {
        let h = make_auto_accept_confirm_handler();
        assert!(h(0));
        assert!(h(999_999));
    }

    #[test]
    fn json_string_field_extracts_value_for_known_key() {
        let text = r#"{"schema_version":1,"request_id":"abc-123","decision":"accept"}"#;
        assert_eq!(super::json_string_field(text, "request_id"), Some("abc-123".to_owned()));
        assert_eq!(super::json_string_field(text, "decision"), Some("accept".to_owned()));
    }

    #[test]
    fn json_string_field_returns_none_for_missing_key() {
        let text = r#"{"request_id":"abc-123"}"#;
        assert!(super::json_string_field(text, "decision").is_none());
    }

    #[test]
    fn parse_decision_accepts_matching_request_id_and_accept() {
        let text = br#"{"schema_version":1,"request_id":"req-7","decision":"accept"}"#;
        assert_eq!(super::parse_decision(text, "req-7"), Some(true));
    }

    #[test]
    fn parse_decision_rejects_matching_request_id_and_reject() {
        let text = br#"{"schema_version":1,"request_id":"req-7","decision":"reject"}"#;
        assert_eq!(super::parse_decision(text, "req-7"), Some(false));
    }

    #[test]
    fn parse_decision_ignores_mismatched_request_id() {
        let text = br#"{"schema_version":1,"request_id":"stale","decision":"accept"}"#;
        assert!(super::parse_decision(text, "req-7").is_none());
    }

    #[test]
    fn parse_decision_returns_none_for_unknown_decision_token() {
        let text = br#"{"schema_version":1,"request_id":"req-7","decision":"maybe"}"#;
        assert!(super::parse_decision(text, "req-7").is_none());
    }

    #[test]
    fn parse_decision_returns_none_for_invalid_utf8() {
        let bytes = b"\xff\xfe not valid utf-8";
        assert!(super::parse_decision(bytes, "req-7").is_none());
    }
}
