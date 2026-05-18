# SPEC: unlock-via-phone proximity + handshake path

## 1. Summary

The unlock path between `pam_syauth` and the bonded phone today is built on a
"per-PAM-call advertise burst + opportunistic scan" model. Every `sudo`
opens a fresh BlueZ GATT application, advertises a rotating UUID for ≤1.2 s,
and hopes the phone happens to be scanning at that exact moment. Android's
`CompanionDeviceManager` proximity-observation API runs at battery-saver
duty cycle (minutes), so the phone almost never sees the window. This spec
proposes the architecture that closes the gap: a long-running desktop
`syauth-presenced` user-service that owns the BLE peripheral role for the
lifetime of the operator's session, a long-running phone foreground service
that holds an `autoConnect=true` GATT client to the desktop, and a Unix-socket
control plane between `pam_syauth` and `syauth-presenced` so the PAM module
itself does no radio work. Target audience: anyone who runs `sudo` /
`gdm-password` and expects the phone-as-key prompt to feel like tapping a
FIDO key on the phone, not waiting for a scan to land.

## 2. Background & Research

### Market Context

| Product | Architecture | Latency in practice | Phone battery cost |
|---|---|---|---|
| Apple Watch → Mac auto-unlock | Persistent BLE + Wi-Fi peer-to-peer + (M2+) UWB. Mac is BLE peripheral, watch is BLE central, connection autoConnect-style across sleep/wake. | 1–3 s typical; "ranging timeout" failures common over noisy 5 GHz Wi-Fi. | Watch already running 24/7; incremental cost is negligible (Apple reports < 1 %/day). |
| CCC Digital Key (Tesla, BMW, Apple Wallet) | Car runs multiple BLE peripheral anchors advertising continuously. Phone connects on first proximity and stays connected. UWB (Release 3) measures fine-grained distance for the actual unlock authorisation. | 200 ms BLE round-trip + UWB ranging window; full passive entry < 2 s. | "Tens of milliamps continuous"; phones budget it under Always-On Display power class. |
| Microsoft Hello / Dynamic Lock | Phone is BLE peripheral, Windows scans periodically. Drives only the LOCK (not the unlock) — the threat model is much weaker. | Detection latency 30 s typical (the lock window). | Negligible — phone advertises a generic Bluetooth name. |
| jotson/bluetooth-lock + azratul/ble-lock-session | Polling shell scripts that call `hcitool` / `bt-device`. Lock when scan returns empty for N seconds. | Lock-only, no unlock; 5–30 s detection. | n/a. |
| `key20` (BLE smart lock, GitHub `duerrfk/key20`) | Per-event BLE connection. Phone connects on tap, exchanges HMAC-SHA512-256 with 16-byte nonce, disconnects. | "Sub-second" cryptography per the README; total user-perceived latency = tap + BLE-connect (~500 ms) + HMAC (~50 ms). | Per-event only — no idle drain. |
| `pam_blue` / `pam_csshfp` (FOSS prior art for PAM-over-BT) | Periodic `hcitool` rssi probes from the desktop. | 1–5 s, fail-open on offline. | None on phone (passive). |

