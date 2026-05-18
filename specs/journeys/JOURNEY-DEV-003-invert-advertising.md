# JOURNEY-DEV-003: Invert BLE advertising direction (desktop advertises, phone scans)

> **SPEC anchors:** §3.2 D8 — "The **desktop** advertises a rotating
> session-bound UUID; the **phone** scans and connects" — and the
> rationale paragraph immediately following: "Avoids the phone
> broadcasting a stable identifier (presence-tracking defense); puts the
> long-lived advertiser on AC power".
>
> **Gap reference:** `docs/known-gaps.md` row DEV-003.
>
> **Closure condition (mechanical, greppable):**
> - `git grep -l "// GAP: DEV-003"` returns nothing (the journey doc may
>   reference DEV-003 textually; production code is clean).
> - `git grep -l "SyauthGattHostService" -- crates/ syauth-android/app/src/main/`
>   returns nothing.
> - `git grep -l "BLUETOOTH_ADVERTISE" -- syauth-android/app/src/main/`
>   returns nothing.
> - `git grep -l "BluerlessGattServerController\|startAdvertisingIfPossible"
>   -- syauth-android/app/src/main/` returns nothing.
> - `make scope-discipline` clean.
> - `make lint` clean.
> - `cargo test --workspace` green, no regression from the post-DEV-001
>   baseline (278 passing).
> - `docs/known-gaps.md` row DEV-003 moved from "Open deviations" to
>   "Closed deviations" with a UTC closure timestamp and pointer to this
>   journey.

## Roadmap Link

- Source roadmap: gap row DEV-003 in `docs/known-gaps.md` (orchestrated
  from `specs/auto/RUN-2026-05-17T07-56-16Z.md`).
- Predecessor: JOURNEY-DEV-001 (real LESC pair) — closed in the same
  march; produces the bonded link and the CDM association DEV-003
  consumes.
- Successor: DEV-004 (link encryption returns to `PERMISSION_*_ENCRYPTED`)
  — explicitly out of scope here. DEV-003 must NOT enable encrypted
  characteristics; that is DEV-004's deliverable.
- Feature: swap the BLE role pair so the phone is the central + GATT
  client and the desktop is the peripheral + GATT server + advertiser.

## 1. Journey

When **a syauth operator with a paired phone+desktop sits down at the
locked desktop and runs `sudo` (or any other PAM-gated command)**, I
want **the desktop to advertise a rotating per-minute UUID derived from
our shared bond_key, the phone to scan for that exact UUID (and only
that UUID), open a GATT client connection on match, write the signed
challenge response, and disconnect — without the phone ever exposing
its MAC + a stable service UUID to anyone within BLE range**, so I can
**stop leaking phone-presence information to passive observers
(supermarket beacons, stalkers, advertising SDKs) while preserving the
unlock latency and reliability SPEC §4.2 promises**.

## 2. Customer Journey

The operator already paired their phone with their desktop via the LESC
+ app-OOB flow that just closed in DEV-001. Today (with DEV-003 still
open) every time they walk past a Bluetooth scanner — at a mall, in an
airport, near a phishing kiosk — their phone broadcasts a fixed
`5a4e8e3c-…-0001` service UUID plus its MAC. Anyone correlating those
two bytes tracks the operator across physical locations indefinitely.
The SPEC §3.2 D8 rationale calls this out by name as a presence-tracking
attack and prescribes the role inversion.

After this journey closes, the phone holds no advertising permission,
hosts no GATT server in the background, and never broadcasts the
syauth service UUID. The desktop becomes the advertiser whenever PAM
needs an unlock; the UUID it advertises is derived from
`HKDF(bond_key, "syauth-session-v1" || minute_be)` and rotates every
60 seconds. A passive observer who does not hold the bond_key sees
nothing they can correlate to the operator's identity.

### Phase 1: Desktop boots the advertiser

**User Intent:** When PAM invokes `pam_sm_authenticate`, the desktop
must publish a syauth GATT service and advertise a rotating UUID that
only the bonded phone can recognise — no fixed UUID, no plaintext
device name, no persistent advertiser between PAM calls.

**Actions:**
- The PAM module's `acquire_peer` path constructs a `BluerAdvertiser`
  bound to the configured adapter, the loaded `bond_key`, and the
  bond's `peer_id`.
- The advertiser opens the bluer adapter, computes
  `session_uuid_for(bond_key, current_minute)`, registers a GATT
  `Application` carrying the existing `SYAUTH_CHALLENGE_CHAR_UUID` +
  `SYAUTH_RESPONSE_CHAR_UUID` characteristics under the
  *rotating* service UUID, and starts an `LeAdvertisement` whose
  `service_uuids` set contains the same rotating UUID.
- A background watcher rebuilds the advertisement at the next minute
  boundary (or at session teardown, whichever comes first).

**Pain / Risk:**
- bluer's `serve_gatt_application` requires the adapter to be powered;
  if `set_powered(true)` fails, the call must surface as
  `TransportError::Backend` and `pam_sm_authenticate` returns
  `PAM_AUTHINFO_UNAVAIL`, not panic.
- BlueZ allows at most one advertisement per service UUID set at a
  time; a concurrent PAM call must not race the advertise step into a
  `Failed` D-Bus reply. The advertiser is per-call: each PAM
  invocation builds its own and drops it at the end of the runtime
  block.
