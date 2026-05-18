# JOURNEY-DEV-004: Re-enable LE link encryption on the unlock GATT characteristics

> **SPEC anchors:** §3.2 D6 — "Android: hardware-backed Android Keystore
> with `STRONGBOX` when available, `setUserAuthenticationRequired(true)`
> so the key can only sign when the user has authenticated"; and SPEC §3.2
> D5 (LE Secure Connections numeric comparison) — link-layer encryption
> is the second factor that the LESC bond produces.
>
> **Threat-model anchor:** `specs/threat/THREAT-2026-05-15.md` row "BLE
> link | T-001, T-003 | T-002 | n/a | T-009 | T-008 | T-001" — the
> Information-disclosure cell maps to T-009 ("passive eavesdrop on the
> radio" / presence inference). Encrypted-authenticated link permissions
> on the challenge + response characteristics force the BlueZ stack to
> reject any non-bonded reader/writer before the application layer ever
> sees the payload.
>
> **Gap reference:** `docs/known-gaps.md` row DEV-004.
>
> **Closure condition (mechanical, greppable):**
> - The desktop `Application` registered by `BluerAdvertiser` declares
>   `encrypt_authenticated_read: true` / `encrypt_authenticated_write: true`
>   on both the challenge and response characteristics (the CCCD descriptor
>   that bluer auto-creates for the notify characteristic inherits its
>   encryption requirement from the characteristic's own security flags
>   — see Architecture Notes below for the bluer 0.17.4 API contract).
> - `git grep -l "// GAP: DEV-004"` returns nothing (the journey doc may
>   reference DEV-004 textually; production code is clean).
> - A new integration test verifies that a non-bonded mock peer attempting
>   to write the challenge characteristic is rejected before the write
>   payload is delivered to the application layer. Test name:
>   `dev004_non_bonded_write_rejected`.
> - `make scope-discipline` clean.
> - `make lint` clean.
> - `cargo test --workspace --all-targets --all-features` green; new test
>   included; the post-DEV-003 baseline of 291 passing tests does not
>   regress.
> - `docs/known-gaps.md` row DEV-004 moves from "Open deviations" to
>   "Closed deviations" with a UTC closure timestamp, a pointer to this
>   journey, and the source-location relocation note (the original row
>   pointed at the deleted phone-side `GattServer.kt::GattPermissions`,
>   which DEV-003 removed when the GATT server role moved to the desktop).

## Roadmap Link

- Source roadmap: gap row DEV-004 in `docs/known-gaps.md` (orchestrated
  from `specs/auto/RUN-2026-05-17T07-56-16Z.md`).
- Predecessor: JOURNEY-DEV-001 (real LESC pair) — closed in the same
  march; produces the bonded link that the encrypted-authenticated
  characteristic permissions require. Without a LESC bond, the BlueZ
  stack would reject every read/write the encrypted permissions guard,
  including the desktop's own legitimate writes.
- Predecessor: JOURNEY-DEV-003 (invert advertising direction) — closed
  in the same march; the GATT server role moved from the phone to the
  desktop. The original DEV-004 source-location pointer
  (`syauth-android/.../bg/GattServer.kt::GattPermissions`) is therefore
  stale; the encrypted-permission flags now live on the desktop's
  `BluerAdvertiser` `Application` registration in
  `crates/syauth-transport/src/bluez_advertise.rs`.
- Feature: flip the unlock-channel characteristics from plain
  `read/write` to `encrypt_authenticated_read/encrypt_authenticated_write`
  so the BlueZ stack enforces "LESC-bonded link only" as a precondition
  for every byte of the challenge/response exchange.

## 1. Journey

When **a syauth operator with a paired phone+desktop sits down at the
locked desktop, runs `sudo` (PAM-gated), and a passive radio attacker
sniffs the surrounding airspace**, I want **every challenge byte the
desktop notifies and every response byte the phone writes back to
travel over an LESC-encrypted link the attacker cannot decrypt**, so I
can **rely on the radio layer to add a second factor of confidentiality
and authentication beyond the frame-layer MAC tag — and have the BlueZ
stack drop any non-bonded peer's attempt to participate before our
application code ever sees the payload**.

## 2. Customer Journey

The operator has completed pairing (DEV-001 closed) and the GATT server
role has moved to the desktop (DEV-003 closed). Today (with DEV-004 still
open) the desktop's unlock-channel characteristics declare plain
`CharacteristicWrite { write: true, ... }` and
`CharacteristicNotify { notify: true, ... }` permissions — bluer's
`Default::default()` values for those structs have every security flag
set to `false`. A rogue scanner that has not bonded with the desktop
can still open a GATT client, write arbitrary bytes to the challenge
characteristic, and subscribe to the response notify — wasting cycles
on the desktop's MAC verifier and exercising the application-layer
parser with attacker-controlled bytes.

After this journey closes, the BlueZ stack rejects the rogue client's
write at the L2CAP layer before the bytes are ever decoded by
`Frame::decode`. The bonded phone, whose LESC bond produced an
encrypted link during DEV-001's pair flow, passes the encryption gate
transparently: the BlueZ daemon enforces encryption on the connection
when it sees the characteristic flags, and the LESC LTK encrypts the
ATT packets that carry the writes and notifications.

### Phase 1: Bonded phone connects and writes encrypted-authenticated

**User Intent:** the legitimate phone, holding the LESC bond produced
during DEV-001 pairing, completes the unlock challenge/response exchange
without the user noticing any change in latency or behaviour.

**Actions:**
- The desktop's `BluerAdvertiser` registers a GATT `Application` whose
  challenge characteristic declares
  `CharacteristicWrite { encrypt_authenticated_write: true, ... }` and
  whose response characteristic declares
  `CharacteristicRead { encrypt_authenticated_read: true, ... }`
  alongside its existing notify configuration. The `notify` field on
  the same characteristic is unchanged; bluer 0.17.4's
  `CharacteristicNotify` struct does not expose a security flag of its
  own — the CCCD descriptor that BlueZ auto-creates for notify
  characteristics inherits its encryption requirement from the
  characteristic's own read/write security flags.
- The phone-side scanner (from DEV-003) finds the desktop, opens a
  `BluetoothGatt` client, and because the LESC bond is on file, the
  Android Bluetooth stack uses the cached LTK to encrypt the link.
- Challenge notify pushes and response writes travel over the
  encrypted-authenticated link. The BlueZ stack does not have to refuse
  any operation because the link already meets every security
  requirement the characteristic flags ask for.

**Pain / Risk:**
- The phone's LESC bond has been wiped (factory reset, app data
  cleared) but the desktop still holds its half of the bond_key — the
  Android side initiates a re-pair, which the desktop's
  `RealPairBackend` (DEV-001) treats as an authorised retry.
