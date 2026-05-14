# JOURNEY-S-010: `syauth-transport` — real BLE central via `bluer`

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md §S-010](../syauth/ROADMAP.md)
- Feature: `BlueZBtPeer`, a drop-in replacement for `MockBtPeer` behind the
  S-007 `BtPeer` / `Session` trait pair, with rotating session UUID, MTU
  fragmentation reassembly, `PrepareForSleep` suspend/resume hook, and an
  explicit `PairingState` consult on the unlock-path.

## 1. Journey

When **I am the operator running syauth on a real Fedora/Debian desktop with
a paired Android phone**, I want **the PAM module to drive a hermetic
`bluer`-backed central that advertises a rotating session-bound UUID, gracefully
survives kernel suspend/resume, and rejects every unlock attempt that originates
from a non-`Bonded` `PairingState`**, so I can **unlock my screen by walking
up with my bonded phone while a third party watching the radio cannot link two
sessions of mine and a malicious or `ProvisionalBonded` peer cannot complete an
unlock**.

## 2. CJM

The unlock path runs `pam_sm_authenticate` → `syauth-transport::BtPeer::connect`
→ session roundtrip → `PAM_SUCCESS` (or a typed failure code). Until this step
the only `BtPeer` implementation was `MockBtPeer` (S-007). S-010 ships
`BlueZBtPeer`, a real implementation backed by [bluer]
(`0.17`, the official BlueZ Rust binding). The fundamental decision is to keep
the trait surface stable: the only thing that changes for callers is the
constructor.

Five concerns drive the design:

1. **Rotating session UUID.** SPEC D8 says the desktop advertises a rotating
   session-bound UUID, scanning by phone. Per the DoD the UUID is
   `HKDF(bond_key, "syauth-session-v1" || timestamp_minute)[0..16]`. The unit
   takes a `minute` integer (floor of unix epoch / 60), not a wall-clock, so
   tests are deterministic without injecting a clock trait. HKDF salt is
   `None`: the bond key is already 32 high-entropy bytes (BLAKE3-keyed bond
   secret); a salt buys no extra security and a hard-coded salt would just be
   another constant to bikeshed. Documented per `/bt` Phase 1 "pin the protocol
   surface" rule.

2. **`PairingState` consult before any unlock-path read.** `/bt` Phase 2
   mandates that the unlock path never reads from `ProvisionalBonded`. We
   model this with a public `PairingState` enum stored on `BlueZBtPeer`. The
   `connect` method checks the variant *before* touching `bluer` at all and
   returns `TransportError::NotPaired` if the peer is `NotPaired`. The check is
   the first executable statement of `connect`; there is no way to reach the
   adapter handle without crossing it. Verified by `connect_rejects_when_not_paired`
   which constructs a `BlueZBtPeer` with `PairingState::NotPaired` and asserts
   the error variant without ever opening an adapter. A separate test
   (`new_records_pairing_state`) exercises the `Bonded` constructor.

3. **MTU fragmentation reassembly.** BLE characteristic writes are bounded by
   the negotiated MTU (typically 247 bytes post-negotiation on modern stacks).
   Frames larger than `MAX_BLE_MTU - FRAGMENT_HEADER_LEN` are split across
   multiple GATT writes. The receiver concatenates payloads in order, peeling
   off a one-byte header per segment whose high bit signals "more fragments".
   `reassemble(segments)` is a pure public function so tests drive it without
   a radio. Three tests cover: happy 2-segment, single-segment short frame,
   and the negative `IncompleteReassembly` path where the last segment has
   `more-fragments=1` (caller dropped the connection mid-frame). We chose a
   new `TransportError::IncompleteReassembly` variant rather than reusing
   `Closed` because the upper-layer log marker is distinct (`bt.frame.dropped`
   vs `bt.unlock.closed`) and tests can match the variant by name without
   string-substring fragility.

4. **Suspend/resume hook.** SPEC §4.2 reliability mandates a restart on
   `org.freedesktop.login1.Manager.PrepareForSleep` true→false. A
   `run_suspend_resume_loop` consumes a `tokio::sync::mpsc::Receiver<bool>`
   (the DBus stream is injected, not opened by the loop) and increments an
   internal restart counter on every true→false transition by calling
   `restart()`. Production code wires the receiver to the actual logind DBus
   signal; tests construct an mpsc pair, push `true` then `false`, and assert
   the counter incremented. This is the canonical "test seam at the radio"
   from `/bt` Phase 3 applied to the suspend signal: we never wait on a live
   DBus daemon in CI. The DBus consumer task is built outside the loop so the
   loop body stays unit-testable in isolation.