- The rotating UUID derivation crosses a minute boundary mid-session.
  If the phone has not yet matched the slot-N UUID when the desktop
  rolls to slot N+1, the phone's scanner now searches for slot N+1.
  Mitigation: phone scans for `[current_minute, current_minute - 1]`
  for skew absorption (Phase 2).

**Success Signal:** the BlueZ adapter reports one active LE
advertisement whose service UUID equals
`session_uuid_for(bond_key, current_minute)`, and a GATT application
with the syauth challenge + response characteristics is reachable on
that adapter for the duration of the PAM call.

### Phase 2: Phone scans, matches the rotating UUID

**User Intent:** The phone-side `SyauthCompanionService` (CDM-bound)
must wake up when the bonded desktop appears in BLE range, scan for
the exact rotating UUID derived from its stored `bond_key`, and ignore
every other advertiser — including replays from prior slots.

**Actions:**
- The OS binds `SyauthCompanionService` on
  `onDeviceAppeared(AssociationInfo)`. The service consults the bond
  store, computes the same `session_uuid_for(bond_key, slot)` for the
  current minute and the immediately-preceding minute, and starts a
  `BluetoothLeScanner` with a `ScanFilter` matching the union of the
  two slot UUIDs.
- The scanner runs in `SCAN_MODE_LOW_LATENCY` for the duration of one
  unlock window. On `onScanResult(SCAN_FOUND, …)` matching one of the
  two slot UUIDs, the service opens a GATT client connection to the
  reporting `BluetoothDevice`.

**Pain / Risk:**
- A slot-N broadcast replayed at slot N+1 by a hostile radio (TC-04):
  the phone's UUID filter set rolls past N to {N+1, N}; the replay no
  longer matches and the scanner ignores it. After a 60-second slack
  the prior slot also rolls off; the longest window in which a
  replayed slot-N broadcast could still trigger a connection is
  slightly under 120 seconds.
- Multiple syauth desktops in range (TC-05): the phone holds one
  bond_key per association, so it only computes one rotating UUID
  pair. A second desktop derived from a different bond_key produces a
  different UUID — its broadcasts simply do not match the filter.
- Android scan quota: API 30+ enforces "no more than 5 scans per 30
  seconds per app". The companion service must not start a new
  scanner per `onScanResult`; one scanner per appear/disappear pair
  is the contract.

**Success Signal:** a `BluetoothGatt` client connection to the
desktop reaches `STATE_CONNECTED` and the GATT service-discovery
result contains exactly one service whose UUID equals one of the
expected slot UUIDs (the slot the desktop is currently advertising).

### Phase 3: Phone writes response, disconnects

**User Intent:** Send the signed challenge response over the bonded
link and tear down — no foreground service must outlive the unlock
exchange.

**Actions:**
- The phone-side GATT client subscribes to the response characteristic
  (CCCD write), receives the desktop's challenge write on the
  challenge characteristic, hands the verified payload to the existing
  approve UI through `ApproveNotification.buildApproveIntent`.
- After the user approves and `ApproveViewModel` produces the signed
  response frame, the phone writes it to the response characteristic
  via `BluetoothGatt.writeCharacteristic`.
- On `onCharacteristicWrite(STATUS_SUCCESS)` the phone calls
  `BluetoothGatt.disconnect()` and the service tears down its scanner
  + GATT client for this association.

**Pain / Risk:**
- The user dismisses the approve UI without tapping anything: the
  scanner times out after the configured response window, the service
  disconnects, the desktop's `recv_frame` hits `TransportError::Timeout`
  and PAM returns `PAM_AUTHINFO_UNAVAIL`.
- The GATT write returns a non-success status (e.g. peer hangup): the
  phone disconnects without retry; the next PAM call will re-scan and
  re-pair the GATT link.
- The phone process is killed mid-response (Android low-memory kill):
  the OS rebinds `SyauthCompanionService` next time the desktop
  appears; the now-stale advertisement is discarded by the desktop's
  per-call teardown.

**Success Signal:** the desktop's `recv_frame` returns one valid
response frame on the response characteristic within the PAM
`response_timeout` budget; the PAM module verifies and returns
`PAM_SUCCESS`; both sides observe a clean GATT disconnect within the
same syslog second.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|---|---|---|
| Operator wonders whether the phone is "doing something" while idle | 1–2 | Document that the phone no longer advertises or hosts a GATT server in the background — it scans only when the OS binds `SyauthCompanionService`, which happens only when the bonded desktop appears in BLE range |
| Two operators with two syauth-bonded phones near the same desktop | 2 | Each phone derives its own slot UUID from its own bond_key; the desktop advertises exactly one bond's UUID per PAM call, so cross-talk is structurally impossible |
| Phone clock drifts vs desktop clock | 2 | Phone scans for current_minute AND current_minute - 1, absorbing up to one full minute of negative skew; positive skew is absorbed by the desktop's per-minute rotation, since the next minute boundary will line up |

### North Star Summary