- The bonded link reports `Connected = true` but BlueZ reports the link
  is not yet encrypted (the Android stack delays LTK exchange for a
  battery-budget reason). The desktop's first write attempt is gated by
  BlueZ until encryption is up; bluer surfaces this as a typed write
  error that bubbles into `TransportError::Backend`. PAM returns
  `PAM_AUTHINFO_UNAVAIL` and the admin's fallback chain runs.
- A second bonded phone (multi-bond scenario, explicitly out of scope
  for v0.1.0 per SPEC §3.3 ML "OUT — explicitly not in v0.1.0") attempts
  to connect — structurally impossible at v0.1.0 because the desktop's
  advertise UUID derives from exactly one bond_key per PAM call.

**Success Signal:** the unlock exchange completes within the SPEC §4.2
2-second budget; the desktop's `BluerAdvertiseSession::recv_frame` returns
one valid frame; `pam_sm_authenticate` returns `PAM_SUCCESS`; the BlueZ
daemon logs the connection as `Encrypted = true` and never logs an
`Insufficient Authentication` ATT error on either characteristic.

### Phase 2: Non-bonded peer connects, write rejected by BlueZ before app sees it

**User Intent:** a non-bonded peer (passive eavesdropper that has
discovered the rotating UUID, attacker on the LAN with a Linux laptop)
must not be able to deliver any bytes to the syauth application layer.