5. **Adapter open error mapping.** The DoD mandates that a missing adapter
   become a typed error. `BlueZBtPeer::new` calls `bluer::Session::new()` then
   `session.adapter(adapter_id)`; the `NotFound` variant of `bluer::Error`
   maps to `TransportError::AdapterMissing { name }`. Every other `bluer::Error`
   maps to `TransportError::Backend { reason }` (a new opaque variant carrying
   the rendered upstream message — never the wrapped `bluer::Error` type, so
   `bluer` does not leak into the public error surface of this crate).

### Phase 1: Pin the wire-level surface (per `/bt` Phase 1)

**User Intent:** Lock down the rotating session UUID derivation and the
fragment header layout before writing code.

**Actions:**
- Direction: desktop → phone (advertised UUID); phone → desktop (GATT write).
- Transport: BLE GATT central; the desktop advertises a Service UUID.
- Service UUID: derived via `session_uuid_for(bond_key, minute)`. Same
  `(bond_key, minute)` always returns the same 16 bytes. Successive minutes
  yield distinct outputs (HKDF is deterministic but its expand step over
  different `info` produces uncorrelated outputs).
- MTU: target `MAX_BLE_MTU = 247` post-negotiation, fragment header
  `FRAGMENT_HEADER_LEN = 1` byte. High bit set ⇒ "more fragments follow".
- Frame format inside the reassembled payload is the S-002 v1 frame,
  unchanged. The fragment header is below the framing layer.

**Pain / Risk:**
- HKDF salt ambiguity. We document the choice explicitly:
  `Hkdf::<Sha256>::new(None, bond_key)` with the application label baked into
  the `info` parameter (`HKDF_INFO_SESSION_V1 || minute_be_bytes`).
- Endianness of `minute` could fork across host architectures.
  `i64::to_be_bytes` pins it to big-endian network order.
- MTU literals scattered across the file. Named constants only.

**Success Signal:** `session_uuid_for` exists, has a doc-comment that names the
HKDF formula and salt choice, and unit tests pass.

### Phase 2: PairingState consult

**User Intent:** Make the `/bt` non-negotiable rule executable: the unlock-path
never reads from `ProvisionalBonded`, and never reads at all if the peer is
`NotPaired`.

**Actions:**
- Add `pub enum PairingState { Bonded { peer_id: String }, NotPaired }`.
- Store the variant on `BlueZBtPeer`.
- Add `TransportError::NotPaired`.
- `BtPeer::connect` first statement: `match self.pairing_state { PairingState::NotPaired => return Err(TransportError::NotPaired), PairingState::Bonded { .. } => () }`.

**Pain / Risk:**
- A future maintainer might add a `ProvisionalBonded` variant and let it fall
  through `connect`. Mitigated by the exhaustive `match` and a one-line
  comment naming `/bt` Phase 2.
- Forgetting to consult before opening a bluer Session would leak side
  effects (DBus chatter) before the rejection. Mitigated by placing the
  check as the literal first statement.

**Success Signal:** `connect_rejects_when_not_paired` passes; no bluer call is
ever made on the NotPaired path.

### Phase 3: Reassembly

**User Intent:** Two-segment BLE writes reassemble into one logical frame.

**Actions:**
- Define `reassemble(segments: &[Vec<u8>]) -> Result<Vec<u8>, TransportError>`.
- For each segment: bounds-check, peel one-byte header, push payload.
- The last segment's header must have the high bit clear; earlier segments must
  have it set.

**Pain / Risk:**
- Empty segment slice: returns `IncompleteReassembly` (no last frame at all).
- Final segment with `more-fragments=1`: also `IncompleteReassembly`.
- A segment shorter than the header: `IncompleteReassembly` (cannot peel).

**Success Signal:** Three unit tests:
`reassemble_joins_two_segments_into_whole_frame`,
`reassemble_passes_single_segment_through`,
`reassemble_rejects_truncated_multi_segment`.

### Phase 4: Suspend / resume

**User Intent:** A desktop that goes to sleep with the phone in range wakes up
and the next unlock attempt works.

**Actions:**
- `restart()`: atomic counter increment (the test seam) plus, in production,
  re-open the bluer adapter handle. v0.1 increments the counter; the adapter
  re-open is layered in `run_suspend_resume_loop` itself (it borrows the
  shared `Arc<BlueZBtPeer>` and the production caller already owns the
  adapter open path through `BlueZBtPeer::new`).