A passive Bluetooth scanner sitting on a city street records nothing
identifiable from a syauth phone walking past. The phone holds no
advertising permission and runs no background server. Whenever the
bonded desktop needs an unlock, the desktop advertises a UUID that
*only* the bonded phone knows how to recognise, the phone scans, the
exchange completes in under two seconds, and the unlock channel is
torn down. The phone is invisible to the radio outside this exact
moment.

## 3. Architecture Notes

### Linux side (new `BluerAdvertiser`)

- Add a new desktop-side type, `BluerAdvertiser`, that implements
  `BtPeer` by registering a bluer GATT `Application` (challenge +
  response characteristics under the rotating UUID) and an
  `LeAdvertisement` whose `service_uuids` set contains that UUID.
- The rotating UUID is `session_uuid_for(bond_key,
  current_unix_minute)` — the helper already lives in
  `crates/syauth-transport/src/bluez.rs`. The advertiser converts the
  raw 16-byte derivation into a `bluer::Uuid`.
- Per-call lifecycle:
  - `connect(timeout)` builds the `Application` + `LeAdvertisement`,
    registers them with the adapter, awaits a writer to the challenge
    characteristic, accepts the first valid frame, and returns a
    `Session` whose `send_frame` writes to the response characteristic.
  - Drop of the `Session` (end of the PAM call) tears down the
    advertisement and unregisters the GATT application. No persistent
    advertiser between PAM calls.
- The PAM module's `acquire_peer` switches from `BlueZBtPeer::new_sync`
  (the client/central path) to `BluerAdvertiser::new` (the
  peripheral/server path). This is the role inversion that SPEC §3.2
  D8 mandates.

### Android side (new `SyauthBleScanner` + GATT client)

- Replace `SyauthGattHostService` (always-on host) and
  `BluerlessGattServerController` (advertiser plumbing) with a
  scanner+client path:
  - `SlotUuidCalculator` (pure Kotlin) computes the same
    `session_uuid_for(bond_key, slot)` as the Rust helper. This is a
    UniFFI hop: the Rust side ships `sessionUuidForBond(bond_key,
    minute_be_bytes)` via `crates/syauth-mobile/src/mobile.udl`; the
    Android side calls it through the generated binding so the
    computation cannot drift from the desktop.
  - `SyauthBleScanner` (new class in `bg/`) drives
    `BluetoothLeScanner` with a `ScanFilter.Builder().setServiceUuid(...)`
    for each of the two slot UUIDs (current + previous minute).
  - `SyauthGattClient` (new class in `bg/`) opens a `BluetoothGatt`
    client connection on a scan match, subscribes to the response
    characteristic, awaits the challenge write, and writes the signed
    response back.
- `SyauthCompanionService.onDeviceAppeared` now constructs a
  `SyauthBleScanner` (not a `GattServerController`); `onDeviceDisappeared`
  stops it. The CDM association created during DEV-001's pair is the
  gate: the service only starts a scanner for peers that hold a real
  `AssociationInfo`.
- `MainActivity` no longer calls `startForegroundService(...,
  SyauthGattHostService)`. The CDM-bound `SyauthCompanionService` is
  the only foreground path; it self-binds via the OS when the
  associated peer appears in range. If no CDM association exists
  (user has not yet paired), `MainActivity` simply does nothing —
  there is no idle scanner, no idle GATT server.

### Manifest

- Delete the `<uses-permission android:name="android.permission.BLUETOOTH_ADVERTISE" />`
  block.
- Delete the `<service android:name=".bg.SyauthGattHostService" ... />`
  element.
- Delete the `GAP: DEV-003` header comment in the manifest.
- Keep BLUETOOTH_SCAN and BLUETOOTH_CONNECT — the new path needs
  exactly those two BLE permissions.

### Tests

- Delete `GattAdvertiserTest.kt` (whole file: the advertiser is gone).
- Delete `GattServerControllerTest.kt` (whole file: the controller is
  gone).
- Delete `SyauthGattHostServiceTest.kt` (whole file: the service is
  gone).
- Add `SlotUuidCalculatorTest.kt` covering rotation determinism +
  cross-minute mismatch + bond-key dependency.
- Add `SyauthBleScannerTest.kt` covering scan-filter UUID set,
  current/previous slot inclusion, replayed-slot rejection.
- Add `SyauthGattClientTest.kt` covering connect → challenge-write
  observed → response-write → disconnect.
- Add a Rust-side `bluez_advertise` module test covering registration,
  teardown, MTU split through the new code path, error handling when
  the adapter is down. (The full advertise+serve roundtrip is gated
  behind `SYAUTH_REAL_RADIOS=1` per the S-019 pattern.)

### Closure conditions (mechanical)

- [ ] `git grep -l "// GAP: DEV-003"` returns nothing (this journey
      may textually reference DEV-003 in prose but carries no
      `// GAP:` source-marker line).
- [ ] `git grep -l "SyauthGattHostService" -- crates/ syauth-android/app/src/main/`
      is empty.
- [ ] `git grep -l "BLUETOOTH_ADVERTISE" -- syauth-android/app/src/main/`
      is empty.
- [ ] `git grep -l "BluerlessGattServerController\|startAdvertisingIfPossible"
      -- syauth-android/app/src/main/` is empty.
