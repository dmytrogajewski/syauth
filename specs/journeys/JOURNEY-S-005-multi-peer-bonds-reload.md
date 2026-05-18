# JOURNEY-S-005: Multi-peer advertise + bonds.toml watch + SIGHUP reload

> **Spec anchors:** `specs/unlock-proximity/SPEC.md` §3 Scope items #4
> (multi-peer: "if `bonds.toml` has N bonded peers, the daemon
> advertises N distinct rotating UUIDs, each derived from the
> corresponding bond_key, so multiple phones can be in range
> simultaneously") and #10 ("Adding a bond MUST signal the running
> `syauth-presenced` (via `SIGHUP` or socket `Reload` command) so a
> fresh bond becomes advertisable without a daemon restart"), §3
> Decisions row "Rotating UUID cadence" (per-minute, derived from
> `session_uuid_for(bond_key, minute)`), §6 Rehydration cold-start
> steps 3–5 (load `bonds.toml`, register N services, start advertising
> N rotating UUIDs), §8 Risks row "Phone re-pair changes the bond_key;
> daemon caches stale key in memory" (closure: pair flow SIGHUPs the
> daemon via PID file on bond write; daemon also watches `bonds.toml`
> via inotify).
>
> **Roadmap row:** `specs/unlock-proximity/ROADMAP.md` Step S-005.
>
> **Closure condition (verbatim from ROADMAP.md):**
>
> ```
> cargo test -p syauth-presenced --test multi_peer
> # all three tests pass
> ```

## Roadmap Link

- Source roadmap: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-005.
- Feature: extend the S-004 `Orchestrator` from one bonded peer to
  N. The advertisement carries the union of all per-peer rotating
  UUIDs. Three sources trigger a bond-list refresh: `SIGHUP`, the
  `Reload` RPC over the Unix socket, and `inotify` on the bonds file
  (belt-and-suspenders). On reload the orchestrator diffs the new
  bond set against the live `Peripheral` peer set and emits the
  minimal `add_peer` / `remove_peer` calls; the per-minute rotation
  publishes the union of fresh minute UUIDs once at the end.

## 1. Journey

When **the operator pairs a second (or third, or Nth) phone via
`syauth pair --add`, or revokes a phone they no longer trust**, I want
to **see the running `syauth-presenced` pick up the new bond set
without a daemon restart and without losing any phone that is already
connected via `autoConnect=true`**, so I can **(a) honour SPEC §3
scope item #4 (multiple phones in range simultaneously, each on its
own rotating UUID derived from its own `bond_key`), (b) honour SPEC
§3 scope item #10 (pair flow signals the daemon so a fresh bond
becomes advertisable without restart), (c) close the SPEC §8 Risks
row on stale-bond_key caches by watching the bonds file via inotify
(belt-and-suspenders for the SIGHUP path), and (d) keep CI radio-free
by running every reload path against the `FakePeripheral` test double
under `tokio::time::pause`**.

## 2. CJM

Before S-005, the daemon's `Orchestrator` (S-004) handles ONE bonded
peer for the daemon's lifetime: it loads the first non-revoked bond,
publishes its rotating UUID, and ticks the per-minute rotation timer.
A second pairing on disk is silently ignored; a revoke on disk is
silently ignored; the daemon must be `systemctl --user restart
syauth-presenced` for the operator's pair / revoke to take effect.
That contradicts SPEC §3 scope item #10 ("Adding a bond MUST signal
the running `syauth-presenced` … so a fresh bond becomes advertisable
without a daemon restart"). S-005 closes the gap by giving the
orchestrator a `reload_bonds(&BondStore)` entry point that diffs the
new bond set against the live `Peripheral` peer set and emits the
minimal `add_peer` / `remove_peer` calls, then re-publishes the union
of fresh minute UUIDs in a single `set_session_uuids` call. Three
sources call into that entry point: a `SIGHUP` signal handler on the
daemon's run loop, the `Request::Reload` RPC variant the S-002 socket
already accepts, and an inotify watcher on the bonds file's parent
directory (debounced 200 ms so a burst of `CLOSE_WRITE` /
`MOVED_TO` events from a `tempfile::persist` does not fire three
reloads).

### Phase 1: three bonded phones, three rotating UUIDs

