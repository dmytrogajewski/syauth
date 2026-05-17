# syauth — Known SPEC Deviations (Audit Trail)

> **Purpose.** Every place in the source where the shipped behaviour diverges
> from `specs/syauth/SPEC.md` §3.2 D1–D8 or §3.3 ML "IN — v0.1.0" gets a row
> here. Each row names:
>
> 1. the SPEC clause being weakened, by section and verbatim line,
> 2. the source location of the deviation (file + line + commit SHA),
> 3. the explicit user-approval message that authorized it,
> 4. the closure condition (the test or behaviour change that removes the
>    deviation from this file).
>
> Per `AGENTS.md` → "Scope Discipline (Non-Negotiable)", a deviation may only
> ship with a row in this file. Without a row, the deviation must not exist.
>
> Format: every row has a stable `id` so source-side `// SPEC-DEVIATION:`
> markers can reference it.

## Open deviations

### `DEV-001` — Provision file replaces LESC pairing

**SPEC clause:** §3.2 D5 — "LE Secure Connections numeric comparison + out-of-band confirmation in syauth UI (display matching code on both ends)" and §3.3 ML "IN — v0.1.0" — "`syauth pair` CLI that runs LE Secure Connections numeric comparison and shows a 6-digit OOB confirmation in the terminal" + "Pairing screen shows the same 6-digit code as the CLI for OOB confirmation."

**Shipped behaviour:** `syauth provision-test` writes a TOML file
(`syauth-provision.toml`) containing the bond_key + Ed25519 seed + pubkey in
plaintext. Operator `adb push`es it to the phone over a USB cable. No BLE
LESC pairing, no app-level 6-digit OOB, no `syauth pair` LESC happy path.

**Source locations:**
- `crates/syauth-cli/src/provision.rs` (entire module)
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/provision/` (entire module: BondBootstrap, BondStore, DiskBondPersister, ProvisionLoader, ProvisionPackage)
- `crates/syauth-cli/src/main.rs::Cmd::ProvisionTest`
- `crates/syauth-cli/tests/snapshots/cli__help_snapshot.snap` (advertises the subcommand)

**Authorized by:** user message in the v0.1 e2e push:
"I implement everything in this session, committing each piece (Recommended)"
followed by silent acceptance of `feat: provision-test CLI subcommand` (af2f27a).
NOT a true SPEC-deviation approval — the user later (this conversation)
flagged the framing as "your imagination" and asked for spec conformance.

**Status:** **NOT user-approved as a deviation.** This row exists to track
the gap; closure is on the roadmap, not approved-to-ship.

**Closure condition:** S-011 (CLI `pair` subcommand) + S-016 (Android pairing
screen) lose their `StubPairBackend` and ship a real bluer-driven LESC numeric
comparison + the app-level 4-word OOB confirmation. The
`provision-test` subcommand is removed (or gated behind `--features=demo`
that is OFF in any release build). On closure, `git grep -l provision-test`
returns only `tests/` files.

---

### `DEV-002` — Ed25519 signing seed in plaintext filesystem, not Keystore

**SPEC clause:** §3.2 D6 — "Android: hardware-backed Android Keystore with `STRONGBOX` when available, `setUserAuthenticationRequired(true)` so the key can only sign when the user has authenticated"

**Shipped behaviour:**
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/provision/BondStore.kt`
writes the 32-byte Ed25519 seed in plaintext to
`<filesDir>/syauth-bond.toml`. `MainActivity.kt::ApproveRoute` reads it
into an `InMemorySigningKeyProvider`, which feeds it to the UniFFI
`buildResponseFrame` Rust function — the seed crosses the JVM boundary in
plaintext on every unlock. Android Keystore is wired in
`AndroidKeystoreSigner` but only signs an audit token, not the wire
response.

**Source locations:**
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/provision/BondStore.kt`
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/provision/BondRecord.kt`
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt::ApproveRoute` (uses `InMemorySigningKeyProvider`)
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/InMemorySigningKeyProvider.kt` (entire class)

**Authorized by:** none (carried in on the v0.1 e2e push).

**Status:** **NOT user-approved as a deviation.**