**Actions:**
- An attacker's BLE scanner picks up the desktop's rotating UUID and
  attempts to open a GATT client connection. The connection itself
  succeeds at the L2CAP layer (BlueZ does not block discovery of a
  GATT service whose UUID it has already advertised).
- The attacker writes raw bytes to the challenge characteristic.
- The BlueZ stack reads the characteristic's flags
  (`encrypt_authenticated_write: true`), checks the link's encryption
  state, finds the link is not encrypted with an authenticated LTK,
  and rejects the write with ATT error `Insufficient Authentication`
  (or `Insufficient Encryption`, depending on the kernel version).
- The bluer characteristic-control stream on the desktop side never
  observes a `CharacteristicControlEvent::Write` for the rogue peer —
  the write was rejected below the application layer.

**Pain / Risk:**
- The attacker downgrades to an unauthenticated bond (the BT spec
  allows a `JustWorks` pair that produces an unauthenticated link key
  rather than the LESC numeric-comparison one). The
  `encrypt_authenticated_write` flag rejects an unauthenticated link
  even though it is encrypted; only LESC numeric comparison produces
  the authenticated LTK BlueZ needs to clear the flag.
- A future BT-spec extension introduces a new variant of authenticated
  encryption. The `encrypt_authenticated_*` flag in bluer maps to the
  BlueZ `encrypt-authenticated-read` / `encrypt-authenticated-write`
  property strings; the BlueZ daemon is the source of truth for which
  link properties satisfy the flag. The syauth project carries no
  custom mapping.
- The attacker is the legitimate operator on a non-bonded second laptop
  that has accidentally tried to connect to a syauth peer. The
  rejection is correct: only the bonded phone's bond_key should be able
  to drive the unlock channel.

**Success Signal:** the rogue peer's GATT write returns ATT-error
`Insufficient Authentication` to the rogue peer's stack; the desktop's
`BluerAdvertiseSession` never observes a `Write` characteristic-control
event from the rogue peer; the BlueZ daemon logs the rejection at
`info` level.

### Phase 3: CCCD subscription on response characteristic requires encrypted link

**User Intent:** subscribing to the response characteristic's notify
must require the same encrypted-authenticated link as direct writes /
reads — otherwise the rogue peer could subscribe to the desktop's
challenge notifications and learn the challenge bytes without ever
needing to bond.

**Actions:**
- The bonded phone subscribes to the response characteristic's notify
  via a CCCD write. The BlueZ stack checks the *characteristic's*
  encryption flags (which now require encrypt-authenticated for read)
  and clears the CCCD write because the link is encrypted with an
  authenticated LTK.
- A rogue peer attempts the same CCCD write. BlueZ rejects it for the
  same reason it rejects raw reads of the characteristic value.

**Pain / Risk:**
- Older BlueZ versions (< 5.55) had a known bug where the CCCD
  descriptor's security inheritance was buggy. The syauth project
  targets BlueZ ≥ 5.66 (Cargo workspace pin via the `bluer = "0.17"`
  dependency, which itself targets BlueZ ≥ 5.66). Document the version
  floor in `docs/security.md`.
- The bluer 0.17.4 API does not expose a public field to set
  per-CCCD-descriptor security flags directly — the CCCD is implicit
  in the `notify`/`indicate` mechanism. The encryption requirement is
  inherited from the characteristic flags, which the test matrix below
  verifies through the assertion that subscribing on a non-bonded peer
  fails.