Two camps emerge: (a) **persistent-connection products** (Apple Watch, CCC,
BLE smart locks like Tesla's app) where the radio link is up at idle so the
unlock event is just one notify-roundtrip; (b) **per-event-connect products**
(Key20, FOSS PAM hacks) that take a fresh ~500 ms hit on every unlock. SPEC
§4.3's "total wall-clock < 2.0 s" budget is reachable only with camp (a)
because BLE link-layer connection establishment alone burns 200–500 ms even
on a clean radio.

The relay-attack literature ([NCC Group 2022](https://www.nccgroup.com/research-blog/technical-advisory-ble-proximity-authentication-vulnerable-to-relay-attacks/))
is the reason syauth's master SPEC §3.2 D8 already chose
"every unlock requires an explicit user action on the phone" over passive
proximity. That decision rules out 5-minute Keystore validity windows or
silent auto-signs — the user MUST present a biometric per unlock so the
relay's RTT cap (BLE PHY + IFS, < 5 ms) is dominated by the human-tap latency
(~500 ms typical), making timing-detection feasible at the application layer.

### Technical Context

Android Developer guidance ([Communicate in the background, Android 16 docs](https://developer.android.com/develop/connectivity/bluetooth/ble/background))
explicitly names the canonical pattern for a ~1-second BLE notification
latency target:

1. `CompanionDeviceService` keeps the app process alive without a visible
   foreground-service notification — the only API that does so on Android 8+.
2. `CompanionDeviceManager.startObservingDevicePresence()` triggers
   `onDeviceAppeared` callbacks at OS-level scan duty cycle.
3. `BluetoothDevice.connectGatt(context, autoConnect=true, callback, TRANSPORT_LE)`
   instructs the OS to maintain the GATT connection across out-of-range /
   in-range transitions without app intervention.
4. `gatt.setCharacteristicNotification(challengeChar, true)` + CCCD write
   subscribes for push-notifies from the peripheral.

The critical caveat the documentation does not surface: `onDeviceAppeared`
fires **once per transition** from "absent" to "present". If the OS scan
catches the peripheral before our observation is started, the binding is in
"already-present" state and the `onDeviceAppeared` callback never fires.
This is exactly the failure mode tonight's hot-fix attempt hit — the
proximity binding never re-triggered after we hand-installed the observer.

Android 16 introduces a redesigned
`startObservingDevicePresence(ObservingDevicePresenceRequest)` that takes a
`PresenceCondition.BLUETOOTH_LE` filter (matches by MAC) or a
`PresenceCondition.BLUETOOTH_LE_SCAN_FILTER` (matches by service UUID). For
our minSdk-26 target the deprecated `startObservingDevicePresence(int)` (by
association id) and `startObservingDevicePresence(String)` (by MAC) overloads
are still the supported floor.

Android 12+ background-launch restrictions
([dev.to: Beyond the Foreground Service](https://dev.to/ble_advertiser/beyond-the-foreground-service-reliable-background-ble-connection-management-on-android-12-2n78))
forbid starting a foreground service from the background without a
permitted trigger. `CompanionDeviceService` is one of those triggers —
binding it from CDM proximity is the OS-blessed path. The alternative
(start a foreground service from `BOOT_COMPLETED`) works but shows a
permanent notification chip the user cannot dismiss, and Samsung One UI
auto-kills the process after ~6 hours of background uptime even with a
foreground service.

`autoConnect=true` semantics ([Android Developers, connectGatt JavaDoc](https://developer.android.com/reference/android/bluetooth/BluetoothDevice#connectGatt(android.content.Context,%20boolean,%20android.bluetooth.BluetoothGattCallback,%20int))):
> "In case of disconnection initiated by the peripheral or because the
> peripheral is out of range, the GATT client automatically tries to
> reconnect when the peripheral is available."

Reconnection is OS-scheduled, not app-scheduled, so the reconnection rate is
not subject to background-app throttling. This is the load-bearing primitive
for our latency target.

On the desktop side, `bluer 0.17` exposes `Adapter::advertise(Advertisement)`
and `Adapter::serve_gatt_application(Application)`. Both return RAII handles
whose `Drop` impl tears down the advertisement / GATT registration in BlueZ.
The current `BluerAdvertiser::connect_inner` (crates/syauth-transport/src/bluez_advertise.rs:221)
creates these handles inside the per-PAM-call session and drops them at
session end — that is structurally incompatible with a phone-side persistent
connection because the GATT app is gone the moment the PAM call finishes.

### Deep Dives

Two reference points from the BLE proximity-unlock corpus shaped the design:

- **CCC Digital Key 3.0 application note (NXP AN12791)**: the car maintains
  the GATT peripheral role across an entire driving session; the BLE
  connection between phone and car-anchor is "persistent within proximity"
  and reconnects across short out-of-range gaps via the L2CAP-cached bond.
  The desktop in our system is the equivalent of the car anchor.
- **`punchthrough.com` BLE Connection Parameters Guide**: connection
  intervals between 30 ms and 100 ms balance round-trip latency against the
  ~3 mA average phone radio draw. The Android stack's default is 11.25 ms
  in low-latency mode, 51.25 ms in balanced. For our sub-2-s budget the
  balanced default is enough; we don't need to negotiate low-latency PHY
  parameters.

The relay-attack threat model
([NCC Group, Tesla phone-as-key advisory](https://www.nccgroup.com/research/technical-advisory-tesla-ble-phone-as-a-key-passive-entry-vulnerable-to-relay-attacks/))
remains decisive: any architecture that omits the per-unlock user gesture
on the phone is broken at the relay layer. The persistent-connection model
DOES NOT break this — the connection carries an inert notify-channel until
the desktop sends a challenge, and the phone STILL gates the signing on a
fresh `BiometricPrompt` per the master SPEC's threat statement.

## 3. Proposal

### Approach

Split the desktop's BLE role between two processes and make the phone's
BLE role persistent:

- **`syauth-presenced`** — a long-running user-systemd service (one per
  desktop user) that owns the BlueZ adapter for the unlock channel. On
  startup it loads `/var/lib/syauth/bonds.toml`, registers a long-lived
  `bluer::gatt::local::Application` containing the challenge + response
  characteristics for every bonded peer, and starts a rotating
  `Advertisement` whose `service_uuids` set carries the current minute's
  `session_uuid_for(bond_key, minute)`. It rotates the advertisement on
  each wall-clock minute boundary. It listens on a Unix socket
  `${XDG_RUNTIME_DIR}/syauth/auth.sock` for challenge transactions from
  `pam_syauth`.
- **`pam_syauth.so`** — stays the kernel-loaded PAM module, but its
  authenticate path is now a Unix-socket round-trip to the daemon.
  `connect("$XDG_RUNTIME_DIR/syauth/auth.sock")` → write
  `{ kind: "challenge", peer_id, nonce }` → read response within
  `auth_timeout`. Owns no BlueZ handles. Survives daemon restarts because
  the unix socket reconnects per-call.
- **`SyauthCompanionService`** (Android) — becomes a long-running
  foreground service (`foregroundServiceType="connectedDevice"`,
  already in the manifest). Maintains a single `BluetoothGatt` client
  per bonded peer, opened with `autoConnect=true` and `TRANSPORT_LE`.
  Subscribes to the challenge characteristic via CCCD write on every
  fresh service discovery. On `onCharacteristicChanged` the bytes go
  through `verifyChallengeFrame(bondKey, frame)`, then a
  `BiometricPrompt` is shown via a tiny one-shot activity, then the
  Keystore signs (auth-per-use), then the response is written back on
  the response characteristic.

### Key Decisions

| Decision | Choice | Reasoning | Alternatives |
|---|---|---|---|
| Desktop BLE-owner topology | Long-running `syauth-presenced` user service hosts the GATT + advertise; `pam_syauth` talks to it over a Unix socket | The current "per-PAM-call advertise" model is structurally incompatible with the phone's persistent GATT client because BlueZ tears down the `Application` the moment the PAM module returns. A long-lived advertiser is the only architecture that lets the phone keep a `autoConnect=true` connection alive. SPEC §3.2 D8 explicitly anticipated this with "puts the long-lived advertiser on AC power". | (a) Keep advertise inside `pam_sm_authenticate`, accept ~500 ms cold-connect on every unlock → can't hit the SPEC's 2-s budget when the user-tap latency alone is 500–1500 ms. (b) Have `pam_syauth` re-exec itself as a daemon-tail that survives PAM unload → fragile, fights libpam's process model. |
| Phone connection lifecycle | One persistent `BluetoothGatt` per bonded peer, opened with `autoConnect=true` and held by `SyauthCompanionService` as a long-running foreground service | This is the canonical Android pattern for ~1-s notify latency per the Android dev docs and matches every commercial BLE-unlock product (Apple Watch, CCC, Tesla). The radio cost at idle (~1–3 mA) is well below the SPEC §4.3 "< 2 %/day" budget for ~50 unlocks. | (a) Open a new `BluetoothGatt` per unlock attempt → adds 200–500 ms BLE connect overhead per sudo, blows the latency budget. (b) Use `CompanionDeviceManager.startObservingDevicePresence` alone → tonight's audit proved CDM scan rate is OS-controlled and not fast enough; presence callback also fires only on transitions. |
| Rotating UUID cadence | Per-minute (unchanged from current SPEC §3.2 D8 implementation) | The persistent connection makes rotation a non-issue for normal unlocks — the phone is already connected on the L2CAP layer and rotation only affects RE-discovery after a long disconnect. Per-minute keeps the existing privacy story (a passive observer cannot fingerprint a desktop's MAC-to-identity mapping for more than 60 s) without hurting unlock latency. | (a) Per-hour or per-day rotation → reduces re-discovery cost, but a passive observer can fingerprint the desktop and infer user-presence from BLE advertisements; this contradicts SPEC §3.2 D8's stated rationale ("Avoids the phone broadcasting a stable identifier — presence-tracking defense"). (b) Static UUID per bond → same fingerprintability concern as (a). |
| Keystore auth window | Auth-per-use (`setUserAuthenticationParameters(0, AUTH_BIOMETRIC_STRONG)`) | The master SPEC's threat model — "every unlock requires an explicit user action on the phone, by design, because passive BLE proximity has been comprehensively broken by link-layer relay attacks" — forbids time-windowed auth. The relay attack's ~5 ms RTT cap is dominated by the human-tap delay; removing the human gesture makes the relay free. | (a) 5-minute validity window → faster UX but breaks the SPEC's relay-defense story. (b) Device-credential auth (PIN/pattern) → weaker than biometric, doesn't simplify UX. |
| PAM ↔ daemon transport | Unix socket at `${XDG_RUNTIME_DIR}/syauth/auth.sock` with a small length-prefixed framed RPC | Local-only IPC, no DBus dependency, ACL via filesystem perms. Easy to mock in tests; the daemon is a single Rust binary. | (a) DBus (system bus) → adds a configuration burden (policy files), forces the daemon to be system-level. (b) Domain socket on TCP localhost → opens an attack surface to non-root local processes. (c) Reuse BlueZ's existing Agent DBus path → the agent-registration API is for pairing, not for in-band challenge transport. |
| Daemon process model | One `syauth-presenced` instance per user session, started via `~/.config/systemd/user/syauth-presenced.service` | Matches the SPEC's "desktop is the long-lived advertiser on AC power" framing and gives every operator their own isolated daemon. Avoids the system-wide trust amplification of a `--system` instance. | (a) System-level daemon → too much trust, requires polkit policy, complicates multi-user. (b) Started lazily by `pam_syauth` via `systemd-run --scope` → cold-start latency on every unlock, defeats the point. |
| Phone fallback when service is killed | Watchdog: `WorkManager` periodic job (15-min interval) re-launches the foreground service if it isn't running and a bond exists | Samsung One UI / Pixel Doze can kill foreground services after long inactivity; the watchdog ensures the service is reborn before the next unlock. | (a) Trust the OS to keep the service alive → unreliable on Samsung One UI per ble_advertiser.app's 2026 article on background BLE robustness. (b) Use `AlarmManager` with `setExactAndAllowWhileIdle` → battery cost is comparable, but the API surface is heavier. |

### Scope

This is the complete change set for the unlock-via-phone path. None of the
items below may be split out to "next session" — they are mutually load-bearing.

**Desktop**

1. `syauth-presenced` binary in a new crate `syauth-presenced` under
   `crates/syauth-presenced/`. Owns the BlueZ adapter for the unlock
   channel. Single-instance per user (locks `${XDG_RUNTIME_DIR}/syauth/presenced.pid`).
2. Long-lived `bluer::gatt::local::Application` containing one service per
   bonded peer; service UUID rotates on each wall-clock minute boundary.
3. Long-lived `bluer::adv::Advertisement` with `service_uuids` carrying the
   current minute's session UUID(s). Rotated by a tokio timer.
4. Multi-peer support: if `bonds.toml` has N bonded peers, the daemon
   advertises N distinct rotating UUIDs (each derived from the corresponding
   bond_key) so multiple phones can be in range simultaneously.
5. Unix-socket RPC server at `${XDG_RUNTIME_DIR}/syauth/auth.sock`.
   Wire format: 4-byte big-endian length prefix + CBOR-encoded request /
   response (matches the existing UniFFI frame style).
6. Challenge transaction flow:
   `pam_syauth → daemon: ChallengeRequest { peer_id }`,
   `daemon → phone: NOTIFY(challenge_frame)` on the per-peer challenge
   characteristic, `phone → daemon: WRITE(response_frame)`,
   `daemon → pam_syauth: ChallengeResponse { signature }`.
7. Backpressure: at most one in-flight challenge per peer; the daemon
   queues subsequent challenges with a 1 s queue deadline.
8. Audit: every challenge transaction writes one structured line to
   `/var/lib/syauth/last.log` with `peer_id, nonce_hex, outcome, elapsed_ms`.
9. systemd user unit: `crates/syauth-presenced/dist/syauth-presenced.service`,
   installed by `syauth install-presenced` (new subcommand) with
   `WantedBy=default.target`.
10. The `pair` flow plus the new `presenced` flow share `/var/lib/syauth/bonds.toml`
    and `/var/lib/syauth/keys/<peer_id>.bin`. Adding a bond MUST signal the
    running `syauth-presenced` (via `SIGHUP` or socket `Reload` command) so
    a fresh bond becomes advertisable without a daemon restart.

**`pam_syauth`**

11. `pam_sm_authenticate` no longer drives BlueZ directly. It opens
    `${XDG_RUNTIME_DIR}/syauth/auth.sock`, issues `ChallengeRequest`, awaits
    a typed response with timeout = `auth_timeout`. On no-daemon /
    connect-refused / response-timeout it returns `PAM_AUTHINFO_UNAVAIL`,
    preserving the SPEC §3.2 D7 fall-through to `pam_unix`.
12. The PAM module gains a `--socket` argument (default
    `${XDG_RUNTIME_DIR}/syauth/auth.sock`) so test harnesses can inject a
    mock daemon.
13. The PAM module's existing `BondStore::load` path is gone — the daemon
    owns the bond state; the PAM module is a thin RPC client.

**Phone**

14. `SyauthCompanionService` is converted to a long-running
    `Service` (not `CompanionDeviceService`) of type
    `foregroundServiceType="connectedDevice"`. Started by `MainActivity` on
    first launch if a bond exists; restarted by a `BOOT_COMPLETED` receiver
    and by a `WorkManager` 15-min watchdog.
15. The service constructs ONE `BluetoothGatt` per bonded peer with
    `autoConnect=true, transport=TRANSPORT_LE`. The OS reconnects across
    out-of-range gaps without app intervention.
16. On `onServicesDiscovered`, the service subscribes to the challenge
    characteristic via `setCharacteristicNotification(true)` + CCCD write.
17. On `onCharacteristicChanged(challenge)`, the service launches a
    transparent one-shot activity that shows a `BiometricPrompt` bound to
    the Keystore key (auth-per-use). The activity is dismissable; cancel
    sends a "denied" frame back on the response characteristic so the
    desktop fails fast.
18. After a successful sign, the service writes the response frame back
    on the response characteristic (same connection — no reconnect).
19. `AndroidCdmPairCompanionScanner.startObservingDevicePresence` is
    KEPT as a belt-and-suspenders signal for the foreground service's
    watchdog (re-launches if killed and proximity event fires), but the
    primary connection path is the `autoConnect=true` GATT client. The
    "tonight's hot-fix" CDM-only path is removed.

**Crypto / state**

20. Keystore key parameters: `setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG)`.
    Per-sign biometric prompt. This is the SPEC §3.2 D6 contract and the
    relay-attack defense.
21. `BondRecord.phonePubkey` populated by the pair flow (already wired in
    DEV-002 closure) is the verifier on the desktop. `bonds.toml` retains
    `bond_key_hex` for HKDF-derived rotating UUID; `keys/<peer_id>.bin`
    holds the 32-byte bond_key for `pam_syauth`'s MAC check (already
    wired tonight by the `pair.rs` patch).

**Observability**

22. Desktop daemon emits one syslog line per advertise rotation
    (`syauth-presenced: rotated id=<peer> minute=<N>`) and one per challenge
    transaction (`syauth-presenced: tx peer=<id> elapsed=<ms> outcome=<ok|fail>`).
23. Phone `SyauthCompanionService` writes a notification per challenge
    (suppressed if the operator dismisses; rate-limited to 1 per 5 s).
24. `syauth status` (existing subcommand) is extended to report:
    daemon liveness, count of bonded peers being advertised, time since
    last challenge, time since last connect by each peer.

**Anti-goals**

- **Phone advertising any UUID, ever.** Explicitly rejected by SPEC §3.2 D8
  (privacy: phone advertisement leaks a long-lived identifier the user
  carries everywhere). Architectural mismatch.
- **UWB ranging for distance attestation.** Hardware availability is
  fragmented (Pixel 6 Pro+, S25 Ultra+, iPhone 11+) and adds a
  cross-platform dependency we don't otherwise have. The relay-attack
  defense is the per-unlock biometric tap, not distance — SPEC's threat
  model is consistent without UWB. Wrong primitive for this layer.
- **Wi-Fi / IP-based rendezvous (mDNS, WebSocket on LAN).** Explicitly
  rejected by SPEC §3.2 D8 ("rendezvous through cloud / unwanted
  dependency"). Adds a network attack surface; the BLE link's bond is
  the trust anchor.
- **Time-windowed Keystore auth (5-min validity).** Breaks the master
  SPEC's threat statement on link-layer relay attacks. Security boundary,
  not scope reduction.
- **Background `BluetoothLeScanner` foreground scan.** Battery cost is
  punitive (30–80 mA continuous per ble_advertiser.app); the
  `autoConnect=true` reconnect path covers the same use case without the
  duty-cycle hit. Wrong primitive.

## 4. Technical Design

### Architecture

```
                                  Linux desktop
   ┌────────────────────────────────────────────────────────────────────┐
   │                                                                    │
   │  sudo → libpam → pam_syauth.so                                     │
   │           │                                                        │
   │           │  AF_UNIX, CBOR-framed                                  │
   │           ▼                                                        │
   │   ${XDG_RUNTIME_DIR}/syauth/auth.sock                              │
   │           │                                                        │
   │           ▼                                                        │
   │  syauth-presenced  ─── reads /var/lib/syauth/bonds.toml,           │
   │           │              /var/lib/syauth/keys/*.bin (0600)         │
   │           │                                                        │
   │   bluer::gatt::local::Application  ───  bluer::adv::Advertisement  │
   │           │                                          │             │
   └───────────┼──────────────────────────────────────────┼─────────────┘
               │                                          │
               │  GATT (notify + write on encrypted ACL)  │  rotating
               │                                          │  service UUID
               ▼                                          ▼
                                  Android phone
   ┌────────────────────────────────────────────────────────────────────┐
   │                                                                    │
   │   SyauthCompanionService (foreground service, connectedDevice)     │
   │           │                                                        │
   │           ▼                                                        │
   │   BluetoothGatt client (autoConnect=true, TRANSPORT_LE)            │
   │   subscribed via CCCD → challenge characteristic notify            │
   │           │                                                        │
   │           ▼                                                        │
   │   onCharacteristicChanged → launch ChallengeApprovalActivity       │
   │           │   (transparent, full-screen-over-keyguard)             │
   │           ▼                                                        │
   │   BiometricPrompt(AUTH_BIOMETRIC_STRONG, per-use)                  │
   │           │                                                        │
   │           ▼                                                        │
   │   Keystore Ed25519 sign(challenge_frame)                           │
   │           │                                                        │
   │           ▼                                                        │
   │   GATT write back on response characteristic (same connection)     │
   └────────────────────────────────────────────────────────────────────┘
```

Modules affected:

- New: `crates/syauth-presenced/` (binary + lib).
- New: `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/ChallengeApprovalActivity.kt`.
- New: `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/PersistentGattClient.kt`
  (replaces the partial `DirectGattController` written tonight).
- Modified: `crates/syauth-pam/src/auth.rs` (talks to Unix socket, not BlueZ).
- Modified: `crates/syauth-transport/src/bluez_advertise.rs` (the
  `BluerAdvertiser` becomes a library used by `syauth-presenced`, not by
  `pam_syauth` directly; the per-PAM-call short-advertise mode goes away).
- Modified: `syauth-android/app/src/main/AndroidManifest.xml` (boot-completed
  receiver, foreground-service notification channel).
- Modified: `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthCompanionService.kt`
  (lifecycle changes; loses `CompanionDeviceService` parent class, gains
  `Service` parent with `startForeground`).
- Modified: `crates/syauth-cli/src/lib.rs` (new `install-presenced`
  subcommand mirroring `install-pam`).

Data flow per unlock:

1. `pam_syauth` connects to `auth.sock`. (~1 ms, local IPC.)
2. `pam_syauth` writes `ChallengeRequest { peer_id }`. Daemon picks a
   fresh 16-byte nonce, builds a `challenge_frame`, and notifies on the
   challenge characteristic of the phone's open GATT connection. (~50 ms
   for the notify round-trip on a healthy BLE link.)
3. Phone's `onCharacteristicChanged` fires. `SyauthCompanionService`
   launches `ChallengeApprovalActivity`. (~100–300 ms — system dialog
   render.)
4. User sees the BiometricPrompt and authenticates. (~200–1500 ms —
   dominated by user reaction time.)
5. Keystore signs. Activity dismisses. Service writes the response
   on the response characteristic. (~100 ms.)
6. Daemon reads the response, verifies the signature against the bond's
   `phone_pubkey`, replies `ChallengeResponse { ok=true }` on the Unix
   socket. (~10 ms.)
7. `pam_syauth` returns `PAM_SUCCESS`. (~1 ms.)

Total: ~500–2000 ms typical, dominated by step 4 (user tap latency).
SPEC §4.3 target (< 2 s) met for fast users; slow users land near the
2 s cap, which the SPEC explicitly tolerates.

### Non-Functional Requirements

- **Performance**:
  - Daemon idle CPU: < 0.5 % on a modern x86_64 (BlueZ event-loop only).
  - Daemon RSS: < 10 MiB.
  - Unlock latency p50: < 1.5 s. p99: < 2.0 s.
  - Offline-detect latency (daemon socket up, phone unreachable): ≤ 1.2 s
    per SPEC §4.3.
  - Daemon-down latency: ≤ 50 ms (Unix socket connect fails fast).
- **Reliability**:
  - Daemon restarts on `SIGHUP` reread `bonds.toml`; no client-visible
    interruption to phones already connected.
  - Daemon survives BlueZ adapter resets (reconnects on `org.bluez.Adapter1`
    PropertiesChanged).
  - Phone foreground service self-resurrects on `BOOT_COMPLETED` and via
    `WorkManager` watchdog.
- **Security**:
  - Unix socket: ACL via `0600` mode and `${XDG_RUNTIME_DIR}` (per-user
    tmpfs) — only the daemon's own UID can connect.
  - GATT link encryption (DEV-004): `encrypt_authenticated_read/write`
    flags stay on the challenge/response characteristics — relay attacks
    over un-bonded links rejected at the BlueZ layer.
  - Per-challenge nonce (16 bytes from `OsRng`) prevents response replay;
    daemon checks the nonce in the response matches the issued nonce.
  - Audit log: peer_id + nonce + outcome + elapsed per transaction.
- **Observability**:
  - syslog tag `syauth-presenced` for the daemon.
  - syslog tag `pam_syauth` for the PAM module (unchanged).
  - Android logcat tag `syauth.bg` for the phone-side service.
  - `syauth status` extended to report daemon liveness, advertised
    peer count, time since last challenge, time since last per-peer
    connect.

### Testing Strategy

**Unit (Rust)**

- `crates/syauth-presenced/src/rpc.rs::tests::challenge_request_roundtrips`:
  CBOR frame encode/decode for both directions.
- `crates/syauth-presenced/src/orchestrator.rs::tests::issue_challenge_drives_notify_then_awaits_response`:
  fake BLE transport, asserts the daemon notifies, waits ≤ `response_budget`,
  returns Ok on a valid response, returns Err on a timeout.
- `crates/syauth-pam/src/auth.rs::tests::authenticate_falls_through_when_daemon_socket_missing`:
  asserts `PAM_AUTHINFO_UNAVAIL` is returned within ≤ 50 ms when the socket
  is absent.
- `crates/syauth-pam/src/auth.rs::tests::authenticate_returns_success_on_daemon_ok`:
  drives a unit-test daemon that replies `ChallengeResponse { ok=true }`;
  asserts `PAM_SUCCESS`.

**Unit (Android, Robolectric)**

- `PersistentGattClientTest::auto_connect_true_passed_to_connectGatt`.
- `PersistentGattClientTest::on_services_discovered_subscribes_to_challenge_char`.
- `PersistentGattClientTest::on_challenge_launches_approval_activity`.
- `ChallengeApprovalActivityTest::biometric_success_writes_response_on_gatt`.
- `ChallengeApprovalActivityTest::cancel_writes_denied_frame`.
- `SyauthCompanionServiceTest::boot_completed_restarts_service_when_bond_exists`.
- `SyauthCompanionServiceTest::workmanager_watchdog_resurrects_killed_service`.

**Integration (Rust)**

- `crates/syauth-presenced/tests/socket_smoke.rs`: spawn the daemon binary
  with a `tempdir` socket + a fake BLE transport, drive a challenge
  transaction end-to-end, assert wall-clock < 100 ms (excluding human
  latency).
- `crates/syauth-pam/tests/pam_daemon_integration.rs`: same shape, exercise
  the real PAM module against the real daemon binary.

**E2E**

- `scripts/e2e-unlock.sh`: requires the connected R5CY214FQHM phone +
  a paired desktop. Drives `pamtester syauth-test dmitriy authenticate`
  100 times in a row, captures the elapsed-ms distribution from the audit
  log, fails if p99 > 2.0 s OR p50 > 1.5 s. `#[ignore]`-gated under
  `SYAUTH_REAL_RADIOS=1` per the DEV-004 pattern.

### Migration & Compatibility

This is a breaking architecture change:

- `pam_syauth` no longer functions without `syauth-presenced` running.
  `syauth install-pam` MUST also install + enable `syauth-presenced`
  (new `--with-presenced` flag, default `true`).
- `SyauthCompanionService` changes parent class
  (`CompanionDeviceService` → plain `Service` with `startForeground`).
  Existing pairs continue to work because the bond record format is
  unchanged.
- The phone-side foreground service notification is visible by default.
  An operator-tweakable channel-importance setting in the Home route
  silences it after first ack.

No on-disk schema changes — `bonds.toml`, `keys/<peer_id>.bin`, and the
phone's `syauth-bond.toml` keep their current shape.

### Dependencies

- `tokio` (already in workspace) — async runtime for the daemon.
- `serde_cbor` or `ciborium` — CBOR framing for the Unix-socket RPC.
  Decision: `ciborium` (already in workspace via `bluer`'s transitive
  deps; smaller surface than `serde_cbor`).
- `nix` (already transitively present) — Unix socket `SO_PEERCRED` ACL
  enforcement.
- AndroidX WorkManager — already declared but unused; this spec activates it.

No new third-party crates beyond what the workspace already pulls in.

## 5. User Journey

### Persona

**Dmitriy**, a software engineer who installed syauth tonight, paired their
Galaxy S25 Ultra with their Fedora 43 laptop, and now wants to feel the
"phone-as-key for sudo" experience. Their FIDO key remains as the fallback;
they want syauth to be the preferred path because they don't always have
the FIDO key plugged in.

### CJM Phases

| Phase | User action | Pain points | Success signal |
|---|---|---|---|
| 1. Install | `sy syauth install-pam --service sudo` | Operator must trust the prompt that says "this installs a system daemon"; if presenced fails to start, sudo silently falls back to FIDO every time without telling the user why | `sudo whoami` succeeds via phone within the same session |
| 2. First sudo | Types `sudo something` in a terminal | Phone screen is off; will the BiometricPrompt wake the screen? On Samsung One UI, lock-screen full-screen activities require `USE_FULL_SCREEN_INTENT` (granted at install) | Phone screen wakes, BiometricPrompt appears, user taps fingerprint, terminal proceeds |
| 3. Subsequent sudos | Same terminal, more sudos | Repeated BiometricPrompts may feel intrusive; per-use auth is intentional but should be quick (~500 ms biometric verify) | Each sudo = one tap; total wall-clock < 2 s |
| 4. Phone left on desk, walked away | sudo from desktop while user is at the printer | Daemon's challenge times out at 1.2 s; PAM falls through to FIDO; if no FIDO either, falls through to password prompt | Terminal prints "syauth: peer offline, falling back to FIDO" within 1.2 s, then FIDO prompt or password |
| 5. Daemon down | `systemctl --user stop syauth-presenced` | sudo via phone is unavailable; the PAM module returns AUTHINFO_UNAVAIL immediately so FIDO takes over | sudo still works via FIDO; `sy syauth doctor` flags "syauth-presenced not running" |
| 6. Phone battery dies / phone left at home | sudo with no phone | Same as phase 4 — fall through to FIDO / password | No hang, falls through within 1.2 s |
| 7. Revoke | Operator taps "Revoke" in the Home route on phone | Bond is removed phone-side; desktop still has the bond record until `syauth revoke --id <peer_id>` runs | Both sides converge; on the next sudo, no phone prompt fires, sudo uses FIDO |

### Friction Map

| Friction | Phase | Opportunity |
|---|---|---|
| Operator doesn't know syauth fast-path is working vs. falling back to FIDO | 2, 3 | Add a `sy syauth doctor` greppable status line + waybar pill that shows "syauth: ready (peer=fedora)" — visible at a glance |
| First sudo after a long phone-out-of-range gap may take longer (BLE reconnect) | 2 | `autoConnect=true` reconnect handled by OS; surface "reconnecting…" in waybar so the operator knows a slower first-tap is expected |
| Phone shows a permanent foreground-service notification | 1+ | Use a low-priority notification channel ("syauth phone-as-key active") that collapses into a small icon; operator can mute the channel after first ack |
| BiometricPrompt over keyguard requires `USE_FULL_SCREEN_INTENT` | 2 | Manifest declares it; the first install of the app prompts for the grant once; doctor reports if missing |
| Audit-log discovery for failures | 4, 6 | `sy syauth doctor` aggregates last 10 lines of the daemon's syslog + matches them to `/var/lib/syauth/last.log` so a single command tells the operator what went wrong |
| Operator wants to disable syauth temporarily without uninstalling | any | `systemctl --user stop syauth-presenced` is the documented kill switch — surface it in `sy syauth status --how-to` |

### North Star

`sudo something` → BiometricPrompt fades in on the phone within 500 ms →
operator taps fingerprint → sudo prompt returns within 1.5 s total. No
FIDO key needed. When the phone is not in range or off, `sudo` falls through
to FIDO within 1.2 s; the operator never waits longer than the SPEC's 2 s
ceiling for any outcome.

## 6. Durability & Failure Handling

### State Model

The unlock workflow has these durable transitions:

```
   Idle (daemon up, phone connected, no challenge in flight)
       │   pam_syauth opens socket
       ▼
   ChallengeIssued { nonce, t_start }
       │   daemon notifies, awaits write
       ├──[response within budget]──▶ ChallengeVerified { ok | denied }
       ├──[response_timeout]────────▶ TimedOut → AuthInfoUnavail
       └──[transport_err]───────────▶ TransportFailed → AuthErr (transport-error)
   ChallengeVerified
       │
       ▼
   Idle
```

- **Persistence point**: every transition writes a single audit line to
  `/var/lib/syauth/last.log` (`peer_id, nonce_hex, t_start, t_end, outcome, reason`).
  This is the only durable state — the daemon itself is stateless apart
  from the in-memory advertise + GATT handles.
- **Recovery point**: daemon crash → systemd restarts within < 1 s →
  BlueZ retains the bonded ACL connection for ~6 s (L2CAP idle timeout),
  so the phone's `autoConnect=true` reconnects without a full re-pair.
- **Idempotency**: every nonce is single-use. A replayed response (same
  nonce) is rejected by the daemon's in-memory nonce cache (LRU of last
  64 nonces per peer).

### Failure Taxonomy

| Failure | Class | Retry | PAM return |
|---|---|---|---|
| Socket connect refused (daemon down) | Permanent (this call) | No | `PAM_AUTHINFO_UNAVAIL` |
| Socket write fails mid-frame | Transient | No (the PAM call is the unit) | `PAM_AUTHINFO_UNAVAIL` |
| Daemon: no advertiser for peer_id | Permanent | No | `PAM_AUTHINFO_UNAVAIL` |
| Daemon: no GATT connection to peer (autoConnect still pending) | Transient | No (the user is waiting; just fail and fall through) | `PAM_AUTHINFO_UNAVAIL` (reason=`offline`) |
| Phone receives notify but user denies | Permanent | No | `PAM_AUTH_ERR` (reason=`denied`) |
| Phone receives notify but biometric fails 3 times | Transient | No (user chose to fail) | `PAM_AUTH_ERR` (reason=`biometric-fail`) |
| Response signature invalid | Permanent | No | `PAM_AUTH_ERR` (reason=`bad-signature`) |
| Response nonce mismatch | Permanent (likely attack) | No | `PAM_AUTH_ERR` (reason=`replay`) |
| Response times out | Transient | No (already at user-attention budget) | `PAM_AUTHINFO_UNAVAIL` (reason=`response-timeout`) |
| BlueZ adapter goes down mid-call | Transient | No | `PAM_AUTHINFO_UNAVAIL` (reason=`adapter-missing`) |

No retries inside one PAM call — the SPEC's 2 s ceiling makes retries
counter-productive. The PAM module's only "retry" is the operator's next
`sudo`.

### Rehydration

Daemon cold-start sequence:

1. Open `${XDG_RUNTIME_DIR}/syauth/auth.sock` (file lock + bind).
2. Open the BlueZ adapter (`hci0` default).
3. Load `/var/lib/syauth/bonds.toml`. For each non-revoked bond, load the
   corresponding `keys/<peer_id>.bin`.
4. Register the GATT `Application` with N services (one per peer).
5. Start advertising the N rotating UUIDs in a single
   `Advertisement` if BlueZ allows (the bluer 0.17 API takes a `HashSet<Uuid>`,
   so yes).
6. Start the wall-clock-minute rotation tokio timer.
7. Become socket-ready.

No persisted "in-flight challenges" — by design, a challenge in flight
when the daemon dies is lost and the operator's `sudo` returns
`PAM_AUTHINFO_UNAVAIL`. They retry. This is the right behavior for a
sub-2-s, human-attention-bound workflow; durable resumption would add
machinery for a vanishingly rare failure.

## 7. Security

### Threat Model

- **T-Relay** (NCC 2022): two custom radios relay encrypted PDUs at the
  link layer. Defense: per-unlock biometric tap on the phone makes the
  human-tap latency (~500 ms) dwarf the relay's ~5 ms RTT cap; the
  operator notices a relayed unlock because they didn't tap. Plus DEV-004
  link-layer encryption rejects non-bonded relays at the BlueZ ATT layer.
- **T-Presence-Tracking**: passive observer logs the desktop's BLE
  advertisement to fingerprint user-presence. Defense: per-minute UUID
  rotation derived from `session_uuid_for(bond_key, minute)`, observable
  only by the bonded peer. Trade-off: rotation cadence vs.
  re-discovery latency — per-minute keeps the privacy story without
  hurting the persistent-connection latency (the bond's L2CAP cache
  survives across rotations).
- **T-Local-Privilege-Escalation**: a non-root local process tries to
  drive `pam_syauth` by impersonating the daemon's socket. Defense:
  the socket lives in `${XDG_RUNTIME_DIR}` (per-user 0700 tmpfs) and
  is bound with `0600` mode. The daemon enforces `SO_PEERCRED` matches
  its own UID on every accept.
- **T-Daemon-DoS**: attacker connects to the socket repeatedly to
  starve `pam_syauth` of socket slots. Defense: the daemon caps
  concurrent socket accepts at 4 and rate-limits new connections to 10/s
  per peer-credential UID.

### Trust Boundaries

- `pam_syauth` ↔ `syauth-presenced`: filesystem-ACL + `SO_PEERCRED`.
- `syauth-presenced` ↔ BlueZ: DBus authenticated bluer client.
- `syauth-presenced` ↔ phone: BLE link-layer encryption + ATT-level
  encrypt-authenticated-read/write flags + LESC bond.
- Phone Keystore ↔ Keystore HAL: AUTH_BIOMETRIC_STRONG + AUTH_PER_USE.

### Data Classification

- `bond_key` (32 bytes): sensitive. Lives at `/var/lib/syauth/keys/<peer_id>.bin`
  (0600 root-owned). Daemon reads on startup, never writes.
- `phone_pubkey` (32 bytes): public. Lives in `bonds.toml` (0600 today;
  could be 0644).
- Per-call `nonce` (16 bytes): transient. In daemon memory only.
- Phone-side Ed25519 private key: never leaves the Keystore enclave;
  AUTH_PER_USE + AUTH_BIOMETRIC_STRONG.

### Audit

- `/var/lib/syauth/last.log` (append-only): one line per challenge tx.
- syslog `syauth-presenced` tag: daemon lifecycle (start, stop,
  bond load, BlueZ adapter events).
- Android logcat `syauth.bg`: phone-side service lifecycle.

## 8. Risks & Mitigation

| Risk | Impact | Likelihood | Mitigation |
|---|---|---|---|
| Samsung One UI kills the foreground service after long idle | unlock fails after a few hours of laptop disuse | High (One UI is known for aggressive process death) | WorkManager 15-min watchdog + `BOOT_COMPLETED` receiver re-launch the service |
| `BluetoothGatt.connectGatt(autoConnect=true)` fails to reconnect on some OEM stacks | unlock fails after first OOR gap | Medium (autoConnect quirks are documented on Pixel pre-Android 13) | Watchdog also re-creates the GATT handle every 30 min as a defensive measure |
| BlueZ `Application` registration conflicts with other GATT users (e.g., audio profile) | daemon fails to start | Low (we register our own UUIDs; no namespace overlap) | Daemon retries adapter open with exponential backoff; surfaces failure via syslog + `sy syauth doctor` |
| `pam_syauth` Unix socket path differs across operators (XDG_RUNTIME_DIR not set, e.g., from a serial console) | unlock fails on text-mode boot | Medium | Fall back to `/run/user/$UID/syauth/auth.sock` if XDG_RUNTIME_DIR is unset; doctor flags the case |
| Daemon writes audit log faster than disk flushes; on power loss, the last few transactions are gone | minimal — audit is for forensics, not for control flow | Low | Accept; the daemon `O_APPEND`s and fsync()s every 32 transactions |
| Phone re-pair changes the bond_key; daemon caches stale key in memory | unlock returns `bad-signature` until daemon SIGHUP | Medium | `pair` flow SIGHUPs the daemon via PID file on bond write; daemon also watches `bonds.toml` via inotify |
| Multi-peer rotation: phone is connected, walks away, comes back during a different minute slot | first unlock after return takes longer (re-discovery) | High (normal case) | Phone listens on N + N-1 + N-2 slot UUIDs for skew tolerance; same window the pair flow uses |
| Operator runs sudo from an SSH session (different XDG_RUNTIME_DIR than the daemon) | unlock fails because socket path mismatch | Medium | PAM module's `--socket` argument lets ops point at the right path; doctor explains the SSH-case to operators |

## 9. Open Questions

1. Should `syauth install-presenced` enable the service per-user (operator
   has to run it once per account) or system-wide via a templated unit
   `syauth-presenced@$USER.service`? Recommend per-user for simplicity;
   revisit if multi-user desktops become a real use case.
2. The biometric prompt opens a transparent activity over the keyguard.
   On Pixel 7+, the prompt's text field allows free-form "reason" copy.
   What should that copy say to differentiate a real desktop sudo from
   a phishing attempt impersonating one? Recommend
   `"$hostname is requesting sudo (peer_id $short)"` with hostname pulled
   from the bond record, not from the incoming frame.
3. Are there desktops where `bluer` cannot keep an `Advertisement` open
   indefinitely without periodic re-registration? Initial belief: no
   (bluer 0.17 documents the `AdvertisementHandle` as a long-lived
   RAII guard). Worth a stress-test before declaring final.
4. Should the daemon offer a JSON variant of its RPC for tooling
   integration (e.g., `sy syauth doctor` querying state without speaking
   CBOR)? Recommend yes — add a `--format json` flag to a future
   `syauth-presenced-ctl` admin tool. Not in this spec's scope but
   noted for follow-on.
5. The current `pam_syauth` returns `PAM_AUTH_ERR` for `wrong-version`
   and `replay`. Should the daemon-mediated path do the same, or
   return `PAM_AUTHINFO_UNAVAIL` to fall through more gracefully?
   Recommend: keep `PAM_AUTH_ERR` for replay/wrong-version (these are
   attack-shaped); use `PAM_AUTHINFO_UNAVAIL` for `offline` /
   `response-timeout` / `daemon-down`.

## 10. Implementation Roadmap

A roadmap in `.agents/skills/roadmap/SKILL.md` shape will be authored
separately; this spec terminates at the design. The roadmap will
sequence (in dependency order):

- The `crates/syauth-presenced/` daemon binary + its CBOR-framed Unix
  socket + the long-lived BLE peripheral.
- The `pam_syauth` rewrite onto the daemon socket.
- The phone-side `PersistentGattClient` + `ChallengeApprovalActivity` +
  the `Service`-class lifecycle change.
- The `sy syauth doctor` + `syauth-presenced.service` install path.
- The e2e benchmark `scripts/e2e-unlock.sh` and the SPEC §4.3
  latency-target gate.

Each step is independently shippable, gated by `make scope-discipline`,
`make lint`, `make test`, and (for the Android side) `:app:assembleDebug`
+ `:app:testDebugUnitTest`. The roadmap will pin closure conditions for
each step against the SPEC's "p50 < 1.5 s, p99 < 2.0 s" latency targets
and "phone battery < 2 %/day at 50 unlocks" power target.