**Closure condition:** The Ed25519 private key never appears as bytes in the
JVM or in app private storage. Sign-over-frame happens through a UniFFI
function that takes a Keystore key handle (Android `KeyStore.Entry` ref) and
the frame; the actual Ed25519 operation runs under `KeyChain` /
`KeyProtection` with `setUserAuthenticationRequired(true)` and STRONGBOX when
the device supports it. On closure, `git grep -l InMemorySigningKeyProvider`
returns no production callers.

---

### `DEV-003` — BLE advertising direction is inverted

**SPEC clause:** §3.2 D8 — "The **desktop** advertises a rotating session-bound UUID; the **phone** scans and connects"

**Shipped behaviour:** The phone hosts the GATT server and advertises the
fixed `SYAUTH_GATT_SERVICE_UUID` continuously via
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthGattHostService.kt`
+ `BluerlessGattServerController.startAdvertisingIfPossible`. The desktop
runs `bluer` central + scan + connect (the opposite role). This leaks the
phone's MAC + a stable service UUID to anyone scanning nearby — exactly the
presence-tracking the SPEC D8 rationale calls out.

**Source locations:**
- `syauth-android/app/src/main/AndroidManifest.xml` (BLUETOOTH_ADVERTISE permission + Header comment)
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/GattServer.kt::BluerlessGattServerController` (advertise plumbing)
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthGattHostService.kt` (the always-on host service that exists only because the direction is inverted)
- `crates/syauth-transport/src/bluez.rs::connect_inner` (desktop side: scan for, connect to, the phone's advertised service)

**Authorized by:** none.

**Status:** **NOT user-approved as a deviation.**

**Closure condition:** Roles are swapped per SPEC §3.2 D8.
- Desktop registers a `bluer` GATT server (or per-session advertiser) that
  publishes a rotating session-bound UUID derived from the bond_key + the
  current minute (the rotation cadence is referenced in `bluez.rs` already as
  `session_uuid_for`).
- Phone runs a `BluetoothLeScanner` filtering for the rotating UUID, opens a
  GATT client connection on match, writes the challenge response, then
  disconnects.
- `SyauthGattHostService` is deleted; `SyauthCompanionService` (CDM-bound)
  becomes the only foreground service path, gated by a real CDM association
  created during S-016's LESC pair.
- The phone no longer holds `BLUETOOTH_ADVERTISE`.

---

### `DEV-004` — GATT characteristics use plain WRITE/READ (no link encryption)

**SPEC clause:** §3.2 D6 (key storage) and the threat model in
`specs/threat/THREAT-2026-05-15.md` (T-009 "passive eavesdrop on the radio")
imply link-layer encryption — the frame layer carries a MAC tag, but a
LESC-encrypted link is the second factor.

**Shipped behaviour:**
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/GattServer.kt`
declares `PERMISSION_WRITE` / `PERMISSION_READ` on the challenge and
response characteristics — no link encryption required. Was set to
`PERMISSION_WRITE_ENCRYPTED` originally and dropped during the e2e push
because the desktop's bluer connection had no LESC bond to satisfy the
encryption gate.

**Source locations:**
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/GattServer.kt::GattPermissions`

**Authorized by:** none.

**Status:** **NOT user-approved as a deviation.** Closes naturally with
DEV-001 (real LESC pairing produces a bonded link, encryption returns).

**Closure condition:** Once `DEV-001` closes (LESC produces a bonded link),
revert to `PERMISSION_WRITE_ENCRYPTED` / `PERMISSION_READ_ENCRYPTED` on both
characteristics + CCCD descriptor. The integration test exercises a
non-bonded peer attempting to write the challenge characteristic and
asserts the write is rejected by the BlueZ stack.

---

## Closed deviations

(none yet)

---

## How to add a row

1. Re-read `AGENTS.md` → "Scope Discipline (Non-Negotiable)" before opening
   a new deviation.
2. Verify the user has explicitly approved the deviation (the approval
   message goes into the row verbatim — chat scrollback, PR comment, etc.).
3. Assign the next `DEV-NNN` id (zero-padded, monotonic).
4. Fill every section: SPEC clause, shipped behaviour, source locations,
   authorized-by, status, closure condition.
5. Add `// SPEC-DEVIATION: DEV-NNN — <one-line reason> — see docs/known-gaps.md`
   at every affected source location.
6. Run `make scope-discipline` to confirm no banned phrases remain in the
   affected files.