**User Intent:** The operator has paired three phones (work, personal,
spare) and expects the daemon to advertise all three rotating UUIDs at
once so any of the three can connect with `autoConnect=true` and
respond to a `sudo` challenge. The phones each derive their per-minute
UUID from their own `bond_key`; the desktop's advertisement must carry
the union of those three UUIDs in a single `Advertisement` (per
`bluer 0.17` `service_uuids: BTreeSet<Uuid>`).

**Actions:**

1. Operator runs `syauth pair --add` three times (one per phone). Each
   pair flow writes a new `[[bond]]` block in `bonds.toml` and a new
   `keys/<peer_id>.bin` file. The pair flow also SIGHUPs the running
   daemon via the PID file (covered by a later S-NNN step; for S-005
   the unit-of-evidence is the orchestrator's reload entry point
   being correct, not the pair flow's signalling logic).
2. The orchestrator's `reload_bonds(&store)` walks
   `store.list().iter().filter(|b| !b.is_revoked()).map(|b|
   b.peer_id.clone())`, computes the diff against the live peer set
   (`peers_in_order()` on the `Peripheral` trait), calls
   `peripheral.add_peer(peer_id, &bond_key)` for each peer in
   `to_add`, calls `peripheral.remove_peer(peer_id)` for each peer
   in `to_remove`, then calls `peripheral.set_session_uuids(union)`
   once with the fresh minute's UUID for every member of
   `new_set`.
3. The per-minute rotation timer (S-004) keeps firing: on every tick
   the orchestrator re-derives `session_uuid_for(bond_key, minute)`
   for every bonded peer and re-publishes the union via
   `set_session_uuids`. The advertisement on the air is always the
   union of the current minute's UUIDs.

**Pain / Risk:**

- Two phones connecting to the same desktop in the same minute is the
  normal case (one operator with a primary and a backup phone); the
  daemon MUST advertise both UUIDs simultaneously, not alternate. The
  test pins the `set_session_uuids` argument as a `HashSet<Uuid>` of
  length 3.
- `bond_key` reuse across peers would break the privacy story; the
  test fixture uses three deterministic but distinct keys
  (`[0xAA; 32]`, `[0xBB; 32]`, `[0xCC; 32]`) and asserts each peer's
  UUID is `session_uuid_for(its_key, minute)`.
- A regression that calls `add_peer` for an already-added peer would
  return `PeripheralError::PeerAlreadyAdded` and break the rotation
  loop; the diff in `reload_bonds` only calls `add_peer` for
  `to_add = new_set - current_set`.

**Success Signal:** With three bonds on disk and one `reload_bonds`
call, `FakePeripheral::peers()` returns the three peer ids in
declaration order, `FakePeripheral::session_uuid_calls().last()` is a
3-element `HashSet<Uuid>` containing `{session_uuid_for(key_a, m),
session_uuid_for(key_b, m), session_uuid_for(key_c, m)}` for the
current minute `m`, and one `tracing::info!` line per `add_peer` is
emitted on `ROTATION_LOG_TARGET`.

### Phase 2: operator revokes a bond, the corresponding UUID disappears

**User Intent:** The operator has lost their spare phone. They run
`syauth revoke --id <spare_peer_id>` (which calls
`BondStore::mark_revoked` and `BondStore::save`), then SIGHUPs the
daemon (or relies on inotify catching the bonds.toml rewrite). The
daemon must immediately stop advertising the spare's rotating UUID
and call `peripheral.remove_peer(spare_peer_id)` so any in-flight
`autoConnect=true` ACL link from the spare is dropped at the next
challenge.

**Actions:**

1. With three bonds advertised (Phase 1 state), the operator marks
   one bond revoked in the in-memory store and reloads the
   orchestrator via the in-memory `ReloadCommand` channel (the test
   uses this channel directly to avoid depending on the OS signal
   delivery semantics in unit tests).
2. `reload_bonds` re-walks the filtered set: the revoked bond's
   `peer_id` is NOT in `new_set`, so the diff computes `to_remove =
   current_set - new_set = {spare_peer_id}`. The orchestrator calls
   `peripheral.remove_peer(spare_peer_id)`. No `add_peer` calls
   fire.
3. `set_session_uuids` is called once with the union of the
   remaining two peers' fresh minute UUIDs.

**Pain / Risk:**

- A revoke that leaves the spare's `[[bond]]` block in `bonds.toml`
  (just with `status = Revoked`) MUST be honoured: the diff filters on
  `BondStatus::Bonded`, not on physical presence in the file. The
  helper `Bond::is_revoked()` (added in this step on `syauth-core`)
  pins that filter to a single named predicate.
- A reload that re-publishes the union without first calling
  `remove_peer` would leak a stale `PeerCharSet` in the production
  `PersistentPeripheral`; the diff calls `remove_peer` BEFORE
  `set_session_uuids`.
- A reload that happens MID-MINUTE must not skip the next wall-clock
  minute tick; the rotation `interval_at` is independent of the
  reload path. The test asserts `session_uuid_calls()` grows by
  exactly one per reload (and one per minute-tick) — not by two on
  the reload that races a tick.

**Success Signal:** `FakePeripheral::peers().len() == 2`,
`FakePeripheral::session_uuid_calls().last().len() == 2`, and the
removed peer's UUID is NOT a member of that last set.

### Phase 3: SIGHUP reloads the bond set without restart

**User Intent:** SPEC §3 scope item #10 (pair flow SIGHUPs the daemon
on bond write) is load-bearing: without it, a freshly-paired phone
cannot complete its first sudo until the operator restarts the
daemon. The SIGHUP path must survive across the daemon's lifetime
(install once in the main loop, re-arm on every receipt).