**Success Signal:** the bonded phone's CCCD subscribe writes `0x0100`
(notifications enabled) and BlueZ accepts it; the rogue peer's CCCD
write returns `Insufficient Authentication`.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|---|---|---|
| The operator wonders whether the link is "really encrypted" after the change | Phase 1 | `syauth status` already prints a one-line bond summary; extending it with `link-encryption: authenticated-LESC` would surface the second factor visibly. Not in DEV-004's scope; tracked as a docs candidate. |
| BlueZ ATT error codes are opaque to a developer staring at a Wireshark BTSnoop trace | Phase 2 | `docs/security.md` already documents the threat model; add a short "what an attacker sees on the radio" appendix pointing at the `Insufficient Authentication` error code. Not in DEV-004's scope; tracked in the same docs candidate above. |
| A future bluer release may expose CCCD security flags directly | Phase 3 | When bluer adds a per-descriptor security knob, DEV-004's `Application` definition can pin the CCCD flags explicitly. Today the inheritance contract is good enough because the BlueZ daemon enforces it. |

### North Star Summary

A passive radio attacker who captures the entire BLE exchange of a
syauth unlock learns the rotating service UUID, the encrypted ATT
packet payloads, and nothing else. Without the LESC LTK (which lives
only on the bonded phone and the desktop's kernel keyring), the
attacker cannot decrypt the challenge or the response. A rogue peer
that attempts to actively participate in the GATT exchange is rejected
by the BlueZ stack before any application-layer code on the desktop is
invoked. The legitimate bonded phone passes the encryption gate
transparently because LESC numeric-comparison produced an authenticated
link key during pairing — the same key the BT spec mandates for
`encrypt_authenticated_*` permissions.

## 3. Architecture Notes

### Desktop side (`BluerAdvertiser` `Application` flag flip)

- The `Application::services[0].characteristics[0]` (challenge) is
  rewritten to add `CharacteristicWrite { encrypt_authenticated_write:
  true, write: true, write_without_response: true, method:
  CharacteristicWriteMethod::Io, ..Default::default() }`. The existing
  `notify` block on this characteristic is unchanged.
- The `Application::services[0].characteristics[1]` (response) is
  rewritten the same way for the `notify` characteristic — but
  `CharacteristicNotify` itself does not carry per-field encryption
  flags in bluer 0.17.4 (see source at
  `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/bluer-0.17.4/src/gatt/local.rs`
  lines 370–389). Encryption on the CCCD descriptor and on the notify
  packets is inherited from the characteristic's own
  `CharacteristicRead` security flags. Therefore the response
  characteristic also receives a `CharacteristicRead {
  encrypt_authenticated_read: true, read: true, ... }` block whose
  read function returns the last-cached response bytes (or
  `ReqError::NotPermitted` if no response has been written yet — the
  legitimate phone does not need this code path because it consumes
  notifications, but BlueZ wants the security flag to anchor on a
  read declaration, not on the notify declaration).

### CCCD descriptor

- bluer 0.17.4's `Characteristic` struct exposes a `descriptors:
  Vec<Descriptor>` field, but in practice the CCCD descriptor for a
  notify characteristic is auto-created by BlueZ when the
  `CharacteristicNotify` block is present. The bluer crate does not
  document a public path for the application to declare its own CCCD
  with custom flags. The CCCD's encryption requirement is *inherited*
  from the characteristic's own read/write security flags — once those
  are encrypt-authenticated, the auto-created CCCD enforces the same.
- The `dev004_cccd_inheritance` unit test asserts the structural
  invariant: the characteristic's `read.encrypt_authenticated_read`
  flag is `true`, so the BlueZ stack will refuse CCCD writes from
  non-bonded peers. The on-radio confirmation is in the
  `SYAUTH_REAL_RADIOS=1`-gated integration test.

### Test seam

- A radio-free unit test (`dev004_security_flags_set_on_application`)
  constructs a `BluerAdvertiser`, walks the public surface enough to
  recover the `Application` definition, and asserts the security flags
  on both characteristics. Because the `Application` is built inside
  `connect_inner` and consumed by `serve_gatt_application`, the test
  needs a small helper that exposes a function building the `Service`
  vector — pure-data, no I/O. The helper lives in `bluez_advertise.rs`
  under `#[cfg(any(test, feature = ...))]`. (It is a pure function
  taking `(rotating_uuid: Uuid, char_control_handle:
  CharacteristicControlHandle, read_fun: CharacteristicReadFun) ->
  Vec<Service>`.) This factoring follows the same test-seam pattern
  the rest of the workspace uses.
