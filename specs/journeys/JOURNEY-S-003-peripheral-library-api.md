# JOURNEY-S-003: Extract BLE peripheral library API from `BluerAdvertiser`

> **Spec anchors:** `specs/unlock-proximity/SPEC.md` §3 Approach (the
> daemon as the long-lived owner of the BlueZ peripheral role), §4
> Architecture "Modules affected" (the line that names
> `crates/syauth-transport/src/bluez_advertise.rs` as the file to
> refactor into a library the daemon consumes), and §4 Architecture
> diagram (the daemon's long-lived `bluer::gatt::local::Application` +
> `bluer::adv::Advertisement`).
>
> **Roadmap row:** `specs/unlock-proximity/ROADMAP.md` Step S-003.
>
> **Closure condition (verbatim from ROADMAP.md):**
>
> ```
> cargo test -p syauth-transport --test peripheral_contract
> # both tests pass
> git grep -l "BluerAdvertiser" crates/syauth-pam/   # still 1+ files (PAM still uses it)
> ```

## Roadmap Link

- Source roadmap: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-003.
- Feature: the reusable `Peripheral` trait + the `PersistentPeripheral`
  production implementation over `bluer 0.17` + the `FakePeripheral`
  test double the daemon's later steps (S-004..S-007) can drive
  without a radio. The existing `BluerAdvertiser` / `BluerAdvertiseSession`
  used by `pam_syauth` today (`crates/syauth-pam/src/auth.rs:575`)
  remains byte-for-byte unchanged — S-009 deletes that call site, not
  S-003.

## 1. Journey

When **the daemon (`syauth-presenced`) needs to hold the BLE peripheral
role across many `pam_sm_authenticate` calls instead of registering
and tearing down a `bluer::gatt::local::Application` per call**, I
want to **construct a single `PersistentPeripheral` at daemon startup,
add bonded peers to it as the bond store is loaded, replace its
advertised service-UUID set on each wall-clock minute boundary, and
push challenge frames at named peers over the same long-lived GATT
application**, so I can **(a) hit the SPEC §4.3 unlock-latency budget
because the phone's `autoConnect=true` GATT client stays connected
across PAM calls and the challenge is just one notify roundtrip, (b)
unit-test every later daemon step (S-004..S-007) against
`FakePeripheral` on CI without a radio, and (c) keep the existing
short-burst `BluerAdvertiser` working until S-009 removes it
intentionally, so this refactor never breaks `pam_syauth` mid-stream**.

## 2. CJM

Before S-003, the only peripheral path on disk is
`BluerAdvertiser::connect_inner` in
`crates/syauth-transport/src/bluez_advertise.rs:221`: a single
per-call function that opens the adapter, registers the GATT
`Application`, starts the `Advertisement`, awaits ONE phone-side
subscribe + write, and drops both handles when the returned
`BluerAdvertiseSession` goes out of scope at the end of one PAM
call. That shape is structurally incompatible with the SPEC §3
approach — the daemon needs to hold the `ApplicationHandle` and the
`AdvertisementHandle` across many PAM calls so the phone's
`autoConnect=true` client never has to re-establish the L2CAP
connection. S-003 extracts the reusable pieces (the
`build_unlock_services` builder, the security flags, the rotating
service UUID slot) behind a small trait and adds a production impl
that owns the long-lived handles, without disturbing the existing
short-burst path the PAM module still calls. Tests for the new
surface stay radio-free via `FakePeripheral`. The next step S-004
wires `PersistentPeripheral` into the daemon's orchestrator;
S-008/S-009 remove the old `BluerAdvertiser` call site once the
daemon takes over.

### Phase 1: daemon constructs `PersistentPeripheral` at startup

**User Intent:** The daemon (later S-004) needs one long-lived
peripheral handle it owns for the lifetime of the user-systemd unit,
not one per PAM call.

**Actions:**
1. The daemon constructs a `PersistentPeripheral` bound to the
   configured BlueZ adapter (`hci0` by default) at startup.
2. It calls `set_session_uuids({uuid_for_minute_n})` to publish the
   current minute's rotating service UUID(s).
3. It loads `bonds.toml`, then calls `add_peer(peer_id, bond_key)`
   once per non-revoked bond so the peripheral's per-peer
   characteristic state is populated.