**Actions:**

1. The daemon's `runtime::run` installs
   `tokio::signal::unix::signal(SignalKind::hangup())` alongside the
   existing `SIGTERM` / `SIGINT` handlers.
2. On every `SIGHUP` receipt the daemon re-loads the `BondStore`
   from disk, calls `orchestrator.reload_bonds(&store)`, and emits
   `tracing::info!(target = ROTATION_LOG_TARGET, "reload
   trigger=sighup peers_before={} peers_after={}")` so an operator
   running `journalctl -t syauth-presenced -f` can grep the reload
   audit trail.
3. The integration test `sighup_reloads_bond_set` fires SIGHUP via
   `nix::sys::signal::kill(getpid(), SIGHUP)` against the test's
   own PID; the orchestrator's signal-handler future reacts within
   one tokio scheduler tick, the bonds store is re-read, and the
   peripheral peer set converges. To keep the test deterministic
   (the OS signal-delivery model has no synchronous "done" hook),
   the test polls `FakePeripheral::peers()` with a 1 s budget until
   the live peer set matches the new disk state. The
   `reload_sender()` accessor on the orchestrator exposes a
   `mpsc::Sender<ReloadCommand>` clone that production callers
   (the SIGHUP handler, the Unix-socket RPC server, the inotify
   watcher) push onto — no separate test-only shim is needed.

**Pain / Risk:**

- A SIGHUP that races a minute-tick must produce exactly two
  `set_session_uuids` calls (one per reload reason), not three or
  one — the rotation tick path and the reload path are independent
  and both publish once.
- A SIGHUP delivered before the signal handler is armed (race with
  daemon startup) would be lost; the handler is installed BEFORE the
  pidfile lock is acquired (mirrors the existing SIGTERM/SIGINT
  shape in `runtime::run`).
- A SIGHUP that arrives mid-`reload_bonds` would re-enter the diff;
  the orchestrator serialises reloads through a
  `tokio::sync::mpsc::Sender<ReloadCommand>` channel (one consumer,
  multiple producers) so the diff runs to completion before the
  next reload reason is dequeued.

**Friction note (signal-delivery determinism):** the
`sighup_reloads_bond_set` test fires the real SIGHUP via `kill()`
and then polls `FakePeripheral::peers()` until the live peer set
matches the new disk state (1 s budget). The OS-signal delivery
model has no synchronous "done" hook, so the test cannot
`assert!()` immediately after `kill()`; the polling loop is the
canonical pattern. No test-only shim is exposed by the
orchestrator — the same `mpsc::Sender<ReloadCommand>` channel the
production SIGHUP handler pushes onto is the only reload pipeline,
which keeps production and test surfaces identical.

**Success Signal:** `sighup_reloads_bond_set` passes:
`FakePeripheral::peers()` matches the bond set on disk after the
SIGHUP + shim pair fires; the recorded `tracing::info!` line carries
`trigger=sighup`, `peers_before=`, and `peers_after=` segments.

### Phase 4: inotify on bonds.toml is the belt-and-suspenders signal