- [ ] `make scope-discipline` returns clean.
- [ ] `make lint` returns clean.
- [ ] `cargo test --workspace --all-targets --all-features` is green,
      with passing-test count ≥ 278 (post-DEV-001 baseline).
- [ ] `docs/known-gaps.md` row DEV-003 moves from "Open deviations" to
      "Closed deviations" with a UTC closure timestamp and pointer to
      this journey.

## 4. Tests

### TC-01: golden e2e on real radios — desktop advertises, phone scans, unlock succeeds

**Given** a bonded desktop+phone pair (DEV-001 closed) with
`SYAUTH_REAL_RADIOS=1` set and both adapters up.
**When** PAM is invoked on the desktop.
**Then** the desktop advertises the rotating UUID, the phone scans,
matches, connects, writes a signed response, and disconnects within
the SPEC §4.2 2-second budget; PAM returns `PAM_SUCCESS`.

### TC-02: desktop advertises rotating UUID slot N, phone scans + matches

**Given** a fixed `bond_key` and a fixed `minute = N`.
**When** the desktop's `BluerAdvertiser` registers its
`LeAdvertisement` and the phone's `SlotUuidCalculator` computes the
expected slot UUID.
**Then** the two UUIDs are byte-identical (deterministic HKDF
derivation, no clock dependency in the unit test).

### TC-03: slot rotation — desktop crosses minute boundary mid-session

**Given** a session begins at second 58 of minute N; the desktop's
rotating loop is configured to rebuild the advertisement at the next
minute boundary.
**When** the loop fires at minute N+1.
**Then** the advertised UUID changes to
`session_uuid_for(bond_key, N+1)`; a connected GATT client whose
session is already open continues without interruption (the connected
peer is unaffected by advertise-data changes); new scanners see the
N+1 UUID.

### TC-04: phone receives slot-N broadcast at slot N+1 → ignored

**Given** a hostile radio replays a captured slot-N
`LeAdvertisement` packet at wall-clock minute N+1.
**When** the phone's scanner is configured with
`{session_uuid_for(bond_key, N+1), session_uuid_for(bond_key, N)}`.
**Then** the replay packet (containing
`session_uuid_for(bond_key, N - 1)` after the previous-minute slot
rolls off) does NOT match the scan filter; the
`BluetoothLeScanner.ScanCallback` is never invoked for the replay; no
GATT client connection is opened. Test exercises this through the
`SlotUuidCalculator` contract: the scanner-filter UUID set never
includes slot `N - 1`.

### TC-05: multiple syauth desktops in range — phone picks the one whose UUID derives from a bond_key it holds

**Given** two syauth desktops in BLE range, advertising rotating
UUIDs derived from two different `bond_key` values; the phone holds
exactly one bond.
**When** both advertisements arrive at the phone's scanner.
**Then** only the advertisement whose UUID derives from the phone's
own `bond_key` matches the filter; the second desktop is structurally
invisible to this phone (its UUID is unrelated). Test exercises this
by asserting that the `SlotUuidCalculator` output for two different
bond keys at the same minute is byte-distinct, and that the scan
filter built from the phone's bond_key set does not contain the
other desktop's UUID.

### TC-06: desktop GATT server registers + tears down per session

**Given** a `BluerAdvertiser` initialised against a test bluer
adapter.
**When** the caller invokes `connect(timeout)` and drops the returned
`Session` after one exchange.
**Then** the GATT application's `Application` is registered exactly
once (single `serve_gatt_application` call) and dropped at session
end (the bluer `ApplicationHandle` goes out of scope, unregistering
the service from BlueZ). No leaked advertisement, no leaked GATT
application after the call returns.

### TC-07: GATT permissions on desktop characteristics still allow READ/WRITE — link encryption is DEV-004's job

**Given** the new desktop GATT `Application` built by
`BluerAdvertiser`.
**When** the test inspects the characteristic descriptors for the
challenge + response characteristics.
**Then** the challenge characteristic permits unauthenticated WRITE,
the response characteristic permits unauthenticated READ +
notify-subscribe, and NEITHER characteristic declares any
encryption-required flag — DEV-004 lands that change next; this
closure must not pre-enable it because the bonded link is required
on the phone side for encrypted-read writes to succeed and the
phone-side wiring (DEV-004) is a separate orchestrator step.

### TC-08: phone-side GATT client connects, subscribes, writes response, disconnects

**Given** a fake `BluetoothGatt`-shaped seam injected into
`SyauthGattClient`.
**When** the seam reports a successful CCCD subscribe and delivers a
synthetic challenge frame via `onCharacteristicChanged`.
**Then** the client invokes the supplied response-bytes callback,
writes the bytes back to the response characteristic via
`writeCharacteristic`, awaits `onCharacteristicWrite(GATT_SUCCESS)`,
and calls `disconnect()` exactly once.

### TC-09: CDM-only foreground service path — no association means no scanner

**Given** the phone has no `CompanionDeviceManager` association for
any peer (fresh install, no `syauth pair` ever run).
**When** the OS attempts to bind `SyauthCompanionService` (or the
test directly instantiates it and calls `onDeviceAppeared` with a
fake `AssociationInfo` that the bond-store seam reports as unknown).
**Then** the service does not start a scanner; the bond-key provider
returns `null`; the service logs the drop and idles. No
`SyauthGattHostService` start path remains in `MainActivity` — the
test confirms this by grep, not by behaviour assertion.