**Pain / Risk:**
- A non-existent adapter (`hci99`) must surface as
  `PeripheralError::AdapterMissing` so the daemon can report it via
  syslog + `sy syauth doctor` rather than panicking deep in the
  bluer stack.
- The trait must be `Send + Sync` so the daemon's tokio orchestrator
  can pass it through `Arc<dyn Peripheral>` to its per-peer
  challenge tasks — without `Send + Sync` the future-proof
  multi-task split in S-007 (per-peer backpressure semaphores) has
  to be redesigned.
- A `set_session_uuids` call before any peer is added must succeed
  (empty service set is legal at the bluer layer) so the daemon's
  cold-start sequence can rotate the advertisement before bonds are
  loaded.

**Success Signal:** `PersistentPeripheral::new(adapter_id).await`
returns `Ok(self)`; one `set_session_uuids` + N `add_peer` calls
return `Ok(())`; the daemon process is now a single owner of one
`AdvertisementHandle` + one `ApplicationHandle` + an in-memory map
of N `PeerCharSet` records.

### Phase 2: daemon adds a second bonded peer at runtime

**User Intent:** The operator pairs a second phone via `syauth pair`
while the daemon is already running. The new bond must become
addressable for the unlock channel without restarting the daemon
(SPEC scope item #10).

**Actions:**
1. `syauth pair` writes the new bond record to `bonds.toml` and
   signals the daemon (S-005's `SIGHUP` / `Reload` RPC).
2. The orchestrator (S-005) computes the diff against the live
   peripheral and calls `peripheral.add_peer(new_peer_id, new_bond_key)`.
3. The orchestrator recomputes the union of per-peer rotating UUIDs
   and calls `peripheral.set_session_uuids({union})`.

**Pain / Risk:**
- `add_peer` for a peer_id that already exists must be idempotent or
  return a typed error — silently double-adding would leak GATT
  service handles into the bluer Application across reload cycles.
- `set_session_uuids` must replace the advertised service-UUID set,
  not append to it; otherwise an old (now-invalid) UUID stays on the
  air and a passive observer can correlate it.
- `remove_peer` for an unknown peer must return a typed error so
  diffs against a stale snapshot fail loud, not silent.

**Success Signal:** A subsequent `notify_challenge(new_peer_id, frame)`
call delivers bytes to the new phone over the same long-lived GATT
application without the daemon ever tearing down the
`ApplicationHandle` for the existing bonded peer(s).

### Phase 3: tests run on CI without a real radio via `FakePeripheral`

**User Intent:** The daemon's later steps S-004..S-007 (rotation
timer, multi-peer diffing, challenge state machine, nonce LRU,
backpressure) need radio-free unit tests so CI stays green on
machines without a BlueZ adapter.

**Actions:**
1. A test constructs `FakePeripheral::new()`.
2. The test drives it via the `Peripheral` trait: `add_peer`,
   `set_session_uuids`, `notify_challenge`, `remove_peer`, in
   whatever order the scenario demands.
3. The test reads back a recorded sequence-of-events log to assert
   the daemon issued the expected calls.

**Pain / Risk:**
- A test that wants to assert "the daemon rotated the advertisement
  exactly three times, with this specific UUID sequence" needs the
  fake to record every `set_session_uuids` call in order, not just
  the latest one — otherwise the rotation test in S-004 has nothing
  to assert on.
- `FakePeripheral` must be `Send + Sync` like the production impl so
  `Arc<dyn Peripheral>` flows through the same daemon code paths
  unchanged.
- The fake must NOT leak into production builds; the cost of carrying
  a `Mutex<FakeState>` for an idle daemon is small but principle says
  test scaffolding stays behind `#[cfg(any(test, feature = "test-fake"))]`.

**Success Signal:**
`cargo test -p syauth-transport --test peripheral_contract` runs on a
CI worker with no BlueZ and no radio, and the two named scenarios
(`add_remove_peer_roundtrip`, `set_session_uuids_replaces_advertisement`)
both pass against `FakePeripheral`.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| `BluerAdvertiser` couples adapter open, application registration, advertisement, and per-call I/O into one constructor — the daemon cannot reuse any of it | 1 | Extract a small `Peripheral` trait with four named methods so the daemon owns the long-lived handles directly and `BluerAdvertiser` stays a sibling for the legacy per-PAM-call path |
| Today's per-call short-burst path uses one in-flight bluer `CharacteristicControl` stream — there is no per-peer routing | 2 | `PersistentPeripheral` keeps a `HashMap<peer_id, PeerCharSet>` keyed by stable peer id so `notify_challenge(peer_id, frame)` targets exactly one phone, with no cross-peer races |
| Unit-testing anything BLE-shaped today requires a radio or the existing `MockBtPeer` (which mimics the old per-call API, not the persistent one) | 3 | `FakePeripheral` records every call in order so rotation, multi-peer, and challenge-flow tests can assert on a sequence-of-events log |

### North Star Summary

The daemon constructs one `PersistentPeripheral` at startup, owns the
`AdvertisementHandle` + `ApplicationHandle` for the lifetime of the
user-systemd unit, rotates the advertised UUIDs on minute boundaries
without re-registering BlueZ resources, and pushes challenge frames at
named peers in O(notify) latency — and every step of that flow is
exercised on CI by `FakePeripheral` with zero radio dependency. The
existing `BluerAdvertiser` keeps working until S-009 removes it.

## 3. UX Implementation and Assessment

### Time to First Value
- [ ] Daemon constructor is sub-second on a healthy host (bluer
      adapter open is the only blocking I/O).
- [ ] `FakePeripheral::new()` is constant-time: tests start fast.

### Onboarding Clarity
- [ ] Trait has exactly four methods + a typed error enum; no
      hidden state.
- [ ] Error variants (`AdapterMissing`, `UnknownPeer`, `Backend`)
      name the operator's fix path.

### Production-Ready Defaults
- [ ] Trait methods are `async` so the daemon's tokio runtime owns
      every blocking call.
- [ ] `Send + Sync` bound so `Arc<dyn Peripheral>` is the natural
      sharing pattern.

### Golden Path Quality
- [ ] `PersistentPeripheral` flow:
      `new` → `set_session_uuids` → `add_peer` × N → `notify_challenge` works end-to-end.
- [ ] Closure-condition probes pass.

### Decision Load
- [ ] Daemon authors choose only the adapter id; the trait surface is
      otherwise opinionated.
- [ ] No `Builder` ceremony — flat constructor.

### Progressive Complexity
- [ ] Tests use `FakePeripheral`; the daemon swaps in
      `PersistentPeripheral` at deploy time. One trait, two impls.
- [ ] No `Box<dyn Any>` extension points; the trait is closed.

### Error Quality
- [ ] `PeripheralError::AdapterMissing { name }` quotes the offending
      adapter id so the operator can fix `hci99` → `hci0` without
      reading source.
- [ ] `UnknownPeer { peer_id }` names the id that was not found.

### Failure Safety
- [ ] The trait does not expose `Drop` semantics in the public API;
      tear-down is the impl's responsibility (RAII on bluer handles).
- [ ] `FakePeripheral` is `Send + Sync` so a panicking test does not
      poison other tests.

### Runtime Transparency
- [ ] Every call is one async method invocation — `tracing` spans
      attach naturally at the daemon layer.
- [ ] The fake records every call so a failing test can print the
      sequence of events.

### Debuggability
- [ ] `Debug` derived on the trait error type.
- [ ] `FakePeripheral` exposes a typed event-log enum so the
      assertion code reads like requirements.

### Cross-Surface Consistency
- [ ] `PeripheralError::Backend { reason }` follows the
      `TransportError::Backend` convention so logs read consistently.
- [ ] `BondKey` (alias) reuses `BOND_KEY_BYTES` from `syauth-transport`.

### Workflow Consistency
- [ ] Trait method order mirrors the daemon's lifecycle (`add_peer`,
      `remove_peer`, `set_session_uuids`, `notify_challenge`).