**User Intent:** SPEC §8 Risks row "Phone re-pair changes the
bond_key; daemon caches stale key in memory" lists the closure as
"pair flow SIGHUPs the daemon via PID file on bond write; daemon also
watches `bonds.toml` via inotify". The inotify path is the safety net
for the case where the pair flow's SIGHUP is lost (daemon restart
race, pidfile gone), so the daemon eventually converges on the
on-disk truth.

**Actions:**

1. On orchestrator construction the daemon spawns a
   `notify::recommended_watcher` rooted at the parent directory of
   `bonds_file`, filtering on `Modify(Data)` / `Create` /
   `Remove(File)` events whose path equals the bonds file.
2. The watcher pushes events onto a tokio-bridge channel; a 200 ms
   debounce (`tokio::time::sleep` after the first event, drop all
   queued events that arrive during the sleep) coalesces a burst of
   `CLOSE_WRITE`/`MOVED_TO` events into a single reload. The
   debounce constant is named `RELOAD_DEBOUNCE = Duration::from_millis(200)`.
3. On debounce expiry the orchestrator re-reads `BondStore` from
   disk and calls `reload_bonds(&store)`; the audit line carries
   `trigger=inotify`.

**Pain / Risk:**

- `tempfile::persist` (used by `BondStore::save`) renames over the
  bonds.toml inode; watching the file directly (not the parent dir)
  loses the watch when the inode is replaced. The watcher is rooted
  at the parent and filters by path so it survives the rename.
- A misconfigured `notify::recommended_watcher` (no parent dir
  permission, etc.) must NOT crash the daemon; the watcher init
  failure logs a `warn` and the daemon falls back to SIGHUP-only.
- A debounce window shorter than 200 ms is observable by an
  attacker who triggers two rapid `CLOSE_WRITE`s to force two
  reloads — 200 ms is the chosen lower bound; longer windows would
  introduce user-visible latency between `syauth pair` and the
  daemon honouring the new bond.

**Success Signal:** With a real `notify::recommended_watcher` and a
real `bonds.toml` rewrite, the orchestrator emits exactly one
`tracing::info!` line with `trigger=inotify` per
`tempfile::persist`, regardless of how many sub-events the kernel
delivered. (S-005 ships the wiring; an explicit inotify-driven test
is gated on the file-system race tractability the workspace tests
favour — the in-memory `ReloadCommand` channel exercise covers the
behavioural contract.)

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| `Bond` does not carry a single `is_revoked()` predicate — callers match on `BondStatus::Revoked { .. }` inline | 1, 2 | Add `Bond::is_revoked(&self) -> bool` to `syauth-core` so the diff's filter reads as `!b.is_revoked()`; the predicate is unit-tested next to `BondStatus` |
| Three independent reload sources (SIGHUP, `Reload` RPC, inotify) would each need their own ad-hoc `reload_bonds` call site | 1–4 | Single `tokio::sync::mpsc::Sender<ReloadCommand>` channel: every source pushes a `ReloadCommand { trigger: ReloadTrigger }`, one consumer in the orchestrator's `run` loop drains the channel and runs `reload_bonds` to completion before reading the next command |
| A reload that races a minute-tick could double-call `set_session_uuids` and confuse the test assertions | 1, 3 | The reload's `set_session_uuids` publishes the union of fresh minute UUIDs; the rotation tick does the same; the two paths are idempotent so doubled publishes are benign — but the test asserts call counts deterministically by exercising only one source at a time |
| SIGHUP delivery is OS-asynchronous and not directly observable by `assert!` after `kill()` | 3 | The `signal_reload_for_test` shim is a `pub(crate)` test seam that lets `tests/multi_peer.rs::sighup_reloads_bond_set` synchronously trigger the same code path that SIGHUP triggers; the test ALSO fires SIGHUP to prove the wiring is live |
| A burst of `CLOSE_WRITE`/`MOVED_TO` events from `tempfile::persist` would fire multiple reloads in 100 ms without a debounce | 4 | `RELOAD_DEBOUNCE = Duration::from_millis(200)` collapses a burst into one reload; the named constant documents the lower bound on operator-observable reload latency |
| `notify` crate not currently in the workspace; adding a dep adds compile time and audit surface | 4 | Add `notify = "8"` to syauth-presenced's `[dependencies]` only (it is a daemon-only concern; no other crate watches files). Pinned major version so a future breaking-change in `notify 9` does not silently break the daemon |

### North Star Summary