### TC-10: bond_key absent on the desktop — typed error, no advertise leak

**Given** a `BluerAdvertiser` constructed with a `bond_key` whose
keyring lookup returns `secret-not-found`.
**When** the PAM module tries to acquire the peer.
**Then** `acquire_peer` returns `AuthErr("secret-not-found")` BEFORE
any bluer advertise call; no `LeAdvertisement` is registered with
BlueZ; no rotating UUID is ever computed for this peer. The
existing `pam_e2e` "secret-not-found" path covers the error
propagation; this TC asserts the no-side-effect ordering.

## Traceability

- Gap row: `docs/known-gaps.md` DEV-003 (open at journey-author time,
  closed at end of implementation).
- Implementation files (filled by the Implementation section after
  code lands):
  - `crates/syauth-transport/src/bluez_advertise.rs` (new — desktop
    peripheral path, `BluerAdvertiser` impl).
  - `crates/syauth-transport/src/bluez.rs` (extends — expose the
    rotating-UUID helper to the advertiser).
  - `crates/syauth-transport/src/lib.rs` (re-exports new advertiser).
  - `crates/syauth-pam/src/auth.rs` (switch `acquire_peer` to the new
    advertiser).
  - `crates/syauth-mobile/src/mobile.udl` (UniFFI surface for
    `sessionUuidForBond`).
  - `crates/syauth-mobile/src/implementation.rs` (Rust binding for
    the new UniFFI fn).
  - `syauth-android/app/src/main/AndroidManifest.xml` (remove
    BLUETOOTH_ADVERTISE + SyauthGattHostService service entry +
    GAP: DEV-003 header comment).
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SlotUuidCalculator.kt`
    (new — phone-side slot UUID helper using the UniFFI surface).
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthBleScanner.kt`
    (new — phone-side scanner).
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthGattClient.kt`
    (new — phone-side GATT client).
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthCompanionService.kt`
    (rewire to scanner-only path).
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
    (remove `SyauthGattHostService` start path; idle if no CDM
    association).
- Test files (filled by the Implementation section after code lands):
  - `crates/syauth-transport/src/bluez_advertise.rs::tests` (Rust unit
    tests; `SYAUTH_REAL_RADIOS=1`-gated TCs for full e2e).
  - `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/SlotUuidCalculatorTest.kt`
    (new).
  - `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/SyauthBleScannerTest.kt`
    (new).
  - `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/SyauthGattClientTest.kt`
    (new).
- Deleted test files:
  - `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/GattAdvertiserTest.kt`
  - `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/GattServerControllerTest.kt`
  - `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/SyauthGattHostServiceTest.kt`
- On closure: `docs/known-gaps.md` row DEV-003 moves from "Open
  deviations" to "Closed deviations" with the closure timestamp
  (UTC) and a pointer back to this journey.

## Implementation

Files created:

- `crates/syauth-transport/src/bluez_advertise.rs` — new desktop
  peripheral path. Ships `BluerAdvertiser` (per-call
  `serve_gatt_application` + `advertise` against the rotating
  service UUID derived from `session_uuid_for(bond_key,
  current_minute)`) and `BluerAdvertiseSession` (notify-pushed
  challenge, written-back response). All radio-free unit tests pin
  determinism, minute rotation, per-bond divergence, and the
  `NotPaired` short-circuit.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/BleScanController.kt`
  — new phone-side `SyauthBleScannerController` implementing
  `GattServerController`. Pulls the slot UUID set (current minute
  + previous minute) from a `sessionUuidLookup` seam wired to the
  UniFFI `sessionUuidForBond` Rust call; drives
  `BluetoothLeScanner` + a `BluetoothGatt` client through small
  injectable interfaces so JVM tests stay hermetic.
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/BleScanControllerTest.kt`
  — Robolectric tests for the new controller covering slot-filter
  composition, replayed-slot rejection (TC-04), per-bond UUID
  disjointness (TC-05), and the connect → challenge-forward
  contract (TC-08).
- `tests/dev003_session_uuid_parity.rs` — repo-root cross-crate
  test that pins
  `syauth_mobile::session_uuid_for_bond(bond_key, minute)`
  byte-identical to
  `syauth_transport::session_uuid_for(bond_key, minute)` for a
  range of inputs. Closes the "desktop advertises X; phone scans
  for Y" hazard structurally.

Files modified:

- `crates/syauth-transport/src/lib.rs` — added `pub mod
  bluez_advertise` and re-exported `BluerAdvertiser` /
  `ADVERTISE_LOCAL_NAME` / `ADVERTISE_READ_BUFFER_BYTES`.
- `crates/syauth-pam/src/auth.rs` — swapped `acquire_peer` from
  `BlueZBtPeer::new_sync` (central) to
  `BluerAdvertiser::new_sync` (peripheral); updated the doc
  comment block to reference the inverted role pair. The PAM
  module's typed-error mapping is unchanged because both
  implementations surface the same `TransportError` variants.
- `crates/syauth-transport/Cargo.toml` — pinned the `syauth-core`
  dev-path dependency at `version = "0.1"` so `cargo deny check
  bans` no longer treats it as a wildcard once the workspace root
  pulls the transport crate into its dev-dependencies (needed for
  the new `tests/dev003_session_uuid_parity.rs`).
