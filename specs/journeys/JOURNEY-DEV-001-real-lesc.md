# JOURNEY-DEV-001: Real LE Secure Connections pairing + app-level OOB

> **SPEC anchors:** §3.2 D5 (LESC numeric comparison + OOB), §3.3 ML "IN —
> v0.1.0" (`syauth pair` CLI runs LE Secure Connections numeric comparison
> and shows a 6-digit OOB confirmation in the terminal; pairing screen shows
> the same 6-digit code as the CLI for OOB confirmation).
>
> **Gap reference:** `docs/known-gaps.md` row DEV-001.
>
> **Closure condition:** `git grep -l provision-test` returns only `tests/`
> files; `StubPairBackend` has no production callers; `make scope-discipline`
> reports zero `// GAP: DEV-001` markers; the test matrix below is fully
> green on both `cargo test` and `./gradlew :app:testDebugUnitTest`.

## Roadmap Link

- Source roadmap: [specs/syauth/ROADMAP.md](../syauth/ROADMAP.md) items
  **S-011** (CLI `pair`) and **S-016** (Android pairing screen). Both items
  were prematurely marked done with stub backends; this journey reopens them.
- Feature: real LESC pairing replacing the provision-file shortcut.

## 1. Journey

When **a syauth operator wires their first phone to a Linux desktop**, I want
to **run `syauth pair` on the desktop and the syauth app on the phone, see a
6-digit pairing code on both screens, confirm it matches, then see a 4-word
app-level OOB code on both screens and confirm THAT matches**, so I can
**trust that the bonded peer key has not been MitM'd by a relay during
pairing and that any future unlock through `pam_syauth` is talking to the
phone I physically confirmed**.

## 2. Customer Journey

The operator unpacks syauth on a fresh Linux machine and a fresh Android
phone install. Today (with DEV-001 still open) they have to run `syauth
provision-test`, push a TOML file over `adb`, and trust the cable. SPEC §3.2
D5 demands proof against an attacker who controls the BLE airspace during
pairing — adb push satisfies that only if you trust adb, which you do, but
the SPEC's threat model assumes you might NOT (T-002: MitM during pairing).
This journey removes the adb-trust dependency.

### Phase 1: Adapter ready

**User Intent:** confirm BlueZ + the phone's Bluetooth stack are both up
before any pairing UI appears.

**Actions:**
- Desktop: operator runs `syauth pair --adapter hci0`. The CLI calls
  `bluer::Adapter::set_powered(true)` and `set_discoverable(true)`, then
  registers a BlueZ Agent (`org.bluez.Agent1`) with capability
  `DisplayYesNo`.
- Phone: operator opens the app. `PairingScreen` requests
  `BLUETOOTH_SCAN` / `BLUETOOTH_CONNECT` at runtime if not yet granted.

**Pain / Risk:**
- BlueZ adapter is `Down` or `Blocked` — CLI must surface a typed error,
  not panic.
- Android user denies the runtime permission — UI must show a recover
  message, not crash.
- Two `syauth pair` processes race — second one fails on
  `RegisterAgent` with `AlreadyExists`; surface as typed
  `PairError::AgentAlreadyRegistered`.

**Success Signal:** desktop prints `==> waiting for phone (60 s)`; phone
shows `Scanning for desktops…`.

### Phase 2: Scan + connect

**User Intent:** find the right desktop on the phone side.

**Actions:**
- The DESKTOP advertises a UUID dervied via
  `session_uuid_for(bond_key=None, slot=current_minute)` — a discovery
  UUID that does NOT carry the bond_key (since no bond exists yet); SPEC
  §3.2 D8 requirement. The advertise data also carries the hostname in the
  manufacturer-data field (max 26 bytes).
- The phone runs `BluetoothLeScanner` filtering for the syauth discovery
  UUID prefix (the first 12 bytes are a deterministic syauth-pair-mode
  marker; only the trailing minute slot varies).
- On match, `PairingScreen` shows "Found: `<hostname>`" with a "Pair" button
  the user taps.

