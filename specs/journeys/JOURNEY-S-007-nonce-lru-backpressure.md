# JOURNEY-S-007: Nonce LRU + per-peer backpressure + queue deadline

> **Spec anchors:** `specs/unlock-proximity/SPEC.md` §3 Scope item #7
> ("Backpressure: at most one in-flight challenge per peer; the
> daemon queues subsequent challenges with a 1 s queue deadline");
> §6 Idempotency ("every nonce is single-use. A replayed response
> (same nonce) is rejected by the daemon's in-memory nonce cache
> (LRU of last 64 nonces per peer)"); §6 Failure Taxonomy rows
> "Phone receives notify but biometric fails 3 times" (transient)
> and "Response nonce mismatch" (permanent, likely attack →
> `PAM_AUTH_ERR(reason=replay)`); §7 T-Daemon-DoS ("the daemon
> caps concurrent socket accepts at 4 and rate-limits new
> connections to 10/s per peer-credential UID" — the per-peer
> `Semaphore(1)` + 1 s queue deadline is the daemon-internal
> companion defense, scoped to a single peer's challenge stream).
>
> **Roadmap row:** `specs/unlock-proximity/ROADMAP.md` Step S-007.
>
> **Closure condition (verbatim from ROADMAP.md):**
>
> ```
> cargo test -p syauth-presenced --test replay --test backpressure
> # all three tests pass
> ```

## Roadmap Link

- Source roadmap: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-007.
- Feature: extend the S-006 `Orchestrator` with (a) a per-peer
  `NonceCache` (in-memory `VecDeque<[u8; NONCE_BYTES]>` ordered LRU,
  cap 64) that rejects any response whose nonce was already seen for
  that peer, surfacing `ChallengeOutcome::Replay` and a
  `reason="replay"` audit row, and (b) a per-peer
  `tokio::sync::Semaphore::new(1)` gate that admits at most one
  in-flight `issue_challenge` per peer; any concurrent caller waits
  for the gate with a `BUSY_QUEUE_DEADLINE = Duration::from_millis(1000)`
  budget and on timeout returns `ChallengeOutcome::Busy` which the
  server maps to `Response::Challenge { ok=false, reason="busy" }`
  for the PAM mapper to translate into `PAM_AUTHINFO_UNAVAIL`.

## 1. Journey

When **the operator triggers two near-simultaneous `sudo` calls
against the same bonded peer (concurrent shells, automation script,
attacker probing the daemon) or when an attacker replays a
previously-captured `(challenge, response)` pair on the bonded
peer's response characteristic**, I want to **see the daemon's
orchestrator (a) admit only one in-flight challenge per peer via
its per-peer `Semaphore(1)`, parking the second caller until either
the first call completes OR the 1 s `BUSY_QUEUE_DEADLINE` elapses
— on timeout returning `ChallengeOutcome::Busy` so the PAM module
fails fast and the operator falls through to FIDO without queueing
further pressure on the BLE link, and (b) reject any response
whose nonce was already seen for that peer by consulting the
per-peer `NonceCache` (`VecDeque<[u8; NONCE_BYTES]>`, cap 64,
LRU-evicting the oldest entry once the 65th nonce arrives), so a
replay attack against the response characteristic surfaces as
`ChallengeOutcome::Replay` with an audit row carrying
`reason="replay"`**, so I can **(a) honour SPEC §3 scope item #7
(at most one in-flight challenge per peer, 1 s queue deadline,
`busy` reason on overflow), (b) honour SPEC §6 idempotency ("LRU
of last 64 nonces per peer"), (c) honour SPEC §6 Failure Taxonomy
("Response nonce mismatch" → `PAM_AUTH_ERR(reason=replay)`),
(d) honour SPEC §7 T-Daemon-DoS by capping per-peer concurrency
without an unbounded queue, (e) keep CI radio-free by exercising
both the replay and the busy paths against the `FakePeripheral`
test double under `tokio::test(start_paused = true)`**.

## 2. CJM

Before S-007, the orchestrator's `issue_challenge` admits concurrent
callers without any gating: two concurrent `tokio::spawn` tasks
both notify on the same peer's challenge characteristic and both
race on the per-peer response mpsc, producing non-deterministic
audit rows and risking nonce-on-nonce collisions on the wire.
There is no replay defense — an attacker who captures a valid
`(challenge, response)` pair and replays the response bytes on the
response characteristic before the legitimate phone responds would
see the orchestrator verify the signature (the signature is valid
against the challenge frame body), return `ChallengeOutcome::Ok`,
and the audit row would show a clean `outcome=ok`. S-007 closes
both gaps with two small additions to the per-peer state:
(a) `Mutex<NonceCache>` — a `VecDeque<[u8; NONCE_BYTES]>` with a
`contains(&[u8; NONCE_BYTES]) -> bool` and `insert([u8; NONCE_BYTES])`
pair, capped at `NONCE_LRU_CAP = 64` so the oldest nonce is
evicted via `pop_front` when the 65th is inserted; (b)
`Arc<tokio::sync::Semaphore>` with a single permit guarding the
peer's challenge characteristic. `issue_challenge` calls
`tokio::time::timeout(BUSY_QUEUE_DEADLINE, sem.acquire())` and on
timeout returns `ChallengeOutcome::Busy`. The nonce check runs
*after* the response arrives, so a fresh nonce admits the response
to the verify path and an already-seen nonce short-circuits to
`Replay`.

### Phase 1: per-peer Semaphore admits at most one in-flight challenge

**User Intent:** the operator runs two `sudo` calls in two shells
near-simultaneously; both calls land on the orchestrator's
`issue_challenge` for the same `peer_id`. The first call acquires
the per-peer `Semaphore(1)`, sends a notify on the per-peer
challenge characteristic, and awaits the phone's response. The
second call hits the semaphore, waits up to
`BUSY_QUEUE_DEADLINE = Duration::from_millis(1000)`, and on
expiry returns `ChallengeOutcome::Busy`. The server's dispatcher
maps `Busy` to `Response::Challenge { ok=false, signature=None,
reason=BUSY_REASON }`; the PAM module (S-008) maps that reason to
`PAM_AUTHINFO_UNAVAIL` so the operator's second shell falls
through to FIDO without piling on the phone's biometric prompt.

**Actions:**

1. The orchestrator looks up the peer's `Arc<Semaphore>` (the
   `PeerEntry::semaphore` field added by this step). Unknown peer
   short-circuits to `ChallengeOutcome::UnknownPeer` *before* any
   semaphore work — the membership check is the orchestrator's
   first gate, so a missing peer never starves the in-flight slot.
2. `tokio::time::timeout(BUSY_QUEUE_DEADLINE, semaphore.acquire_owned())`
   admits the first caller within microseconds. A second caller
   for the same peer waits in the semaphore's FIFO queue until
   either the first permit drops (the first challenge completes,
   times out, or errors) or the 1 s budget elapses.
3. On `tokio::time::timeout::Elapsed`, the orchestrator audits one
   row with `outcome=busy, reason=busy` and returns
   `ChallengeOutcome::Busy`. The caller holds no permit; the
   first call's permit is still alive.
4. On a successful permit acquire, the orchestrator proceeds with
   the existing S-006 path (RNG nonce, build frame, notify, await
   response, verify, audit). The permit is held by the orchestrator
   future until the function returns; `Drop` releases it
   automatically — no explicit `drop(permit)` is needed.

**Pain / Risk:**

- An unbounded queue behind the semaphore would defeat the SPEC
  §7 T-Daemon-DoS defense (an attacker who keeps connections open
  could pile up arbitrarily many waiters and exhaust memory). The
  `tokio::time::timeout` wrapper bounds every waiter at 1 s, so
  the queue length is bounded by `(connections-per-second) × 1 s`
  on the operator's box, which the daemon's
  `CONCURRENT_ACCEPT_CAP = 4` already caps upstream.
- A test under `tokio::test(start_paused = true)` MUST advance
  the virtual clock past `BUSY_QUEUE_DEADLINE` *while the first
  task is parked on its own await* — `tokio::time::advance` keeps
  the runtime in a consistent state and lets the busy path return
  deterministically.
- A semaphore held across an `.await` is a known footgun (the
  permit can outlive the holding task on cancellation). The
  orchestrator's `issue_challenge` future holds the permit for
  the entire call, but cancellation of the future (e.g., the PAM
  caller drops the socket connection mid-call) releases the
  permit via `Drop`, freeing the slot for the next caller — the
  standard tokio semaphore contract.

**Success Signal:** with two concurrent `issue_challenge` tasks
spawned for the same peer under `tokio::test(start_paused = true)`,
the first task is parked awaiting `wait_for_response` (no injected
response), the test advances the clock by
`BUSY_QUEUE_DEADLINE + ε`, and the second task resolves to
`ChallengeOutcome::Busy`. The first task remains parked. Cancelling
the first task releases its permit (verified by a separate sanity
assertion: a third sequential `issue_challenge` after the first
two would not also see `Busy` once the first task drops).

### Phase 2: per-peer NonceCache rejects replayed nonces

**User Intent:** an attacker who can write to the bonded peer's
response characteristic (e.g., via a relay rig on the BLE link, or
a compromised companion device) replays a previously-captured
`(challenge_frame, response_frame)` pair. The orchestrator's S-006
verify path would accept the signature (it's a real signature over
the real challenge frame). S-007 closes the gap by recording every
issued nonce in a per-peer `NonceCache`. A response whose nonce
matches a cache entry is `Replay`; a response whose nonce is fresh
is admitted to verify and, on success, the nonce is inserted into
the cache before the function returns.

**Actions:**

1. Each `PeerEntry` carries a `Mutex<NonceCache>` (`tokio::sync::Mutex`
   so `issue_challenge` can `await` while holding the cache lock,
   though in practice the cache ops are O(64) and uncontended).
2. The orchestrator's `issue_challenge_with_nonce(peer_id, nonce,
   deadline)` test helper bypasses the OsRng so a test can force a
   collision deterministically; production callers go through
   `issue_challenge(peer_id, deadline)` which generates a fresh
   `OsRng` nonce.
3. *After* the signature verifies, the orchestrator checks
   `cache.contains(&nonce)`. If `true`, audit `outcome=replay,
   reason=replay`, return `ChallengeOutcome::Replay`. If `false`,
   `cache.insert(nonce)` (which `pop_front`s when the cache hits
   `NONCE_LRU_CAP + 1` entries), audit `outcome=ok`, return
   `ChallengeOutcome::Ok`.
4. The audit row's `nonce_hex` column captures the actual nonce
   (not the zero placeholder) so an operator can grep the audit
   log for the replayed nonce and correlate against the phone-side
   log.

**Pain / Risk:**

- The replay check runs *after* the signature verify. A pre-verify
  check would let an attacker who couldn't sign still pollute the
  cache with arbitrary nonces, denying-of-service the legitimate
  user. Post-verify means only nonces with valid signatures
  consume cache slots — the cache is a defense against
  signed-replay, not against signature-spam.
- An LRU cap of 64 means an operator who runs 65 sudos in rapid
  succession overwrites the oldest cached nonce. A nonce that
  cycles back through OsRng's output stream after 64 uses has
  probability `64 / 2^128` of colliding with a legitimate fresh
  nonce — well below the SPEC's correctness floor.
- The `Replay` audit row carries `nonce_hex` so an operator
  investigating an attack can `grep <nonce>` the audit log to see
  both the original `ok` row and the subsequent `replay` row for
  the same nonce.

**Success Signal:** with a test that calls
`issue_challenge_with_nonce(peer_id, nonce_a, ..)` followed by
`issue_challenge_with_nonce(peer_id, nonce_a, ..)` (same nonce,
fresh signed response for each call), the first call returns
`ChallengeOutcome::Ok`, the second returns
`ChallengeOutcome::Replay`. The audit log has two lines —
`outcome=ok, nonce_hex=<a>` then `outcome=replay, nonce_hex=<a>`.
A separate pure-data test on `NonceCache` directly verifies that
inserting 65 distinct nonces evicts the first; the first
`contains` returns `false` and the 65th `contains` returns `true`.

### Phase 3: server-side mapping and audit-line discipline

**User Intent:** the new `ChallengeOutcome::Busy` and the now-real
`ChallengeOutcome::Replay` variants surface through
`Response::Challenge { ok, signature, reason }` exactly the same
way the S-006 variants do — typed reason string, `signature=None`,
`ok=false`. The PAM module (S-008) maps `reason=busy` to
`PAM_AUTHINFO_UNAVAIL` and `reason=replay` to `PAM_AUTH_ERR`. The
orchestrator audits every Busy and Replay outcome to
`/var/lib/syauth/last.log` so the operator's investigation surface
is unchanged.

**Actions:**

1. The `ChallengeOutcome` enum gains a `Busy` variant alongside
   the existing `Ok / Denied / Replay / BadSignature / TimedOut /
   UnknownPeer / TransportError`. The `reason_str()` accessor maps
   `Busy → BUSY_REASON = "busy"`.
2. The constant `OUTCOME_REASON_BUSY = "busy"` joins the
   `OUTCOME_REASON_*` family for grep symmetry, and is re-exported
   from `crates/syauth-presenced/src/lib.rs`.
3. The server's `dispatch` arm for `Request::Challenge` already
   routes through `outcome.signature_bytes()` + `outcome.reason_str()`
   — no change is needed beyond the orchestrator producing the
   new variants.
4. The S-006 audit-row contract is preserved: every Busy and
   every Replay outcome appends one line to the audit log before
   the function returns. The `outcome` column equals the
   `reason_str()` of the variant; the `reason` column matches
   (mirroring the S-006 design where outcome and reason converge
   on the single typed string).

**Pain / Risk:**

- A `Busy` audit row with an empty `nonce_hex` would break the
  audit-line column shape (the audit row's nonce column is fixed
  at 32 hex chars). The orchestrator audits Busy with the
  `ZERO_NONCE_HEX` placeholder (same convention as `UnknownPeer`)
  so an `awk -F,` pipeline sees consistent column widths.
- The `socket_smoke` and `lifecycle_smoke` tests construct the
  server without an orchestrator and expect
  `Response::Challenge { reason="not-implemented" }`. That contract
  is unchanged in S-007 — the new `Busy` reason is only produced
  when an orchestrator is wired.

**Success Signal:** the `tests/backpressure.rs` test asserts the
second concurrent call returns `ChallengeOutcome::Busy` with
`outcome.reason_str() == BUSY_REASON`. The `tests/replay.rs` test
asserts the second-with-same-nonce call returns
`ChallengeOutcome::Replay` with `outcome.reason_str() ==
OUTCOME_REASON_REPLAY`. Direct unit assertions on `NonceCache`
verify the LRU semantics at the data-structure level.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Two concurrent `sudo` calls on the same peer would race the per-peer notify / response slots and produce non-deterministic audit rows | 1 | `Arc<tokio::sync::Semaphore>` on `PeerEntry` admits at most one in-flight challenge per peer; a 1 s `BUSY_QUEUE_DEADLINE` caps the waiter queue length |
| An attacker who captures `(challenge, response)` and replays the response would otherwise be indistinguishable from a fresh unlock | 2 | `VecDeque<[u8; NONCE_BYTES]>` per peer, cap `NONCE_LRU_CAP = 64`, `contains` before insert; `ChallengeOutcome::Replay` short-circuits to a `reason=replay` audit row before the function returns success |
| A pre-verify nonce check would let a junk-signature attacker DoS the cache by pre-filling 64 slots with cheap garbage | 2 | The nonce-insert runs AFTER the signature verifies; the cache only accepts nonces backed by a valid signature, so signature-spam cannot evict legitimate nonces |
| A test that drives `BUSY_QUEUE_DEADLINE` under wall-clock time would add 1+ s per CI run | 1 | `tokio::test(start_paused = true)` + `tokio::time::advance` advances virtual time past the deadline in microseconds |
| The S-006 `FakePeripheral::wait_for_response` polls every 10 ms; under `start_paused = true` the polling itself does not consume virtual time so an unbounded wait would hang | 1 | The test forces the first call to park on `wait_for_response` (no injected response), then advances the clock by `BUSY_QUEUE_DEADLINE + ε`; only the second task resolves (to `Busy`); the first task remains parked until the test drops it |
| Calling `issue_challenge` always with a fresh OsRng nonce makes the replay test unreachable | 2 | A test-only entry point `issue_challenge_with_nonce(peer_id, nonce, deadline)` bypasses the RNG; production callers continue to use `issue_challenge` |

### North Star Summary

After S-007 closes, the daemon enforces two SPEC §6 contracts on
every challenge transaction: (a) at most one in-flight challenge
per peer, bounded by a 1 s queue deadline beyond which the daemon
returns `Busy` so the PAM caller falls through to FIDO without
piling additional pressure on the BLE link or the phone's
biometric prompt; (b) per-peer single-use nonces enforced by an
LRU of the last 64 nonces, so a captured `(challenge, response)`
replayed on the response characteristic surfaces as `Replay` with
a `reason=replay` audit row that the operator can grep alongside
the original `ok` row to investigate. Both defenses are exercised
against `FakePeripheral` under `tokio::test(start_paused = true)`,
keeping CI radio-free and sub-second.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] First `sudo` on a fresh daemon still admits within the
      S-006 1.2 s deadline; the per-peer semaphore admits the
      first caller within microseconds.
- [x] `tokio::test(start_paused = true)` keeps the busy-path test
      sub-second on CI.

### Onboarding Clarity
- [x] Named constants `NONCE_LRU_CAP`, `BUSY_QUEUE_DEADLINE`,
      `BUSY_REASON`, `OUTCOME_REASON_BUSY` document the
      idempotency + backpressure pipeline inline.
- [x] The `Busy` audit row uses the same comma-separated shape as
      every other audit row; `awk -F,` pipelines need no special
      casing.

### Production-Ready Defaults
- [x] `NONCE_LRU_CAP = 64` is the SPEC §6 idempotency floor; no
      operator knob.
- [x] `BUSY_QUEUE_DEADLINE = Duration::from_millis(1000)` is the
      SPEC §3 scope item #7 floor; no operator knob.

### Golden Path Quality
- [x] First call: acquire permit → notify → wait_for_response →
      verify → check nonce cache → insert nonce → return Ok. One
      named sequence, one function per arrow.

### Decision Load
- [x] `Orchestrator::issue_challenge(peer_id, deadline)` keeps
      its two-argument signature; the test-only
      `issue_challenge_with_nonce(peer_id, nonce, deadline)`
      adds one argument for the deterministic-collision test.

### Progressive Complexity
- [x] The S-006 single-peer challenge surface still works; the
      semaphore admits the first caller within microseconds when
      no concurrent caller exists.

### Error Quality
- [x] `Busy` and `Replay` each map to a documented reason string
      on the wire; the PAM caller's error-mapping table is a
      one-line `match`.
- [x] `tokio::time::error::Elapsed` (from the semaphore-acquire
      `tokio::time::timeout`) collapses to `ChallengeOutcome::Busy`
      with no panic, no `unwrap`.

### Failure Safety
- [x] A panic inside the verify routine cannot leave the
      semaphore permit leaked — `Drop` releases it on stack
      unwind.
- [x] A panic inside the nonce-cache `insert` cannot leave the
      audit log un-appended; the audit `append` runs before the
      `match` arm that constructs the return value.

### Runtime Transparency
- [x] One audit line per `Busy` and `Replay` outcome on disk.
- [x] One `tracing::info!` line on `ROTATION_LOG_TARGET` per
      transaction summarising peer_id + outcome.

### Debuggability
- [x] `RUST_LOG=syauth_presenced=debug` shows the nonce_hex
      hitting the cache; the audit row's `nonce_hex` column is
      the canonical join key against the phone-side log.
- [x] `tail /var/lib/syauth/last.log | grep replay` returns
      every per-peer replay event.

### Cross-Surface Consistency
- [x] `BUSY_REASON = "busy"` matches the SPEC §3 scope item #7
      wire text verbatim; the PAM mapper's table is one
      consistent string.

### Workflow Consistency
- [x] `NonceCache` follows the S-004 / S-005 `PeerEntry` pattern
      — one small struct on the per-peer record, no fan-out.

### Change Safety
- [x] The `Response::Challenge` wire shape is unchanged from
      S-006; only the `reason` strings expand to include
      `BUSY_REASON`.
- [x] The `ChallengeOutcome` enum adds one variant
      (`Busy`); existing match arms in `server.rs` route through
      `outcome.reason_str()` so no callers break.

### Experimentation Safety
- [x] Every challenge outcome is exercised against
      `FakePeripheral` in `tests/replay.rs` and
      `tests/backpressure.rs`; no real BlueZ adapter is required
      for CI.

### Interaction Latency
- [x] One `tokio::time::timeout(BUSY_QUEUE_DEADLINE,
      semaphore.acquire_owned())` per call; no busy loop, no
      polling.

### Developer Feedback Speed
- [x] `cargo test -p syauth-presenced --test replay --test
      backpressure` runs in under a second on CI (paused clock).

### Team Scale
- [x] `NonceCache` is a `pub(crate)` struct with a 2-method
      surface (`contains`, `insert`); reviewers see the cap and
      the eviction policy at the type definition.

### System Scale
- [x] `NonceCache::contains` is `O(NONCE_LRU_CAP) = O(64)` — a
      fixed constant; `insert` is `O(1)` amortised with a single
      `pop_front` on overflow.

### Right Behavior by Default
- [x] All named constants in scope: `NONCE_LRU_CAP`,
      `BUSY_QUEUE_DEADLINE`, `BUSY_REASON`, `OUTCOME_REASON_BUSY`.

### Anti-Bypass Design
- [x] Every `Busy` and `Replay` branch writes the audit row
      before returning. There is no codepath that returns without
      an audit append.

## Acceptance Criteria (DoD, verbatim from ROADMAP.md Step S-007)

- [x] `NonceCache` per-peer LRU (cap 64) implemented in `orchestrator.rs`.
- [x] Per-peer `Semaphore(1)` gates concurrent challenges.
- [x] `crates/syauth-presenced/tests/replay.rs::repeated_nonce_returns_replay`
      passes.
- [x] `crates/syauth-presenced/tests/replay.rs::lru_evicts_oldest_nonce_at_cap_65`
      passes.
- [x] `crates/syauth-presenced/tests/backpressure.rs::second_in_flight_request_returns_busy_after_1s`
      passes.
- [x] `make scope-discipline && make lint && make test` green.

## 4. Tests

### TC-01: `repeated_nonce_returns_replay`

**Given** a `FakePeripheral` registered with one peer whose bond
carries a known `phone_pubkey`, the orchestrator constructed with
that bond and an audit log pointed at a tempdir. **When** the
test signs the challenge frame body built around a fixed nonce
`A`, calls `fake.inject_response(peer_id, signed_a)`, drives
`orchestrator.issue_challenge_with_nonce(peer_id, A,
DEFAULT_AUTH_TIMEOUT)` (assert: `Ok`), then re-signs the same
nonce-`A` challenge body, calls
`fake.inject_response(peer_id, signed_a)` a second time, and
drives `orchestrator.issue_challenge_with_nonce(peer_id, A,
DEFAULT_AUTH_TIMEOUT)`. **Then** the second call returns
`ChallengeOutcome::Replay`, the audit log records 2 lines whose
outcome columns are `["ok", "replay"]`, and the nonce_hex column
on both rows equals `hex::encode(A)`.

### TC-02: `lru_evicts_oldest_nonce_at_cap_65`

**Given** a fresh `NonceCache` (no orchestrator required —
direct unit test on the LRU data structure). **When** the test
calls `cache.insert(n_i)` for `i in 0..=64` (65 distinct nonces,
the first one `n_0` and the last one `n_64` are both distinct).
**Then** `cache.contains(&n_0)` returns `false` (evicted) and
`cache.contains(&n_64)` returns `true` (newest). For belt-and-
suspenders, `cache.contains(&n_1)` returns `true` (still inside
the cap window).

### TC-03: `second_in_flight_request_returns_busy_after_1s`

**Given** a `FakePeripheral` registered with one peer, the
orchestrator wired with that peer, no injected response (so the
first task parks on `wait_for_response`). **When** the test
spawns task A (`orchestrator.issue_challenge(peer_id,
DEFAULT_AUTH_TIMEOUT)`), waits a short virtual tick to let task A
acquire the semaphore and park on `wait_for_response`, then
spawns task B for the same peer, advances the virtual clock by
`BUSY_QUEUE_DEADLINE + ε`. **Then** task B resolves to
`ChallengeOutcome::Busy`, the audit log records one line with
`outcome=busy`, and task A remains parked (asserted by checking
the spawn handle is not finished). The test runs under
`#[tokio::test(start_paused = true)]` so wall-clock cost on CI
is sub-second.

## Implementation

Files created:

- `specs/journeys/JOURNEY-S-007-nonce-lru-backpressure.md` —
  this document.
- `crates/syauth-presenced/tests/replay.rs` — two integration
  tests (`repeated_nonce_returns_replay`,
  `lru_evicts_oldest_nonce_at_cap_65`).
- `crates/syauth-presenced/tests/backpressure.rs` — one
  `tokio::test(start_paused = true)` integration test
  (`second_in_flight_request_returns_busy_after_1s`).

Files modified:

- `crates/syauth-presenced/src/orchestrator.rs` — adds the
  `NonceCache` struct (`VecDeque<[u8; NONCE_BYTES]>` with
  `contains` + `insert` and an LRU pop-front at `NONCE_LRU_CAP +
  1`), the constants `NONCE_LRU_CAP = 64`,
  `BUSY_QUEUE_DEADLINE = Duration::from_millis(1000)`,
  `BUSY_REASON = "busy"`, `OUTCOME_REASON_BUSY = "busy"`, the
  `ChallengeOutcome::Busy` variant, the
  `PeerEntry::nonce_cache: Arc<TokioMutex<NonceCache>>` and
  `PeerEntry::challenge_slot: Arc<Semaphore>` fields (constructed
  via `PeerEntry::new`), the `PeerState` lookup struct, the
  semaphore-gated `Orchestrator::issue_challenge` wrapper, the
  test-only `Orchestrator::issue_challenge_with_nonce` entry
  point, the `Orchestrator::acquire_challenge_slot` helper
  (`tokio::time::timeout(BUSY_QUEUE_DEADLINE,
  Arc::clone(slot).acquire_owned())`), and the
  `Orchestrator::run_challenge` body that performs the post-
  verify replay check (`cache.contains(&nonce) →
  ChallengeOutcome::Replay`, otherwise `cache.insert(nonce)`).
  Two unit tests inside `mod tests` cover the
  `NonceCache::contains` + `insert` contract and the cap-1
  eviction.
- `crates/syauth-presenced/src/lib.rs` — re-exports
  `BUSY_QUEUE_DEADLINE`, `BUSY_REASON`, `NONCE_LRU_CAP`,
  `NonceCache`, `OUTCOME_REASON_BUSY`.
- `specs/unlock-proximity/ROADMAP.md` — ticks S-007 DoD bullets
  and appends the `Traceability` line.

## Traceability

- Roadmap item: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-007.
- Implementation files: see "Implementation" above.
- Test files: `crates/syauth-presenced/tests/replay.rs`,
  `crates/syauth-presenced/tests/backpressure.rs`, and the unit
  tests inside `crates/syauth-presenced/src/orchestrator.rs`
  (the `NonceCache` data-structure tests).