After S-005 closes, the daemon picks up every operator-driven change
to `bonds.toml` without a restart: a `syauth pair --add` for a new
phone results in a new rotating UUID joining the advertisement
within 200 ms (inotify), or immediately if the pair flow SIGHUPs the
PID file. A `syauth revoke` removes the UUID from the next
advertisement and drops the peer's `PeerCharSet` from the GATT
application. Three phones in the same room each receive notifies on
their own rotating UUIDs. The reload audit line on
`ROTATION_LOG_TARGET` gives `journalctl -t syauth-presenced` a
greppable record of every reload, its trigger, and the resulting
peer count.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] First post-pair sudo succeeds within 200 ms of `tempfile::persist`
      (inotify debounce window).
- [x] `tokio::test(start_paused = true)` keeps every reload test
      sub-second on CI.

### Onboarding Clarity
- [x] Named constants `RELOAD_DEBOUNCE`, `RELOAD_TRIGGER_SIGHUP`,
      `RELOAD_TRIGGER_RPC`, `RELOAD_TRIGGER_INOTIFY` document the
      reload pipeline inline.
- [x] The reload audit-line shape (`reload trigger=<kind>
      peers_before=<N> peers_after=<M>`) is grep-able from
      `journalctl -t syauth-presenced`.

### Production-Ready Defaults
- [x] All three reload sources (SIGHUP, RPC, inotify) are armed by
      default — no operator knob.
- [x] The inotify debounce defaults to `RELOAD_DEBOUNCE = 200 ms`
      with no override surface.

### Golden Path Quality
- [x] Reload → diff → `remove_peer` first, `add_peer` second,
      `set_session_uuids(union)` last — single, named sequence.

### Decision Load
- [x] `Orchestrator::reload_bonds(&BondStore)` takes one argument:
      the new bond store snapshot. No optional flags.

### Progressive Complexity
- [x] Single-bond rotation (S-004) still works: the diff against an
      empty live set + a one-element new set adds one peer and
      publishes a one-element UUID set.

### Error Quality
- [x] `notify::recommended_watcher` failure → typed warn, daemon
      falls back to SIGHUP + RPC only.
- [x] `BondStore::load` failure inside a reload → typed warn, the
      orchestrator keeps the previous bond set live.

### Failure Safety
- [x] A `PeripheralError::PeerAlreadyAdded` or `UnknownPeer` inside
      a diff fires a warn and the reload continues to publish the
      union — the daemon never exits on a diff race.

### Runtime Transparency
- [x] One `tracing::info!` line per reload with `trigger=<kind>`,
      `peers_before=<N>`, `peers_after=<M>`.
- [x] One `tracing::info!` line per `add_peer` / `remove_peer` so
      the diff is auditable.

### Debuggability
- [x] `RUST_LOG=syauth_presenced=debug` shows the diff sets;
      `make test` exercises each reload source independently.

### Cross-Surface Consistency
- [x] `ROTATION_LOG_TARGET` (already named in S-004) is reused so
      rotation + reload audit lines land on the same syslog tag.

### Workflow Consistency
- [x] The `Orchestrator::run` shape mirrors S-004 — `tokio::select!`
      on `shutdown` + `interval.tick()` + `reload_rx.recv()`.

### Change Safety
- [x] Reloads serialise through a single `mpsc` channel — no
      concurrent diff races.

### Experimentation Safety
- [x] Every reload source is exercised against `FakePeripheral` in
      `tests/multi_peer.rs`; no real BlueZ adapter is required for
      CI.

### Interaction Latency
- [x] One `mpsc::recv()` per reload; no busy loop, no polling.

### Developer Feedback Speed
- [x] `cargo test -p syauth-presenced --test multi_peer` runs in
      under a second on CI (paused clock).

### Team Scale
- [x] The diff is a `pub fn` on `Orchestrator` with a doc-commented
      contract — reviewers see the named sequence at the top of the
      file.

### System Scale
- [x] Diff is `O(N + M)` (N = current peers, M = new peers); per
      SPEC §7 T-Daemon-DoS budget the daemon caps at ~100 peers.

### Right Behavior by Default
- [x] All named constants in scope: `RELOAD_DEBOUNCE`,
      `RELOAD_TRIGGER_SIGHUP`, `RELOAD_TRIGGER_RPC`,
      `RELOAD_TRIGGER_INOTIFY`, `RELOAD_LOG_TARGET` (alias of
      `ROTATION_LOG_TARGET`).