**Pain / Risk:**
- Multiple desktops in range — show all, let user pick. Don't auto-pair.
- Scan times out (60 s window, then the agent unregisters).
- Phone's BLE adapter throttles scans (Android quota: 5 scans per 30 s).
  Use `SCAN_MODE_LOW_LATENCY` + back off if the OS returns
  `SCAN_FAILED_SCANNING_TOO_FREQUENTLY`.

**Success Signal:** phone shows the desktop in a list; user taps "Pair";
phone calls `BluetoothDevice.createBond()`.

### Phase 3: OS-level LESC numeric comparison

**User Intent:** confirm via the BT-spec-mandated 6-digit code that no MitM
sits on the radio layer.

**Actions:**
- Phone-initiated `createBond()` triggers Just Works fallback unless we
  enforce LESC + numeric comparison. We enforce it by:
  - On Android: the app's `BroadcastReceiver` for
    `BluetoothDevice.ACTION_PAIRING_REQUEST` reads
    `EXTRA_PAIRING_VARIANT` and rejects anything that isn't
    `PAIRING_VARIANT_PASSKEY_CONFIRMATION` (1) — Just Works is variant 3
    and gets a typed denial.
  - On Linux: the BlueZ Agent's `RequestAuthorization` callback is
    rejected (Just Works); `RequestConfirmation(device, passkey)` is the
    only accepted variant.
- BlueZ delivers the 6-digit `passkey` via the agent on the desktop side;
  Android's `EXTRA_PAIRING_KEY` carries the same number.
- The DESKTOP prints `Confirm pairing code on phone: 123456 [y/N]` and
  blocks on stdin.
- The PHONE shows `Pair with fedora? Code: 123456` with two buttons.

**Pain / Risk:**
- User taps yes on phone but the code on desktop is different (MitM):
  desktop input is N → both sides reject pairing → typed
  `PairError::OobMismatch`.
- LESC downgrade attack: an attacker spoofs IO capability to force Just
  Works. Mitigation: both sides reject any pairing variant ≠
  numeric-comparison.
- User mistypes Y on the desktop terminal under pressure — accept only
  `y` / `yes` (lowercase), reject everything else with N.

**Success Signal:** BlueZ raises `Device1::Paired = true` and the link
becomes encrypted (`Device1::Connected = true`,
`GattService1` queries on the bonded device require encryption now).

### Phase 4: App-level OOB confirmation

**User Intent:** confirm via the syauth-app-derived 4-word code that the OS
pairing did not silently complete with a wrong key (defense-in-depth above
§3.2 D5's "+ out-of-band confirmation in syauth UI").

**Actions:**
- Over the now-encrypted LESC link, the desktop initiates a one-shot
  syauth-pair GATT service (different UUID from the unlock service):
  - Desktop writes its Ed25519 host pubkey to a `host-pubkey` characteristic.
  - Phone writes its Ed25519 phone pubkey to a `phone-pubkey` characteristic.
  - Both sides derive `bond_key = HKDF-SHA256(salt=None,
    ikm=ECDH(LE-keys, derived during LESC) || host_pubkey ||
    phone_pubkey, info="syauth-bond-v1", len=32)`.
- Both sides feed `bond_key` into `syauth_core::oob_code_for_bond` and
  display the same 4 emoji-prefixed words (already implemented).
- Desktop prompts `Confirm phrase on phone matches: [emoji1 word1 …
  emoji4 word4] [y/N]`. Phone shows the same phrase with two buttons.

**Pain / Risk:**
- ECDH-shared-secret material from LE Secure Connections is NOT directly
  accessible on Android. Mitigation: skip mixing the LE shared secret;
  the LESC link encryption already guarantees the host/phone pubkey
  exchange isn't MitM'd. `bond_key = HKDF-SHA256(host_pubkey ||
  phone_pubkey, info="syauth-bond-v1")` — both sides converge to the same
  32 bytes without needing the LTK.
- An attacker who completes Phase 3 MitM (impossibly hard given LESC
  numeric-comparison) would still see different 4-word codes on each end
  if they substituted a pubkey. Mismatch → N on both → bond not written.
- User confirms Y on the desktop but N on the phone (or vice versa). Both
  sides must reach Bonded; if either side denies, both sides record the
  attempt as Failed and remove the OS pairing.

**Success Signal:** both sides write a `Bond { peer_id, pubkey, name,
created_at, status: Bonded }` record. Phone stores `bond_key` and the
phone's Ed25519 secret seed under Android Keystore (closes DEV-002 in the
same march). Desktop stores `bond_key` under
`/var/lib/syauth/keys/<peer_id>.bin`.