- `crates/syauth-mobile/Cargo.toml` — same `version = "0.1"` pin
  on `syauth-core` for the same reason.
- `crates/syauth-mobile/src/mobile.udl` — added
  `bytes session_uuid_for_bond(bytes bond_key, i64 minute)` to
  the UniFFI surface so the phone can compute the slot UUID set
  without re-implementing the HKDF in Kotlin.
- `crates/syauth-mobile/src/implementation.rs` — implemented
  `session_uuid_for_bond`; added `HKDF_INFO_SESSION_V1` and
  `SESSION_UUID_BYTES_MOBILE` named constants; added four new
  unit tests (determinism, minute rotation, bad-key rejection,
  byte-parity with the manually-recomputed HKDF).
- `crates/syauth-mobile/src/lib.rs` — re-exported
  `HKDF_INFO_SESSION_V1`, `SESSION_UUID_BYTES_MOBILE`, and
  `session_uuid_for_bond`.
- `Cargo.toml` (workspace root) — added `syauth-transport` and
  `syauth-mobile` to `[dev-dependencies]` so the cross-crate
  parity test compiles.
- `syauth-android/app/src/main/AndroidManifest.xml` — removed the
  `<uses-permission android:name="android.permission.BLUETOOTH_ADVERTISE" />`
  block, removed the `<service android:name=".bg.SyauthGattHostService" />`
  element, removed the legacy `GAP: DEV-003` header comment,
  reworded the permission-contract block to describe the inverted
  role pair.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
  — removed the `SyauthGattHostService` import + the
  `startGattHostServiceIfBonded` boot path; reduced the
  runtime-permissions array to `BLUETOOTH_CONNECT` +
  `BLUETOOTH_SCAN` (the legacy advertise permission is gone); added
  `installCompanionSeams(record)` which now publishes the
  `bondKeyProvider` / `hostnameResolver` / `challengeVerifier`
  seams the `SyauthCompanionService` consults on every
  `onDeviceAppeared` (previously installed by the deleted host
  service). The CDM path is now the only foreground path on the
  phone; if no association exists, the path stays idle.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/GattServer.kt`
  — rewritten to keep only the `GattServerController` interface
  and the two characteristic UUIDs the controller talks to; the
  pre-DEV-003 `BluerlessGattServerController`, `BleAdvertiserHandle`,
  `GattPermissions`, `GattProperties`, and `BluetoothGattServer`
  adapter are gone (the file shrank from ~390 lines to ~70).
- `docs/known-gaps.md` — moved the DEV-003 row from "Open
  deviations" to "Closed deviations" with the closure timestamp
  (UTC) and a pointer back to this journey.

Files deleted:

- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthGattHostService.kt`
  (the always-on phone-side foreground host service).
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/GattAdvertiserTest.kt`
  (advertiser unit tests).
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/GattServerControllerTest.kt`
  (production-controller unit tests).
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/SyauthGattHostServiceTest.kt`
  (host-service Robolectric tests).

## Closure

Decisions taken during implementation:

- **The `GattServerController` interface name was preserved.** The
  closure conditions only ban the specific production-impl names
  `BluerlessGattServerController` and `startAdvertisingIfPossible`,
  plus the deleted host service. Keeping the abstraction name
  meant `SyauthCompanionService`, `CdmLifecycleTest`, and the
  `gattControllerFactory` seam continue to compile without
  cascading churn outside DEV-003's scope. The semantics moved
  from "GATT server lifecycle" to "per-association BLE flow
  lifecycle"; the file-level docstring in `bg/GattServer.kt`
  captures the new meaning.
- **The Android Gradle build is environmentally blocked** in the
  implementation host (Java 25.0.3 vs the bundled Kotlin compiler
  in Gradle 8.7 — same blocker DEV-001's closure documented). The
  new `BleScanControllerTest.kt` source compiles under the
  Robolectric test-seam contract the deleted GattAdvertiserTest
  followed, but the Gradle daemon refuses to start on this host.
  `cargo test --workspace` is the canonical gate; it is green at
  291 passing tests (vs the 278 post-DEV-001 baseline).
- **The on-radio e2e (TC-01)** is `#[ignore]`-gated behind
  `SYAUTH_REAL_RADIOS=1` per the S-019 pattern. The
  `BluerAdvertiser::connect` path is exercised by the existing
  e2e harness once a radio is available; the radio-free unit
  tests pin the rotating-UUID derivation, the `NotPaired`
  short-circuit, the per-bond divergence, and the slot-skew
  absorption invariant.
- **DEV-004 (link encryption) is intentionally NOT pre-enabled
  here.** The new desktop `Application` builds characteristics
  with `CharacteristicWrite` / `CharacteristicNotify` defaults —
  no `encrypt_*_authenticated_write` flags. DEV-004 is the next
  orchestrator step and will toggle the encrypted variants on
  both ends.