- [ ] Test names match the closure condition's verbatim test ids.

### Change Safety
- [ ] `BluerAdvertiser` and `BluerAdvertiseSession` public surface is
      byte-identical post-refactor — verified by the PAM module still
      compiling and `crates/syauth-pam` tests still passing.

### Experimentation Safety
- [ ] Fake-impl carries a process-local recorder, so adding a new
      test scenario does not require touching production code.

### Interaction Latency
- [ ] No internal queues, no background threads — every call returns
      as soon as bluer (or the fake) returns.

### Developer Feedback Speed
- [ ] `cargo test -p syauth-transport --test peripheral_contract`
      runs under 1 s on a fake-only test surface.

### Team Scale
- [ ] Trait + impl + fake all live in one file (`peripheral.rs`)
      under a `cfg(any(test, feature = "test-fake"))` gate on the fake
      so reviewers see the whole contract at once.

### System Scale
- [ ] `HashMap<peer_id, PeerCharSet>` scales linearly in bond count;
      the SPEC's "tens of bonded peers" budget is comfortable.

### Right Behavior by Default
- [ ] No `unwrap` / `expect` in production code (forbidden by
      AGENTS.md).
- [ ] Constants for adapter open timeouts, never magic numbers.

### Anti-Bypass Design
- [ ] The trait is `pub` but the production impl's internal field
      types stay `pub(crate)`-or-private so consumers cannot bypass
      `add_peer` to splice raw bluer state in.