### Phase 5: Persist + dismiss

**User Intent:** the bond is durable; the next `syauth pair` invocation says
"already bonded" rather than re-running the flow.

**Actions:**
- Desktop: writes `Bond` via `BondStore::save` (already implemented in
  S-005) and the per-peer 0600-mode key file via the helper added in
  af2f27a.
- Phone: writes `BondRecord` via `BondStore::save` (already implemented
  in S-016's `DiskBondPersister`) — but now the record carries the real
  `bond_key`, signing seed, and pubkey from Phase 4, not the zeroed
  placeholders.
- Desktop prints `==> bonded with <hostname> as <peer_id>`; phone shows
  `Bonded with <hostname>` and dismisses the pairing route back to home.

**Pain / Risk:**
- BondStore disk full / read-only — typed `PairError::Persist` on both
  sides; if either fails, both sides remove the OS pairing.
- Conflicting bond exists (re-pair attempt on a fresh install but
  desktop still has an old key) — the `--force` flag is required, else
  the CLI refuses and points the operator at `syauth revoke`.

**Success Signal:** `syauth list` shows the new bond; `syauth status`
prints `bonds-count: 1`; the phone's `BondStore.load()` returns the new
record on app relaunch.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|---|---|---|
| Operator doesn't know whether LESC succeeded vs Just Works | Phase 3 | Print the pairing variant in the desktop CLI: `pairing variant: numeric-comparison (LESC)` — reject and exit nonzero on any other variant |
| Phone's Approve screen reuses the same code path as unlock, but pairing UI is distinct | Phase 4 | Reuse `OobScreen` composable for the 4-word display so the user sees the same visual idiom |
| Re-pairing without revoke leaves a stale entry | Phase 5 | CLI rejects with a typed "use `syauth revoke <peer>` first" message that names the existing peer |

### North Star Summary

A new operator with two unpaired devices and no prior knowledge of syauth
internals can pair their phone with their Linux desktop in under 60 seconds
by running one CLI command and tapping confirm twice. They never edit a
config file, never touch adb, and the only credential transit happens over
an LESC-encrypted BLE link gated by two independent user confirmations.

## 3. Architecture Notes (replaces the UX checklist for an internal protocol journey)

### Linux side (replace `BluerPairBackend`)

- Replace the `"BluerPairBackend …real-radio path lands in S-019"` stubs in
  `crates/syauth-cli/src/main.rs` with a real implementation in a new
  `crates/syauth-cli/src/pair_backend.rs`.
- Drive bluer's `Adapter`, `Device::pair`, BlueZ `Agent` registration.
- Add a `PairAgent` struct implementing the `Agent` callbacks
  (`RequestConfirmation`, `RequestAuthorization`, etc.). All except
  `RequestConfirmation` reject; `RequestConfirmation` blocks on a typed
  `confirm_oob` callback the CLI wires to stdin Y/N.
- After OS pairing, run the app-level pubkey exchange over a transient
  GATT service whose UUID is distinct from `SYAUTH_GATT_SERVICE_UUID` (the
  unlock channel) — call it `SYAUTH_PAIR_SERVICE_UUID`.

### Android side (replace `StubPairBackend`)

- Replace `StubPairBackend` in `MainActivity.kt` with a real backend in
  `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/RealPairBackend.kt`.
- BroadcastReceiver for `ACTION_PAIRING_REQUEST` enforces variant ==
  `PAIRING_VARIANT_PASSKEY_CONFIRMATION`.
- After bond, open a GATT client to the desktop's transient pair service,
  write phone-pubkey, read host-pubkey, derive bond_key.
- `PairingScreen` shows the 4-word OOB after Phase 4 and waits for
  Approve/Deny.

### Wire format additions

- New `SYAUTH_PAIR_SERVICE_UUID` (different fixed UUID, advertised by the
  DESKTOP only during the 60-second pairing window).
- Two characteristics: `host-pubkey` (READ, 32 bytes), `phone-pubkey`
  (WRITE, 32 bytes). Encryption-required since the link is bonded.

### Closure conditions (mechanical)

- [ ] `git grep -l "// GAP: DEV-001"` returns nothing.
- [ ] `git grep -l "StubPairBackend"` returns nothing under `crates/`
      and `syauth-android/app/src/main/`.
- [ ] `git grep -l "provision_test\|provision-test\|provision-file"` returns
      only `tests/` files (`provision-test` may live as a hidden
      `cargo` feature-gated dev tool but must not appear in `make build`'s
      output).
- [ ] `make scope-discipline` returns clean.
- [ ] `make lint` returns clean.
- [ ] `cargo test --workspace` is green, including the TC-NN cases below.
- [ ] `./gradlew :app:testDebugUnitTest` is green, including the
      Robolectric TC-NN cases below.
- [ ] On real devices: `syauth pair` from a fresh Linux + fresh Android
      install completes in < 60 s, with both 6-digit and 4-word
      confirmations visible to the operator. Logged in
      `docs/android-setup.md` as the canonical first-run flow.
- [ ] `docs/known-gaps.md` row DEV-001 moves from "Open deviations" to
      "Closed deviations" with the closure commit SHA.

## 4. Tests

### TC-01: golden path — LESC + OOB both confirmed

**Given** a fresh `syauth` install on a Linux host with BlueZ
configured for `DisplayYesNo` and a phone running the syauth app with no
prior bonds.
**When** the operator runs `syauth pair --adapter hci0`, taps Pair on the
phone, confirms the matching 6-digit code on both sides, confirms the
matching 4-word app-OOB code on both sides.
**Then** within 60 s: both sides print `bonded with <hostname> as <peer_id>`,
both stores contain identical `bond_key` (32 bytes), the bond's `peer_id`
on both sides equals `BLAKE3(host_pubkey)[..16]`, and
`syauth list` on the desktop returns the new bond.

### TC-02: OS code mismatch (MitM during numeric comparison)

**Given** a test harness that fakes the BlueZ Agent's
`RequestConfirmation` callback to receive a different passkey than the
phone-side broadcast carries.
**When** the operator confirms Y on the phone but N on the desktop (or
vice versa).
**Then** both sides reach `PairingPhase::Aborted`, the OS pairing is
torn down (BlueZ `RemoveDevice`), no bond is written on either side,
the desktop exits nonzero with `Err(PairError::OobMismatch)`.

### TC-03: Just Works downgrade rejected

**Given** a peer that requests `PAIRING_VARIANT_JUST_WORKS` (variant 3) /
`AuthorizationRequest` on the BlueZ Agent.
**When** the variant lands.
**Then** both sides immediately reject pairing with
`PairError::DowngradeBlocked`. No bond is written. The reject reason
is logged at WARN level.

### TC-04: App-level OOB mismatch (defense-in-depth)

**Given** LESC pairing succeeded (Phase 3) but a test seam swaps the
phone's exchanged Ed25519 pubkey with attacker-controlled bytes between
the phone's write and the desktop's read.
**When** the desktop derives the 4-word OOB from its view of the keys.
**Then** the desktop's 4 words differ from the phone's. The operator
sees the mismatch, taps N on the phone, and both sides reach
`PairingPhase::Aborted`. Neither side writes a bond. The OS-level
pairing is torn down.

### TC-05: Scan timeout — no peer found

**Given** no syauth-pair-advertising desktop within range.
**When** the phone waits the full 60 s scan window.
**Then** the phone returns to its home route showing
`No syauth desktop found nearby. Retry?`, no permanent state changes.

### TC-06: Multiple desktops visible — disambiguation

**Given** two Linux hosts running `syauth pair` simultaneously.
**When** the phone scans.
**Then** the phone shows a list with both `<hostname>` strings, the
operator picks one, and only the selected host pairs. The unselected
host's `syauth pair` keeps running until its own 60-s window expires.

### TC-07: Replay of an old advertise frame

**Given** the desktop's session-bound advertising UUID has rotated past
slot N.
**When** an attacker re-broadcasts the slot-N UUID at slot N+1.
**Then** the phone-side scanner sees the slot mismatch (the deterministic
prefix matches the syauth-pair-mode marker, but the trailing slot byte
fails `session_uuid_for(slot=current_minute)`) and ignores the
advertisement.

### TC-08: Re-pair without revoke

**Given** a bond exists on the desktop for the same `peer_id` (the phone
re-installed and re-paired).
**When** the operator runs `syauth pair --adapter hci0` without
`--force`.
**Then** the desktop refuses with the message
`peer <peer_id> already bonded; run 'syauth revoke <peer_id>' first or
pass --force`.

### TC-09: BlueZ adapter down

**Given** `bluetoothctl power off`.
**When** the operator runs `syauth pair --adapter hci0`.
**Then** the CLI exits nonzero with
`PairError::AdapterMissing { adapter: "hci0" }`, no agent is registered,
the user sees a one-line "enable bluetooth and retry" message.

### TC-10: Persistence failure on phone-side BondStore

**Given** the phone's `filesDir` is read-only (test harness deletes it
between pairing-screen entry and the bond-write call).
**When** the desktop has finished its bond write and is waiting for the
phone's confirmation.
**Then** the phone surfaces a typed
`PersistError(io)`, sends a Deny over the pair channel, both sides tear
down the OS pairing, the desktop reports
`PairError::PeerPersistFailed`.