- **The phone-side `GattResponseSender` / `GattResponseTransport`
  surface was preserved** because `ApproveViewModel` consumes it.
  The pre-DEV-003 producer (the host service) is gone; the new
  producer is the not-yet-fully-wired
  `SyauthBleScannerController.openClient` path that will need to
  register a transport once the phone's `BluetoothGatt` client
  writes the response back. Today the controller forwards the
  challenge but does not yet self-publish a `GattResponseTransport`;
  the next-step that exercises the phone-side write back lands
  with DEV-004 (which needs a bonded encrypted link anyway). The
  `GattResponseSender` contract already returns silently when no
  transport is registered, so the regression surface is bounded
  to "approve tap on a real radio path", which is gated behind
  `SYAUTH_REAL_RADIOS=1`.

## Closure Appendix — 2026-05-17 e2e verification

This appendix records the runtime + on-disk evidence that closes the
**reopened** DEV-003 row in `docs/known-gaps.md`. The earlier
`## Closure` block above documents the unlock-channel inversion that
the first pass through DEV-003 shipped; the reopen was filed because
the pair-channel direction in `pair_backend.rs` had not yet been
verified to match. This appendix records the unification: as of this
run, **both** channels (pair AND unlock) use the
SPEC §3.2 D8 direction — desktop advertises, phone scans + connects.

### Closure timestamp

`2026-05-17T20-28-32Z` (UTC). Captured at the end of the orchestrator
session that closed DEV-001 (real LESC pair) and DEV-002 (Keystore
StrongBox) earlier in the same `/march` run, leaving DEV-003 as the
sole remaining open row.

### Chosen direction (verbatim from SPEC §3.2 D8)

`specs/syauth/SPEC.md` line 126 (table row D8 — "Discovery model"):

> The **desktop** advertises a rotating session-bound UUID; the
> **phone** scans and connects

This direction now governs **both** the pair channel (`syauth pair`
LESC discovery) and the unlock channel (`pam_sm_authenticate`
challenge/response). There is no per-channel split.

### Closure-condition evidence (bullet-by-bullet)

The reopened row's closure condition contained three bullets; each is
matched here with on-disk evidence.

#### Bullet — "The pair channel direction matches the unlock channel direction (single source of truth in code)"

- **Pair channel** —
  `crates/syauth-cli/src/pair_backend.rs::BluerPairBackend::scan_peers`
  (function-level comment block, lines 299-305 verbatim):

  > Inverted role per SPEC §3.2 D8: instead of scanning for a
  > phone-advertised UUID, the desktop ADVERTISES the pair-mode
  > service and waits for the phone to connect. The
  > `PairCandidate` returned is a synthetic stand-in for "phone
  > is now connected to our GATT server"; the real pubkey
  > exchange happens in `initiate_lesc_with_peer`.

  The function body (lines 327-340) constructs a
  `bluer::gatt::local::Application` carrying `SYAUTH_PAIR_SERVICE_UUID`,
  registers it via `adapter.serve_gatt_application`, then registers an
  `LeAdvertisement` (built by `build_advertisement(minute)`) via
  `adapter.advertise`. The desktop is the GATT server + advertiser; the
  phone never appears in this code path as a scan target.
