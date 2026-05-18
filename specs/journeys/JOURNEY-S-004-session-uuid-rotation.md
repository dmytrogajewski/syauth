# JOURNEY-S-004: Per-minute session-UUID rotation in the daemon

> **Spec anchors:** `specs/unlock-proximity/SPEC.md` §3 Decisions row
> "Rotating UUID cadence" (per-minute, derived from
> `session_uuid_for(bond_key, minute)`), §3 scope items #2 and #3
> (long-lived `bluer::gatt::local::Application` whose service UUID
> rotates on each wall-clock minute boundary), §6 Rehydration cold-start
> steps 5 and 6 (start advertising the N rotating UUIDs and start the
> wall-clock-minute rotation tokio timer), §7 T-Presence-Tracking
> (per-minute rotation is the privacy story).
>
> **Roadmap row:** `specs/unlock-proximity/ROADMAP.md` Step S-004.
>
> **Closure condition (verbatim from ROADMAP.md):**
>
> ```
> cargo test -p syauth-presenced --test rotation
> # both tests pass
> ```

## Roadmap Link

- Source roadmap: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-004.
- Feature: the daemon's `Orchestrator` — the first piece of
  `syauth-presenced` that holds a `Peripheral` open across its
  lifetime, loads ONE bond on cold start, and rotates the advertised
  `service_uuids` set on each wall-clock minute boundary using
  `session_uuid_for(bond_key, minute)`. SPEC §3.2 D8 single-bond case
  only; multi-peer is the next step (S-005).

## 1. Journey

When **the daemon (`syauth-presenced`) starts and a single
non-revoked bond exists on disk**, I want to **construct an
`Orchestrator` that owns one `Peripheral` handle for the lifetime of
the daemon, publishes that bond's current minute's rotating UUID
immediately, and then aligns a `tokio::time::interval_at` to the next
wall-clock minute boundary so every minute thereafter
`set_session_uuids({session_uuid_for(bond_key, minute)})` is called
exactly once with a `tracing::info!` line of the documented shape**, so
I can **(a) preserve the SPEC §7 T-Presence-Tracking privacy story
without hurting unlock latency (the persistent L2CAP bond survives
across rotations), (b) give every later S-NNN step (S-005 multi-peer,
S-006 challenge flow) a known-good rotation surface they can layer
onto without rewriting the timer, and (c) keep CI radio-free by
running the rotation tests against the `FakePeripheral` test double
under `tokio::time::pause` + `tokio::time::advance`**.

## 2. CJM

Before S-004, `syauth-presenced` is the S-001/S-002 shell: it locks
the pidfile, binds the Unix socket, and answers `Request::Challenge`
with a stub `not-implemented` response. No BlueZ work is happening,
no bond is loaded, and no advertisement is on the air. The phone's
`autoConnect=true` GATT client (S-010 onwards) has nothing to bind
to. S-004 is the smallest step that closes that gap for one bonded
peer: read the first non-revoked bond's `peer_id` + load its 32-byte
`bond_key` from `keys/<peer_id>.bin`, hand both to a fresh
`Orchestrator` that wraps an `Arc<dyn Peripheral>`, call
`set_session_uuids` once on construction so the advertised UUID is
correct before the operator's first `sudo`, then drive a tokio
`interval_at` aligned to the next wall-clock minute boundary so the
UUID rotates atomically every minute. The reload + multi-peer + diff
machinery is S-005's job; S-004 stops at "one peer, one timer, one
tracing line per tick".

### Phase 1: Daemon cold-starts and loads a single bond