## 4. Tests

### TC-01: `add_remove_peer_roundtrip` (FakePeripheral)

**Given** a fresh `FakePeripheral`.
**When** the test calls `add_peer("a", &key_a)`, `add_peer("b", &key_b)`,
`add_peer("c", &key_c)`, then `remove_peer("b")`.
**Then** `FakePeripheral::peers()` returns exactly `["a", "c"]` in
insertion order. This is the closure-condition test
`peripheral_contract::add_remove_peer_roundtrip`.

### TC-02: `set_session_uuids_replaces_advertisement` (FakePeripheral)

**Given** a fresh `FakePeripheral`.
**When** the test calls `set_session_uuids({uuid_a})`,
`set_session_uuids({uuid_b})`, `set_session_uuids({uuid_a, uuid_c})`.
**Then** `FakePeripheral::session_uuid_calls()` returns exactly those
three sets in that order — every call recorded, none dropped, none
merged. This is the closure-condition test
`peripheral_contract::set_session_uuids_replaces_advertisement`.

### TC-03: Trait is object-safe and `Send + Sync`

**Given** the `Peripheral` trait.
**When** a test stores `Arc<dyn Peripheral>` in a variable.
**Then** it compiles. The bound is load-bearing for the daemon's
later use of `Arc<dyn Peripheral>` to share the peripheral across
per-peer tasks.

### TC-04: `notify_challenge` on unknown peer returns typed error
       (FakePeripheral)

**Given** a `FakePeripheral` with no peers added.
**When** the test calls `notify_challenge("ghost", &[0xab])`.
**Then** the result is `Err(PeripheralError::UnknownPeer { peer_id: "ghost" })`.
Pins the contract the daemon's orchestrator relies on for diff-based
peer updates in S-005.

### TC-05: `remove_peer` on unknown peer returns typed error
       (FakePeripheral)

**Given** a `FakePeripheral` with peers `{a}` only.
**When** the test calls `remove_peer("b")`.
**Then** the result is `Err(PeripheralError::UnknownPeer { peer_id: "b" })`.
Same rationale as TC-04 — silent failures break the diffing
invariant.

### TC-06: Existing `BluerAdvertiser` API surface unchanged

**Given** the post-refactor `bluez_advertise.rs`.
**When** the existing `crates/syauth-pam` tests run.
**Then** they still pass. Mechanical proof: `cargo test -p syauth-pam`
exits 0; `git grep -l "BluerAdvertiser" crates/syauth-pam/` returns
at least one file.

## Implementation

Files created:

- `crates/syauth-transport/src/peripheral.rs` — defines the
  `Peripheral` trait, the `PeripheralError` enum, the
  `BondKey` type alias, the production `PersistentPeripheral`
  built on `bluer 0.17`, and the test `FakePeripheral` behind
  `#[cfg(any(test, feature = "test-fake"))]`.
- `crates/syauth-transport/tests/peripheral_contract.rs` — radio-free
  integration tests `add_remove_peer_roundtrip` and
  `set_session_uuids_replaces_advertisement` (plus TC-04 / TC-05
  negative-path coverage).

Files modified:

- `crates/syauth-transport/src/lib.rs` — adds `pub mod peripheral;`
  and re-exports `Peripheral`, `PeripheralError`, `PersistentPeripheral`,
  and `BondKey`.
- `crates/syauth-transport/Cargo.toml` — adds the `test-fake` feature
  flag so downstream daemon tests can opt into `FakePeripheral`.

## Traceability

- Roadmap item: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-003.
- Implementation files: see "Implementation" above.
- Test files: `crates/syauth-transport/tests/peripheral_contract.rs`
  and unit tests inside `crates/syauth-transport/src/peripheral.rs`.