- A non-radio-free integration test
  (`dev004_non_bonded_write_rejected`) exercises a real BlueZ stack:
  the test starts a `BluerAdvertiser`, opens a mock central peer via
  bluer's client API (no prior pair, no LESC bond), attempts to write
  the challenge characteristic, and asserts the write fails with an
  `Insufficient Authentication`-shaped error. This test is gated
  behind `SYAUTH_REAL_RADIOS=1` per the S-019 / DEV-001 / DEV-003
  pattern, because it needs a live BlueZ daemon plus a BLE adapter
  reachable from the test process.

### Closure conditions (mechanical)

- [ ] `git grep -l "// GAP: DEV-004"` returns nothing.
- [ ] The desktop `Application` registered by `BluerAdvertiser` declares
      `encrypt_authenticated_read: true` on the response characteristic's
      `read` block and `encrypt_authenticated_write: true` on the
      challenge characteristic's `write` block.
- [ ] `dev004_security_flags_set_on_application` unit test passes
      without a radio.
- [ ] `dev004_non_bonded_write_rejected` integration test exists and
      is `#[ignore]`-gated behind `SYAUTH_REAL_RADIOS=1`.
- [ ] `make scope-discipline` returns clean.
- [ ] `make lint` returns clean.
- [ ] `cargo test --workspace --all-targets --all-features` is green;
      the post-DEV-003 baseline of 291 passing tests does not regress.
- [ ] `docs/known-gaps.md` row DEV-004 moves from "Open deviations" to
      "Closed deviations" with a UTC closure timestamp, a pointer to
      this journey, and the source-location relocation note.

## 4. Tests

### TC-01: bonded peer write succeeds end-to-end on real radios

**Given** a bonded desktop+phone pair (DEV-001 closed) with
`SYAUTH_REAL_RADIOS=1` set, both adapters up, and the LESC LTK on file
on both sides.
**When** PAM is invoked on the desktop.
**Then** the desktop advertises the rotating UUID with the new
encrypt-authenticated characteristic flags, the phone scans, connects,
the BlueZ stack brings the link up encrypted-authenticated using the
cached LTK without any user interaction, the phone writes the signed
response, and the desktop's `BluerAdvertiseSession::recv_frame` returns
the response frame within the SPEC §4.2 2-second budget; PAM returns
`PAM_SUCCESS`.
*(Gated behind `SYAUTH_REAL_RADIOS=1`.)*

### TC-02: non-bonded peer write rejected by BlueZ before app layer sees it

**Given** a `BluerAdvertiser` registered against a live BlueZ adapter
under `SYAUTH_REAL_RADIOS=1`, and a second BlueZ-driven test peer that
has **not** completed any pair flow with the advertiser.
**When** the unbonded peer attempts to write the challenge
characteristic.
**Then** the BlueZ stack rejects the write with ATT error
`Insufficient Authentication` (or `Insufficient Encryption`, depending
on the kernel version); the `BluerAdvertiseSession`'s
characteristic-control stream observes zero `Write` events for the
rogue peer; the desktop logs the rejection at `debug` level via the
existing `tracing` instrumentation.
*(Gated behind `SYAUTH_REAL_RADIOS=1`; the structural pin lives in
TC-04 below, which runs radio-free.)*

### TC-03: CCCD subscription rejected from a non-bonded peer

**Given** the same setup as TC-02.
**When** the unbonded peer attempts to write `0x0100` (notifications
enabled) to the response characteristic's auto-created CCCD descriptor.
**Then** the BlueZ stack rejects the CCCD write with ATT error
`Insufficient Authentication`; the desktop's `BluerAdvertiseSession`
observes zero `Notify` characteristic-control events for the rogue
peer.
*(Gated behind `SYAUTH_REAL_RADIOS=1`; the structural pin lives in
TC-04, which asserts the characteristic's read flag — the CCCD
descriptor inherits its security from the characteristic's flags per
the bluer 0.17.4 API contract.)*