**User Intent:** The daemon (running under
`systemctl --user start syauth-presenced` per JOURNEY-S-001) needs to
read `/var/lib/syauth/bonds.toml`, pick the FIRST non-revoked bond,
load its 32-byte `bond_key` from
`/var/lib/syauth/keys/<peer_id>.bin`, and hand both to a fresh
`Orchestrator`. If no bond exists yet (operator hasn't paired) the
daemon logs a warn and stays up without an orchestrator — the
S-001 lifecycle_smoke tests run with an empty bonds file and must
keep passing.

**Actions:**
1. `runtime::run` calls `BondStore::load(&config.bonds_file)`. Empty
   file or no file → log warn, skip orchestrator, continue with the
   S-001/S-002 socket loop.
2. If at least one non-revoked bond is present, read the first one's
   `peer_id` and load `<keys_dir>/<peer_id>.bin` (must be 32 bytes).
3. Construct an `Arc<FakePeripheral>` in tests, or a future
   `Arc<PersistentPeripheral>` in production (the production wire-up
   lands in a follow-on S-NNN step that adds the real BlueZ adapter
   open; S-004 ships the orchestrator + timer so tests are radio-free
   on CI).
4. Construct `Orchestrator::new(peripheral, bond, bond_key, start)`
   where `start` is `tokio::time::Instant::now() + align_to_next_minute(SystemTime::now())`.

**Pain / Risk:**
- Empty `bonds.toml` (S-001's tests, brand-new install) must not
  crash the daemon. The orchestrator is skipped in that case, not
  errored.
- `keys/<peer_id>.bin` length ≠ 32 bytes → typed error, daemon
  surfaces a warn and skips the orchestrator (the operator's fix
  path is `syauth pair --repair`, tracked elsewhere).
- A future maintainer who adds a real BlueZ open inside the
  orchestrator without a way to bypass it on test machines would
  break CI and the S-001 lifecycle_smoke — `Arc<dyn Peripheral>` as
  a constructor parameter pins the inversion-of-control surface in
  place.

**Success Signal:** With a valid one-bond fixture, the daemon logs
`syauth-presenced: rotated id=<peer> minute=<N> uuid=<short>` once
on startup (the "publish current UUID before first sudo" path) and
the test-injected `FakePeripheral::session_uuid_calls()` length grows
by exactly one.

### Phase 2: Wall-clock minute boundary triggers a rotation

**User Intent:** The advertised UUID rolls over on the next wall-clock
minute mark so a passive observer never sees a stable identifier for
more than ~60 s (SPEC §7 T-Presence-Tracking). The rotation must be
aligned to the minute, not "every 60 s from start" — otherwise two
daemons started at different times would advertise different UUIDs at
the same wall-clock instant, breaking phone-side rediscovery.

**Actions:**
1. Free function `align_to_next_minute(now: SystemTime) -> Duration`
   computes the offset from `now` to the next wall-clock second
   `s%60 == 0`. Unit-tested at the minute mark, mid-minute, and just
   before the mark.
2. `Orchestrator::run` builds a `tokio::time::interval_at(start, Duration::from_secs(SECONDS_PER_MINUTE))`
   where `start = Instant::now() + align_to_next_minute(SystemTime::now())`.
3. On each `interval.tick().await`, the orchestrator:
   - reads the current minute via `SystemTime::now().duration_since(UNIX_EPOCH).as_secs() / SECONDS_PER_MINUTE`
   - calls `session_uuid_for(&bond_key, minute as i64)`
   - calls `peripheral.set_session_uuids({uuid})` (one-element set,
     single-bond case)
   - emits `tracing::info!(target: ROTATION_LOG_TARGET, "rotated id={peer} minute={N} uuid={short}")`
     with `short = first 8 hex chars of the UUID`.

**Pain / Risk:**
- Wall-clock skew across hosts: the rotation is aligned to UTC
  unix-epoch seconds, so two daemons on different hosts that share
  the same bond rotate at the same instant.
- A second `tick()` firing too late (e.g. system suspend/resume)
  must NOT fire two ticks in succession with the same minute integer
  — `session_uuid_for` is keyed on the minute, so a skipped minute is
  fine; doubled output would just write the same UUID twice. The
  `tokio::time::pause` + `tokio::time::advance` test pins one tick
  per simulated minute exactly.
- `tracing::info!` with the wrong target string would never reach the
  audit grep; the named constant `ROTATION_LOG_TARGET` is checked by
  the second test.

**Success Signal:**
`cargo test -p syauth-presenced --test rotation --
rotates_at_minute_boundary` passes: after `tokio::time::advance(60s)
* 3`, the `FakePeripheral`'s `session_uuid_calls()` records exactly
4 calls (initial + three minute ticks) and each carries the
`session_uuid_for(bond_key, minute_n)` output.

### Phase 3: Audit line shape pinned by a tracing test

**User Intent:** The SPEC §3 scope item #22 audit shape
(`syauth-presenced: rotated id=<peer> minute=<N> uuid=<short>`) is
load-bearing for `sy syauth doctor` (S-016) and the waybar pill
(S-017). A future refactor that drops the `minute=` field or renames
the target would silently break the operator's diagnostic story.

**Actions:**
1. The second integration test installs a `tracing_subscriber::Layer`
   that records emitted events into a `Vec<RecordedEvent>` (the
   workspace doesn't carry `tracing-test`, so the test ships a ~30-LOC
   recorder in-test).
2. With `tokio::time::pause`, the test advances time past one minute,
   then asserts at least one recorded event has
   `target == ROTATION_LOG_TARGET` and message-shape with
   `rotated id=`, `minute=`, `uuid=`.

**Pain / Risk:**
- A subscriber installed via `set_global_default` would poison every
  other parallel test; the recorder uses `tracing::subscriber::with_default`
  so its scope is limited to the test future.
- Asserting on the FULL line string would tie the test to the `fmt`
  layer's formatting choices; the test inspects the structured event
  via the layer instead, asserting on the `target` and the rendered
  message field.

**Success Signal:**
`cargo test -p syauth-presenced --test rotation --
syslog_emits_rotation_line` passes; the recorded event matches the
SPEC line shape.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| `Bond` does not carry a `bond_key`; the 32-byte key lives in `keys/<peer_id>.bin` | 1 | The orchestrator takes both `bond` (for `peer_id` + display name) and `bond_key: [u8; BOND_KEY_BYTES]` (for `session_uuid_for`) as explicit constructor args — no implicit filesystem read inside the orchestrator |
| `interval_at(now + 60s, 60s)` would drift from wall-clock minute boundaries on a slow start | 2 | `align_to_next_minute(SystemTime::now())` is a pure function unit-tested at the minute, mid-minute, and just-before-mark; the orchestrator multiplies it through `Instant::now() + align_to_next_minute(...)` to seed `interval_at` |
| Tests that need to fire many ticks fast must not actually sleep 180 s | 2 | `tokio::test(start_paused = true)` + `tokio::time::advance(Duration::from_secs(60))` step the clock without real wait |
| `tracing` events are easy to emit and hard to assert on | 3 | A ~30-LOC in-test recorder layer captures events under `tracing::subscriber::with_default` so the assertion is scoped and deterministic |
| Wiring the orchestrator unconditionally into `runtime::run` would break S-001's `lifecycle_smoke` (no bond on disk) | 1 | Empty/no-bond → log warn, skip orchestrator construction, daemon stays up with the S-001/S-002 socket loop |

### North Star Summary

After S-004 closes, the daemon — given a single bonded peer on disk
— publishes the current minute's rotating UUID immediately on cold
start and re-publishes it once per wall-clock minute, with one
`tracing::info!` line per rotation matching the SPEC audit shape.
The rotation timer is aligned to wall-clock so two hosts sharing the
same bond agree on the advertised UUID at each minute, and the
orchestrator's `Peripheral` handle is opaque to the timer so the
production `PersistentPeripheral` and the test `FakePeripheral` are
behaviourally indistinguishable from the orchestrator's point of
view. S-005 layers multi-peer + diff + reload on the same scaffold.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] First rotation event fires on `Orchestrator::run` entry, not
      on the first minute tick — operators see the current UUID
      immediately.
- [x] `tokio::test(start_paused = true)` keeps the rotation tests
      sub-second on CI.

### Onboarding Clarity
- [x] The named constants `SECONDS_PER_MINUTE`,
      `ROTATION_LOG_TARGET`, and `SHORT_UUID_HEX_LEN` document the
      rotation cadence, the syslog tag, and the audit short-form
      width inline.

### Production-Ready Defaults
- [x] `align_to_next_minute(now)` defaults to whatever `SystemTime::now()`
      returns — no operator knob.
- [x] The rotation interval is `Duration::from_secs(SECONDS_PER_MINUTE)`
      with no override surface.

### Golden Path Quality
- [x] Cold start → publish current UUID → align timer → tick → publish
      next minute's UUID → repeat. No retries, no error branches in
      the steady state.

### Decision Load
- [x] `Orchestrator::new` takes three load-bearing inputs: the
      peripheral, the bond, the 32-byte bond_key. No optional
      cadence override.

### Progressive Complexity
- [x] Single-bond, single-UUID, single-target log line. S-005's
      diff/multi-peer machinery layers in without changing the
      single-bond path.

### Error Quality
- [x] `bonds.toml` missing → typed warn line, daemon stays up.
- [x] `keys/<peer_id>.bin` not 32 bytes → typed warn line, daemon
      stays up.

### Failure Safety
- [x] An orchestrator error (e.g. `Peripheral::set_session_uuids`
      returns `Err`) is logged at `tracing::warn!` and the next tick
      still fires — the daemon does not exit on a transient BLE
      failure.

### Runtime Transparency
- [x] One audit line per rotation; one warn line on construction
      failure; no silent state.

### Debuggability
- [x] `RUST_LOG=syauth_presenced=debug` shows the next-minute
      computation; `make test` exercises both the success and the
      audit-line-shape paths.

### Cross-Surface Consistency
- [x] `ROTATION_LOG_TARGET = "syauth-presenced"` matches the
      S-001 `SYSLOG_TAG` constant in `main.rs` — operators grep
      `journalctl -t syauth-presenced` and find both lifecycle and
      rotation lines.

### Workflow Consistency
- [x] The `Orchestrator::run(self, shutdown: CancellationToken)`
      shape mirrors the S-002 `server::serve(config, shutdown)`
      shape so a future `runtime::run` that joins both is a
      mechanical merge.

### Change Safety
- [x] The orchestrator does not touch `BondStore` writes — only
      reads on construction. Concurrent `syauth pair --add` cannot
      corrupt the orchestrator's view.

### Experimentation Safety
- [x] `FakePeripheral` is the test surface; the rotation tests do
      NOT need a real BlueZ adapter and do NOT mutate
      `/var/lib/syauth`.

### Interaction Latency
- [x] One `interval.tick().await` per minute; no busy loop, no
      polling.

### Developer Feedback Speed
- [x] `cargo test -p syauth-presenced --test rotation` runs in
      under a second on CI (paused clock).

### Team Scale
- [x] The `Orchestrator` is a `pub struct` with a doc-commented
      constructor — reviewers see the contract at the top of the
      file.

### System Scale
- [x] The single-bond rotation tick is `O(1)`. Multi-peer (S-005)
      extends the same tick to `O(N)`.

### Right Behavior by Default
- [x] No magic numbers — `SECONDS_PER_MINUTE`,
      `ROTATION_LOG_TARGET`, `SHORT_UUID_HEX_LEN` are all named.

### Anti-Bypass Design
- [x] The rotation timer is the only path that calls
      `set_session_uuids` from the orchestrator. There is no
      `set_session_uuids_now()` operator override that would let an
      external command publish a stale UUID.

## 4. Tests

### TC-01: `align_to_next_minute_at_minute_mark_returns_60s`

**Given** a `SystemTime` whose seconds-since-epoch is divisible by 60.
**When** `align_to_next_minute(now)` is called.
**Then** the returned `Duration` is exactly 60 seconds — the boundary
test is "we're at 12:00:00 right now; the next minute boundary is
12:01:00 in 60 s".

### TC-02: `align_to_next_minute_mid_minute_returns_remainder`

**Given** a `SystemTime` whose seconds-since-epoch modulo 60 is 17.
**When** `align_to_next_minute(now)` is called.
**Then** the returned `Duration` is exactly 43 seconds (60 - 17).

### TC-03: `rotates_at_minute_boundary`

**Given** an `Orchestrator` wired to a `FakePeripheral`, a known bond,
and a known 32-byte bond_key.
**When** the test starts under `#[tokio::test(start_paused = true)]`,
spawns `Orchestrator::run`, lets the construction-time rotation fire,
then calls `tokio::time::advance(Duration::from_secs(60))` three times.
**Then** `FakePeripheral::session_uuid_calls()` records exactly 4
calls (one on construction + one per simulated minute tick) and each
recorded set is `{session_uuid_for(bond_key, minute_n)}` for the
correct integer minute.

### TC-04: `syslog_emits_rotation_line`

**Given** the same orchestrator as TC-03, with a `tracing_subscriber`
recorder layer installed via `tracing::subscriber::with_default`.
**When** the test advances the clock past one minute.
**Then** the recorder holds at least one event whose
`target == ROTATION_LOG_TARGET` and whose rendered message contains
`rotated id=`, `minute=`, and `uuid=` substrings — the SPEC §3 scope
item #22 audit line shape.

### TC-05: `daemon_skips_orchestrator_when_bonds_file_empty`

**Given** the daemon's `runtime::run` called with a `bonds_file` that
exists but contains zero non-revoked bonds.
**When** the daemon starts.
**Then** the orchestrator is NOT constructed, a `tracing::warn!`
records `"no bond available, skipping rotation"`, and the S-001
lifecycle_smoke tests still pass (no crash, clean SIGTERM exit).

This last test is the regression check that pins the integration
path: the existing `cargo test -p syauth-presenced --test
lifecycle_smoke` still passes after S-004.

## Implementation

Files created:

- `crates/syauth-presenced/src/orchestrator.rs` — defines the
  `Orchestrator` struct, `align_to_next_minute` free function, the
  `ROTATION_LOG_TARGET` + `SHORT_UUID_HEX_LEN` constants, the
  `Orchestrator::new` constructor that takes
  `peripheral: Arc<dyn Peripheral + Send + Sync>`, `bond: Bond`,
  `bond_key: [u8; BOND_KEY_BYTES]`, and `start: tokio::time::Instant`,
  and the `Orchestrator::run(self, shutdown: CancellationToken)` async
  entrypoint.
- `crates/syauth-presenced/tests/rotation.rs` — TC-03 + TC-04
  integration tests, plus a ~30-LOC in-test recorder layer.

Files modified:

- `crates/syauth-presenced/src/lib.rs` — exports
  `orchestrator::{Orchestrator, align_to_next_minute,
  ROTATION_LOG_TARGET, SHORT_UUID_HEX_LEN}`.
- `crates/syauth-presenced/src/runtime.rs` — `runtime::run` reads the
  first non-revoked bond on cold-start; if present, loads the
  `bond_key` from `<keys_dir>/<peer_id>.bin` and constructs an
  `Orchestrator` (currently against `FakePeripheral` via a
  test-injection seam OR skipped in production until a follow-on step
  wires `PersistentPeripheral` — the seam is the `peripheral` arg of
  `Config`).
- `crates/syauth-presenced/Cargo.toml` — adds `syauth-core`,
  `syauth-transport` (with `test-fake` feature for tests), and
  `tokio-util` (for `CancellationToken`).
- `specs/unlock-proximity/ROADMAP.md` — tick S-004 DoD bullets and
  append the `Traceability` line.

## Traceability

- Roadmap item: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-004.
- Implementation files: see "Implementation" above.
- Test files: `crates/syauth-presenced/tests/rotation.rs` and unit
  tests inside `crates/syauth-presenced/src/orchestrator.rs`.