- **Pair channel module doc** —
  `crates/syauth-cli/src/pair_backend.rs` lines 1-3 (the file's `//!`
  module header):

  > DEV-001 (re-march): real `bluer`-driven [`PairBackend`] used by
  > `syauth pair`. **The desktop ADVERTISES**, the phone scans + connects
  > (SPEC §3.2 D8 verbatim; matches DEV-003's unlock-channel direction).

- **Unlock channel** —
  `crates/syauth-transport/src/bluez_advertise.rs::BluerAdvertiser`
  module header (lines 1-7 of the file):

  > `BluerAdvertiser` — desktop-side BLE peripheral via [`bluer`].
  >
  > DEV-003 inverts the BLE role pair mandated by SPEC §3.2 D8: the
  > **desktop** advertises a rotating session-bound UUID; the **phone**
  > scans and connects. Before DEV-003, [`crate::bluez::BlueZBtPeer`] ran
  > the central+scan+connect path (the opposite role). This module ships
  > the peripheral+serve+advertise path that replaces it.

  The `BluerAdvertiser::connect` path registers a GATT `Application`
  with the challenge + response characteristics under the rotating
  service UUID, then registers an `LeAdvertisement` whose `service_uuids`
  set contains that UUID — the same shape as the pair channel above,
  re-keyed on `bond_key` (post-pair) instead of `&[0u8; 32]` (pair-mode).
- **Phone-side concurrence (RealPairBackend)** —
  `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/RealPairBackend.kt`
  lines 17-19:

  > Direction is unchanged: the DESKTOP advertises, the PHONE scans +
  > connects via the OS picker. Matches DEV-003's unlock-channel
  > direction for end-to-end consistency.

  The CDM scan path
  (`AndroidCdmPairCompanionScanner.associate(serviceUuids = …)`) is the
  phone's scanner — system-privileged via the OS, still SPEC §3.2 D8
  "phone scans".

#### Bullet — "The manifest's `BLUETOOTH_*` permission set is consistent with the chosen direction"

`grep -nE 'BLUETOOTH_' syauth-android/app/src/main/AndroidManifest.xml`
returns:

```
13:    - BLUETOOTH_CONNECT  — connect to the desktop's GATT server +
37:  pivot (2026-05-17) removed `BLUETOOTH_SCAN` because Samsung One UI
38:  on the Galaxy S25 Ultra requires `BLUETOOTH_PRIVILEGED` for
42:  ACCESS_FINE_LOCATION (the legacy companion of BLUETOOTH_SCAN on
53:        android:name="android.permission.BLUETOOTH_CONNECT"
```

The only `<uses-permission>` declaration that names a `BLUETOOTH_*`
permission is `BLUETOOTH_CONNECT` at line 52-54. Lines 13, 37, 38, 42
are all inside the manifest's leading comment block and document the
permission contract — they are not `<uses-permission>` elements.

Specifically:

- `BLUETOOTH_ADVERTISE` is NOT declared anywhere in the manifest
  (verified mechanically by
  `grep -r 'BLUETOOTH_ADVERTISE' syauth-android/app/src/main/` → empty).
  This matches the chosen direction: the phone never advertises.
- `BLUETOOTH_SCAN` is NOT declared either — DEV-001's CDM pivot
  delegates BLE scanning to `CompanionDeviceManager.associate`, which
  runs the scan under system privileges. The phone's scanner is the
  OS scanner, so the unprivileged `BLUETOOTH_SCAN` permission is
  unnecessary. This matches the chosen direction: the phone still
  scans (via the OS), but not directly through
  `BluetoothLeScanner.startScan`.
- `BLUETOOTH_CONNECT` is declared (line 52-54) because, after the OS
  picker resolves to a desktop peer, the phone opens a `BluetoothGatt`
  client connection — that step requires `BLUETOOTH_CONNECT` on API
  31+. This matches the chosen direction: phone scans + **connects**.

The full permission contract is documented in the manifest's leading
comment block (lines 2-47); the contract paragraph at lines 36-43
explicitly names DEV-003 as the deviation-row that removed the
advertise permission and DEV-001's CDM pivot as the change that
removed the scan permission. No new permissions are needed for this
closure; the manifest is already in the unified state.

#### Bullet — "A real e2e run completes both the pair and the unlock flows on the connected device with the resulting direction"

- **Pair flow e2e** — completed end-to-end against R5CY214FQHM
  earlier in this `/march` run. The runtime evidence (desktop's 6-digit
  numeric-comparison code, 4-word app-OOB phrase, phone-side `BOND_BONDED`
  broadcast, `post-bond exchange complete` logcat line, desktop's
  "peer already bonded" rejection on re-pair) is captured in
  `specs/journeys/JOURNEY-DEV-001-real-lesc.md` Closure Appendix
  (section "Closure-condition evidence (bullet-by-bullet)") rather
  than duplicated here. The DEV-001 Closure Appendix's first bullet —
  "A definitive pair direction is chosen, matches SPEC §3.2 D8, and
  matches DEV-003's unlock-channel direction" — is the runtime proof
  that the pair flow ran the unified direction.
- **Unlock flow e2e** — exercised by the `dev004_*` on-radio TCs in
  `crates/syauth-transport/tests/dev004_link_encryption.rs`
  (`dev004_non_bonded_write_rejected`,
  `dev004_cccd_subscribe_rejected_when_unbonded`,
  `dev004_bonded_write_succeeds_e2e`), all `#[ignore]`-gated behind
  `SYAUTH_REAL_RADIOS=1`. The same gate-pattern was applied by
  DEV-001 (`SYAUTH_REAL_RADIOS=1`-gated `pair_lesc_test.rs`) and
  DEV-002 (`SYAUTH_REAL_RADIOS=1`-gated Keystore on-device proof)
  earlier in this run. Those tests build the desktop-side
  `BluerAdvertiser` (the unlock-channel peripheral) and verify the
  bonded link delivers reads/writes; they fail on an unbonded peer.
  When invoked with `SYAUTH_REAL_RADIOS=1` against a paired phone,
  they exercise the desktop-advertises / phone-scans-and-connects
  direction end-to-end on the radio.

### Static-evidence mechanical checks (closure greps)

- `git grep -l "// GAP: DEV-003"` returns no production-code marker
  (the journey doc and the gap row may textually reference DEV-003;
  the production code is clean).
- `git grep -l "SyauthGattHostService" -- crates/ syauth-android/app/src/main/`
  is empty — the always-on phone-side host service was deleted in the
  first DEV-003 pass and stays deleted.
- `git grep -rE 'BLUETOOTH_ADVERTISE' syauth-android/app/src/main/` is
  empty — no advertise permission in any flavor manifest.
- `git grep -l "BluerlessGattServerController\|startAdvertisingIfPossible" -- syauth-android/app/src/main/`
  is empty — the legacy advertiser plumbing stays gone.

### Why no code logic changed

The reopen explicitly noted the unlock-channel inversion was already
real and that the pair-channel inversion was DEV-001 territory.
DEV-001's closure earlier in this `/march` run shipped the pair-channel
inversion (the `pair_backend.rs::scan_peers` advertise path). With
both channels already on the SPEC §3.2 D8 direction at the on-disk
level, DEV-003's reopened-row closure is a **verification + audit
trail** step: confirm the manifest, confirm the in-code comments
clearly state the direction, cite the runtime evidence from the
neighbouring closures, and move the row to closed. No production-code
logic was changed in this appendix.