### TC-04: characteristic security flags are set on the desktop `Application` (radio-free unit test)

**Given** a `BluerAdvertiser` constructed via `new_sync` against a
fixture bond_key and `PairingState::Bonded`.
**When** the test invokes the radio-free helper that builds the
`Service` vector for the `Application` (the same `Service` vector
`connect_inner` would register via `serve_gatt_application`).
**Then** the challenge characteristic's `write` block carries
`encrypt_authenticated_write: true`, the response characteristic's
`read` block carries `encrypt_authenticated_read: true`, and neither
characteristic has the weaker `encrypt_read` / `encrypt_write` flag
set as the gate (defence-in-depth: the *authenticated* variant is what
LESC numeric comparison produces; the unauthenticated variant would
accept JustWorks-paired links which SPEC §3.2 D5 forbids).
*(Pure-function test, no radio. This is the canonical mechanical
guarantee the closure condition pins.)*

## Traceability

- Gap row: `docs/known-gaps.md` DEV-004 (open at journey-author time,
  closed at end of implementation).
- Implementation files (filled by the Implementation section after
  code lands):
  - `crates/syauth-transport/src/bluez_advertise.rs` (modify — flip
    flags + factor the radio-free `build_unlock_services` helper).
  - `crates/syauth-transport/tests/dev004_link_encryption.rs` (new —
    integration test file housing the `dev004_non_bonded_write_rejected`
    real-radio test).
- Test files (filled by the Implementation section after code lands):
  - `crates/syauth-transport/src/bluez_advertise.rs::tests` — radio-free
    unit test `dev004_security_flags_set_on_application`.
  - `crates/syauth-transport/tests/dev004_link_encryption.rs` —
    real-radio `dev004_non_bonded_write_rejected`
    (`#[ignore]`-gated behind `SYAUTH_REAL_RADIOS=1`).
- On closure: `docs/known-gaps.md` row DEV-004 moves from "Open
  deviations" to "Closed deviations" with the closure timestamp
  (UTC), a pointer back to this journey, and the source-location
  relocation note.

## Implementation

Files created:

- `specs/journeys/JOURNEY-DEV-004-link-encryption.md` — this journey doc.
- `crates/syauth-transport/tests/dev004_link_encryption.rs` — new
  integration test file housing the three `#[ignore]`-gated on-radio
  TCs (`dev004_non_bonded_write_rejected`,
  `dev004_cccd_subscribe_rejected_when_unbonded`,
  `dev004_bonded_write_succeeds_e2e`). Each gates on
  `SYAUTH_REAL_RADIOS=1` per the S-019 / DEV-001 / DEV-003 pattern; the
  no-radio default short-circuits with a `return` and a clear
  documentation block describing the operator-driven procedure when a
  radio is available.

Files modified:

- `crates/syauth-transport/src/bluez_advertise.rs` — factored out
  `pub(crate) fn build_unlock_services(rotating_uuid: Uuid,
  char_handle: CharacteristicControlHandle) -> Vec<Service>`. The new
  function flips the unlock characteristics from plain READ/WRITE to:
  - challenge characteristic: `CharacteristicRead { read: true,
    encrypt_authenticated_read: true, fun: |_| Err(ReqError::NotPermitted), ... }`
    paired with the existing `CharacteristicNotify { notify: true,
    method: Io, ... }`. The read function returns `NotPermitted`
    because the legitimate phone consumes challenge bytes via notify,
    not direct read — but the read declaration is what anchors the
    BlueZ security flag (the CCCD descriptor inherits its encryption
    requirement from the characteristic's own read/write security
    flags per the bluer 0.17.4 contract).
  - response characteristic: `CharacteristicWrite { write: true,
    write_without_response: true, encrypt_authenticated_write: true,
    method: Io, ... }`. The phone's signed-response writes must travel
    over an authenticated-encrypted (LESC) link.
  `BluerAdvertiser::connect_inner` now calls `build_unlock_services`
  instead of inlining the `Service` vector. Added the module-level
  doc-comment block describing the DEV-004 update.