- `run_suspend_resume_loop`: receives `bool` events; on `true` records the
  pre-sleep state; on `false` (immediately following a `true`) calls
  `restart()`.

**Pain / Risk:**
- Spurious `false` without a prior `true`: ignored (no restart).
- Burst of `true/false/true/false`: each completed true→false pair triggers
  one restart.
- Channel closed mid-loop: the loop terminates cleanly without panic.

**Success Signal:** `suspend_resume_restarts_transport` injects
`tx.send(true); tx.send(false);` and asserts `restart_count == 1`.

### Phase 5: Bluer-backed `connect`

**User Intent:** A real unlock against a real radio (smoke test only).

**Actions:**
- `BlueZBtPeer::new(adapter_id, &bond_key, pairing_state)` opens the adapter.
- `connect` (when `Bonded`): in v0.1 returns `TransportError::Backend { reason: "real-radio path not wired in S-010; see S-019" }`. The DoD does not require a working roundtrip against a real adapter — only that the trait is implemented, error mapping is typed, and the wire-level pieces (UUID, reassembly, suspend hook, PairingState consult) are correct. S-019 ("Full e2e on real radios") is the step that wires the actual challenge/response.
- `tests/bluer_smoke.rs` gated on `SYAUTH_E2E=1` attempts a `bluer::Session::new()` + `adapter("hci0")` and reports the powered state. Skips cleanly when the env var is unset.

**Pain / Risk:**
- A test that depends on a live BlueZ daemon would break `make test` on hosts
  without a Bluetooth adapter. The `SYAUTH_E2E=1` gate makes the test a no-op
  unless the operator explicitly opted in.
- Linking against `bluer` requires `libdbus-1-dev` system-side. CI containers
  provide it; the local Fedora dev host has it via `dbus-devel`. Documented
  in the smoke test top comment.

**Success Signal:** `cargo build -p syauth-transport` succeeds and the
gated smoke test compiles.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Operator unsure which adapter to use | 5 | Default to `hci0`, named `DEFAULT_ADAPTER_NAME` const so a future config-parser change is one-line. |
| Test depends on real radio | 5 | All testable pieces are pure functions (`session_uuid_for`, `reassemble`) or injected channels (`run_suspend_resume_loop`). |
| Bluer error surface leaks via `From` | All | `TransportError::Backend { reason: String }` so upstream type does not leak; the `bluer::Error` is rendered via `Display`. |

### North Star Summary

A Linux desktop with `pam_syauth.so` and a paired Android phone unlocks within
2 s when the user walks up; presence-tracking attackers cannot correlate
sessions of the same user across minutes; `ProvisionalBonded` and `NotPaired`
peers are rejected before the radio is even touched; suspending and resuming
the desktop does not require a manual unlock-and-re-pair dance.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] Constructor is one call: `BlueZBtPeer::new("hci0", &bond_key, PairingState::Bonded { peer_id })`.
- [x] No config parsing in this step; the adapter id is a function argument.

### Onboarding Clarity
- [x] `TransportError` variants name the failure (`AdapterMissing`, `NotPaired`, `IncompleteReassembly`, `Backend`).

### Production-Ready Defaults
- [x] `DEFAULT_ADAPTER_NAME = "hci0"` matches the SPEC default.
- [x] `SESSION_UUID_ROTATION_INTERVAL = 60s` matches the DoD.

### Golden Path Quality
- [x] `session_uuid_for` is deterministic per `(bond_key, minute)`.
- [x] `reassemble` accepts the 2-segment shape the DoD specifies.

### Decision Load
- [x] HKDF salt: `None`. Documented.
- [x] Suspend channel type: `tokio::sync::mpsc::Receiver<bool>`. Documented.

### Progressive Complexity
- [x] Real-radio `connect` body returns `Backend` rather than panicking; S-019 wires it.

### Error Quality
- [x] Every error variant has a `thiserror` `#[error("...")]` line that names the underlying cause.

### Failure Safety
- [x] `connect` never opens a radio when `NotPaired`.
- [x] `reassemble` returns a typed error on every malformed input, never panics.

### Runtime Transparency
- [x] `restart()` increments an atomic counter so tests and future tracing spans can observe.

### Debuggability
- [x] `Backend` carries the rendered `bluer::Error` message.

### Cross-Surface Consistency
- [x] `BlueZBtPeer` implements the same `BtPeer` trait `MockBtPeer` does (S-007 contract).

### Workflow Consistency
- [x] All new constants are named (no magic literals).
- [x] No `unwrap()` in production paths.