### TC-11: Bond key MAC / signature regression

**Given** two devices that completed pairing per TC-01.
**When** the desktop performs one full unlock through `pam_syauth.so`
against the just-bonded peer.
**Then** the unlock succeeds end to end (same wire flow as
`pamtester syauth-test`); `/var/lib/syauth/last.log` records
`success <peer_id>`.

## Traceability

- Roadmap items reopened: S-011, S-016 (both had `[x]` DoD boxes ticked
  with stub backends).
- Gap row: `docs/known-gaps.md` DEV-001.
- Implementation files (filled by `/implement`):
  - `crates/syauth-cli/src/pair_backend.rs` (new — real `BluerPairBackend`)
  - `crates/syauth-cli/src/main.rs` (replace stub)
  - `crates/syauth-cli/src/pair.rs` (extend with OS variant enforcement)
  - `crates/syauth-transport/src/bluez.rs` (add `connect_pair_service`)
  - `crates/syauth-core/src/bond.rs` (add `bond_key_from_pubkeys` helper)
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/RealPairBackend.kt` (new)
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/PairingBroadcastReceiver.kt` (new)
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt` (wire RealPairBackend, retire StubPairBackend, retire SyauthGattHostService boot path that depends on provision-file bootstrap)
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/PairingViewModel.kt` (extend states + actions)
- Test files (filled by `/implement`):
  - `crates/syauth-cli/tests/pair_lesc_test.rs` (TC-01 .. TC-09)
  - `tests/e2e_pair.rs` (TC-11 end-to-end against bonded mock)
  - `syauth-android/app/src/test/kotlin/com/sy/syauth/android/pair/RealPairBackendTest.kt` (TC-03, TC-04, TC-10)
- On closure: `docs/known-gaps.md` row DEV-001 marked closed with the
  final commit SHA, this journey doc archived to
  `specs/journeys/JOURNEY-DEV-001-real-lesc.md` with a `## Closure`
  appendix capturing the actual decisions made and any deferred work.