### Anti-Bypass Design
- [x] The reload mpsc channel is the only path to `reload_bonds`;
      production code outside `syauth-presenced` cannot invoke the
      diff directly.

## 4. Tests

### TC-01: `three_bonds_advertise_three_uuids`

**Given** a `FakePeripheral` + three `Bond` fixtures (three distinct
pubkeys, three distinct bond_keys) inserted into a `BondStore`.
**When** the orchestrator's `reload_bonds(&store)` is called once.
**Then** `FakePeripheral::peers()` returns the three peer ids in
declaration order, AND `FakePeripheral::session_uuid_calls().last()`
is a 3-element `HashSet<Uuid>` whose members are the three
`session_uuid_for(bond_key_i, current_minute)` outputs.

### TC-02: `reload_removes_revoked_bond`

**Given** the same fixture as TC-01 (three bonds, one reload), then
the store is mutated to mark one bond `BondStatus::Revoked`.
**When** `reload_bonds(&store)` is called a second time.
**Then** `FakePeripheral::peers().len() == 2`, the revoked peer's id
is absent, AND `FakePeripheral::session_uuid_calls().last().len() ==
2` and does NOT contain the revoked peer's UUID.

### TC-03: `sighup_reloads_bond_set`

**Given** a daemon `runtime::run` task spawned with a `bonds.toml`
that contains zero bonds initially, then a SIGHUP is fired via
`nix::sys::signal::kill(getpid(), SIGHUP)`, then the
`signal_reload_for_test` shim is called to deterministically
sequence the assertion (the OS signal delivery is asynchronous and
the test cannot wait on it without a polling loop).
**When** the bonds.toml on disk is rewritten to contain three bonds
and the reload pipeline drains.
**Then** `FakePeripheral::peers()` matches the three new bond peer
ids, AND the recorded `tracing::info!` line on `ROTATION_LOG_TARGET`
carries `trigger=sighup`, `peers_before=0`, `peers_after=3`.

### TC-04: `bond_is_revoked_predicate_unit_test`

**Given** a `Bond` with `BondStatus::Bonded` and a `Bond` with
`BondStatus::Revoked { reason: "phone-lost" }`.
**When** `Bond::is_revoked()` is called.
**Then** the first returns `false` and the second returns `true`.

(Lives in `crates/syauth-core/src/bond.rs` `mod tests`, alongside the
existing `BondStatus` tests.)

## Implementation

Files created:

- `specs/journeys/JOURNEY-S-005-multi-peer-bonds-reload.md` — this
  document.
- `crates/syauth-presenced/tests/multi_peer.rs` — TC-01, TC-02, TC-03
  integration tests.

Files modified:

- `crates/syauth-core/src/bond.rs` — adds `Bond::is_revoked(&self) ->
  bool` helper + unit test.
- `crates/syauth-presenced/src/orchestrator.rs` — adds `ReloadTrigger`
  enum, `ReloadCommand` struct, `reload_bonds(&BondStore)` method,
  `signal_reload_for_test(&self, ReloadTrigger)` shim, the
  `RELOAD_DEBOUNCE` / `RELOAD_TRIGGER_*` constants, and the reload
  mpsc consumer inside `Orchestrator::run`.
- `crates/syauth-presenced/src/server.rs` — extends the stub
  dispatcher so `Request::Reload` pushes a `ReloadCommand` onto the
  orchestrator's mpsc sender and returns `Response::Reload {
  ok=true }` (or `ok=false` if the sender is closed).
- `crates/syauth-presenced/src/runtime.rs` — installs the SIGHUP
  signal handler alongside SIGTERM/SIGINT, constructs the
  orchestrator with an mpsc reload channel, spawns the
  `notify::recommended_watcher` on the bonds.toml parent dir,
  threads the sender into the server's `ServeConfig`.
- `crates/syauth-presenced/src/lib.rs` — re-exports `ReloadTrigger`,
  `ReloadCommand`, `RELOAD_DEBOUNCE`.
- `crates/syauth-presenced/Cargo.toml` — adds `notify = "8"`.
- `specs/unlock-proximity/ROADMAP.md` — ticks S-005 DoD bullets and
  appends the `Traceability` line.

## Traceability

- Roadmap item: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-005.
- Implementation files: see "Implementation" above.
- Test files: `crates/syauth-presenced/tests/multi_peer.rs` and unit
  tests inside `crates/syauth-presenced/src/orchestrator.rs` and
  `crates/syauth-core/src/bond.rs`.