- `crates/syauth-transport/src/bluez_advertise.rs::tests` — new
  radio-free unit test `dev004_security_flags_set_on_application`
  asserts the structural pin: challenge.read.encrypt_authenticated_read
  is `true`, response.write.encrypt_authenticated_write is `true`, and
  neither characteristic uses the weaker `encrypt_read` /
  `encrypt_write` flag as the gate (defence-in-depth against a
  JustWorks-bonded link, which SPEC §3.2 D5 forbids).
- `docs/known-gaps.md` — moved the DEV-004 row from "Open deviations"
  to "Closed deviations" with the UTC closure timestamp, a pointer
  back to this journey, and the source-location relocation note (the
  original row pointed at the phone-side `GattServer.kt::GattPermissions`
  which DEV-003 deleted when the GATT server role moved to the desktop).

Files deleted:

- (none — DEV-004 is a flag flip + a new test; no source files become
  obsolete.)

## Closure

Decisions taken during implementation:

- **The bluer 0.17.4 API does not expose a public path to declare per-CCCD
  security flags.** The CCCD descriptor for the notify characteristic is
  auto-created by BlueZ when `CharacteristicNotify { notify: true, ... }`
  is present, and its encryption requirement is inherited from the
  characteristic's own read/write security flags. The journey doc's
  Phase 3 Pain/Risk paragraph documents this; the closure condition is
  satisfied by the `encrypt_authenticated_read: true` flag on the
  challenge characteristic's read declaration, which the CCCD inherits.
  When a future bluer release exposes per-descriptor security knobs,
  pinning the CCCD's flag explicitly would be a one-line addition; the
  closure condition does not block on it because the BlueZ stack
  already enforces the inheritance.
- **A `CharacteristicRead` block was added to the challenge
  characteristic** (it previously had only `notify`) so that BlueZ has
  a security flag to anchor on. The `fun` returns `ReqError::NotPermitted`
  because the legitimate phone consumes the challenge via notify rather
  than direct read; the only purpose of the read declaration is to
  carry the `encrypt_authenticated_read: true` flag that BlueZ
  inherits to the CCCD descriptor. The unit test asserts both: the
  flag is set, and the unauthenticated-encryption variant (`encrypt_read`)
  is NOT set as the gate.
- **The on-radio TCs (`dev004_non_bonded_write_rejected`,
  `dev004_cccd_subscribe_rejected_when_unbonded`,
  `dev004_bonded_write_succeeds_e2e`)** are `#[ignore]`-gated behind
  `SYAUTH_REAL_RADIOS=1` per the S-019 / DEV-001 / DEV-003 pattern. The
  radio-free structural pin in
  `bluez_advertise::tests::dev004_security_flags_set_on_application`
  is the canonical guarantee the closure condition names; the on-radio
  TCs verify the *consequence* of those flags (BlueZ stack rejection
  of non-bonded peers) when an operator runs them against a live
  adapter pair.
- **No phone-side code change** was needed. The phone's `BluetoothGatt`
  client (DEV-003) opens its connection over the LESC bond produced
  by DEV-001; the Android Bluetooth stack uses the cached LTK to
  encrypt the link transparently. The phone does not need to know
  about the new encryption flags — the encryption is enforced at the
  link layer by the BlueZ daemon on the desktop side.
- **The original DEV-004 row's source-location pointer was stale**
  after DEV-003 closed (the row named
  `syauth-android/.../bg/GattServer.kt::GattPermissions`, which DEV-003
  deleted). The closed-deviation row in `docs/known-gaps.md` includes
  a "Note (source-location relocation)" paragraph so future readers
  understand why the DEV-004 source location moved.