### Change Safety
- [x] No mutation of the S-007 trait surface.
- [x] No `TransportError` variant renamed; only additive (`NotPaired`, `AdapterMissing`, `IncompleteReassembly`, `Backend`).

### Experimentation Safety
- [x] `tests/bluer_smoke.rs` is gated on `SYAUTH_E2E=1`; default `make test` does not hit a radio.

### Interaction Latency
- [x] `connect` early-returns on `NotPaired` in O(1) — no I/O.

### Developer Feedback Speed
- [x] All unit tests are pure-CPU; runtime under 1 second.

### Team Scale
- [x] `bluer` is pinned to major version `0.17`; minor revs flow via Cargo.lock.

### System Scale
- [x] Fragment reassembly bound by `MAX_BLE_MTU`; oversized inputs are rejected, not silently truncated.

### Right Behavior by Default
- [x] `connect` defaults to refusing unknown peers (only `Bonded` proceeds).

### Anti-Bypass Design
- [x] The PairingState consult is the literal first statement of `connect`; no path bypasses it.

## 4. Tests

### TC-01: `session_uuid_for_is_deterministic_per_minute`
**Given** a fixed `bond_key` and a fixed `minute`.
**When** `session_uuid_for` is called twice with those inputs.
**Then** both calls return the same 16-byte UUID.

### TC-02: `session_uuid_for_rotates_each_minute`
**Given** a fixed `bond_key` and three successive minutes (M, M+1, M+2).
**When** `session_uuid_for` is called for each minute.
**Then** the three outputs are pairwise distinct.

### TC-03: `reassemble_joins_two_segments_into_whole_frame`
**Given** two segments: `[0x80, A, B]` (more-fragments=1) and `[0x00, C, D]`
(more-fragments=0).
**When** `reassemble(&[seg0, seg1])` is called.
**Then** the result is `[A, B, C, D]`.

### TC-04: `reassemble_passes_single_segment_through`
**Given** one segment `[0x00, X, Y, Z]`.
**When** `reassemble(&[seg0])` is called.
**Then** the result is `[X, Y, Z]`.

### TC-05: `reassemble_rejects_truncated_multi_segment`
**Given** one segment `[0x80, X]` (more-fragments=1 but no follow-up).
**When** `reassemble(&[seg0])` is called.
**Then** the result is `Err(TransportError::IncompleteReassembly)`.

### TC-06: `reassemble_rejects_empty_segment_slice`
**Given** an empty slice of segments.
**When** `reassemble(&[])` is called.
**Then** the result is `Err(TransportError::IncompleteReassembly)`.

### TC-07: `connect_rejects_when_not_paired`
**Given** a `BlueZBtPeer` constructed with `PairingState::NotPaired`.
**When** `connect(Duration::from_millis(10))` is awaited.
**Then** the result is `Err(TransportError::NotPaired)` and the production
adapter handle is not initialized (verified by the test using a constructor
that never opens a real adapter for the NotPaired arm).

### TC-08: `suspend_resume_restarts_transport`
**Given** a `BlueZBtPeer` (NotPaired so no real adapter is opened) and a
`tokio::sync::mpsc::channel::<bool>(2)`.
**When** the test sends `true` then `false`, then closes the channel and
awaits the loop.
**Then** the peer's restart counter is 1.

### TC-09: `adapter_missing_maps_to_typed_error` (unit, error mapping)
**Given** the helper `map_bluer_error(notfound)` is called with a synthesized
`bluer::Error` whose kind indicates "not found".
**When** the mapping runs.
**Then** the result is `TransportError::AdapterMissing { name }` with the
expected adapter name preserved.

### TC-10: `bluer_smoke_skips_without_env` (integration, gated)
**Given** `SYAUTH_E2E` is unset.
**When** `tests/bluer_smoke.rs` is run.
**Then** the test exits 0 with one log line "skipped: SYAUTH_E2E != 1" and
no DBus call is made.

## Traceability
- Roadmap item: [specs/syauth/ROADMAP.md §S-010](../syauth/ROADMAP.md)
- Implementation files: `crates/syauth-transport/src/bluez.rs`,
  `crates/syauth-transport/src/error.rs` (new variants),
  `crates/syauth-transport/src/lib.rs` (re-export),
  `crates/syauth-transport/Cargo.toml` (new deps),
  `tests/bluer_smoke.rs`.
- Test files: `crates/syauth-transport/src/bluez.rs::tests`,
  `tests/bluer_smoke.rs`.

[bluer]: https://docs.rs/bluer
