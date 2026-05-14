# JOURNEY-S-003: `syauth-core` replay nonce cache

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md §S-003](../syauth/ROADMAP.md)
- Feature: in-memory sliding LRU + TTL cache of recently-seen response nonces,
  used by `pam_sm_authenticate` to reject replayed responses inside a single
  boot session.

## 1. Journey

When **I am the `syauth-pam` author landing `pam_sm_authenticate` in S-008/S-009**,
I want **a deterministic, time-injected `ReplayCache` in `syauth-core` that I
can feed every response nonce and trust to tell me `Fresh` or `Replayed`
without ever calling `Instant::now()` itself**, so I can **defeat the T-002
replay-attack class (SPEC §6) using a tiny pure-Rust data structure that is
hermetically unit-testable, never panics on adversarial input, never logs
nonces, and adds zero new third-party deps to the workspace's audit surface**.

## 2. CJM

The downstream user is the PAM module author (this same project, one roadmap
step ahead). Today they have a parsed `Frame` from S-002 carrying a 16-byte
nonce and no way to know whether it is the original or a replayed copy. SPEC
§4.2 promises a 64-entry, 10-second sliding window of recently-seen nonces;
SPEC §6 lists T-002 (Replay) as Mitigated by exactly this mechanism. The
attacker model: an on-path adversary who has captured a previous, valid
response frame and resubmits it (verbatim or piecewise) inside the same PAM
invocation or a closely subsequent one. The defender model: a stateless PAM
module with a fresh tokio runtime per call — *no* state crosses the FFI
boundary (SPEC §4.4) — so the cache lives only for the duration of one
`pam_sm_*` call. That bounds the worst-case window to ~2.0 s (the unlock
deadline), but the spec keeps 10 s of headroom for slow phones and BLE
retransmits.

The PAM author needs: (a) a value type they can stack-allocate at function
entry, (b) `observe(nonce, now) -> Acceptance` that *takes* the time so unit
tests can fast-forward without sleeping, (c) `Fresh` / `Replayed` as the only
two outcomes (no `Result`, no boolean blindness), and (d) named constants for
the defaults so the magic-number police never quote them at us.

### Phase 1: Construct

**User Intent:** Allocate a cache sized for the current PAM invocation.

**Actions:** Call `ReplayCache::new(DEFAULT_REPLAY_CAP, DEFAULT_REPLAY_TTL)`,
where the consts equal SPEC §4.2's `64` and `Duration::from_secs(10)`. The
caller may also pick smaller / larger caps and TTLs for tests.

**Pain / Risk:**
- Caller passes `cap == 0`. The cache must accept this gracefully — every
  observation returns `Fresh`, no entries are retained. This is the natural
  semantics of "remember zero things" and avoids a fallible constructor
  (`new` is infallible, matching the rest of the public surface).
- Caller picks a TTL of `Duration::ZERO`. Entries expire instantly; every
  observation returns `Fresh`. Same reasoning — degenerate but well-defined.
- Caller fears the cache will silently grow. The cap is a hard cap; LRU
  evicts the oldest on overflow. Documented on the type.

**Success Signal:** `ReplayCache::new(...)` returns a value-typed cache; no
allocation observable to the caller is required to call this.

### Phase 2: Observe — fresh nonce

**User Intent:** Record a freshly-decoded nonce and learn whether it is a
first observation.

**Actions:** Call `cache.observe(nonce, now)` where `now` is an `Instant`
the caller chose (in production, `Instant::now()` at the call site; in
tests, a fixed origin plus arithmetic).

**Pain / Risk:**
- Cache must not call `Instant::now()` internally. Enforced by API shape
  (the parameter is the *only* time source) and by the test
  `ttl_expiration_re_accepts` which would deadlock if the cache used wall
  time.
- Eviction of TTL-expired entries must happen *before* the fresh-vs-replay
  decision. Otherwise a stale match would falsely flag a nonce as `Replayed`
  past its window.
- Eviction of overflow entries must happen *after* insertion. Otherwise the
  first observation of the `(cap+1)`th nonce would evict itself.

**Success Signal:** Returns `Acceptance::Fresh`; the nonce is now in the
cache; subsequent observations of the same nonce within TTL return
`Acceptance::Replayed`.

### Phase 3: Observe — exact replay

**User Intent:** Catch the on-path attacker who resubmits the captured
response.

**Actions:** Call `cache.observe(nonce, now)` with a nonce already present
in the cache and `now - inserted_at < ttl`.

**Pain / Risk:**
- A replay arriving *after* TTL is, by spec, accepted as fresh — the cache
  is a sliding window, not an eternal blacklist. This is intentional: a
  long-since-expired nonce is no longer evidence of an active replay; the
  upstream signature and tag checks still gate acceptance. The cache is one
  of several defenses, not a sole gate.
- Replay-on-replay: a third observation of the same nonce, still within
  TTL, must still return `Replayed`. The cache does not "consume" the entry.

**Success Signal:** Returns `Acceptance::Replayed`. No state mutation that
the caller can observe (entry's recorded `inserted_at` is *not* refreshed —
otherwise an attacker could keep a captured nonce alive forever by spamming
replays).

### Phase 4: Observe — capacity overflow (LRU eviction)

**User Intent:** Bound memory at 64 entries even under a flood.

**Actions:** Observe `cap + 1` distinct nonces in quick succession (`now`
the same or monotonically increasing, all within TTL).

**Pain / Risk:**
- Naive ring-buffer that overwrites by index would lose ordering. We
  evict-the-front-on-overflow, which gives true LRU on a `VecDeque`.
- Eviction order must be deterministic. The first nonce inserted is the
  first nonce evicted. Test asserts the very first nonce becomes
  re-acceptable.

**Success Signal:** A *fresh* observation of the very first inserted nonce
returns `Acceptance::Fresh` (because it has been evicted), while the other
`cap` nonces still return `Acceptance::Replayed`.

### Phase 5: Observe — TTL expiration

**User Intent:** Let a long-quiescent nonce be re-issued legitimately.

**Actions:** Observe a nonce, advance `now` past `ttl`, observe the same
nonce again.

**Pain / Risk:**
- TTL boundary semantics: `now - inserted_at >= ttl` is expired,
  `< ttl` is fresh. Inclusive of equality on the expired side is the
  cleaner choice (think "ten seconds means ten seconds, not ten-minus-one-
  nanosecond"). A test asserts behavior at `ttl + DEFAULT_REPLAY_TTL_NUDGE`
  where the nudge is a small named const — no naked literals.

**Success Signal:** Second observation returns `Acceptance::Fresh`.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Caller doesn't know what cap/TTL to pick | 1 | Export `DEFAULT_REPLAY_CAP` and `DEFAULT_REPLAY_TTL` matching SPEC §4.2 |
| Tests would have to sleep 10 s to exercise TTL | 5 | Inject `now: Instant` rather than calling `Instant::now()` |
| Boolean return loses replay semantics in logs | 2-3 | Return `enum Acceptance { Fresh, Replayed }` |

### North Star Summary

A PAM module author can drop `let mut cache = ReplayCache::new(...);` at the
top of `pam_sm_authenticate`, call `cache.observe(...)` once per response
frame, and pattern-match `Fresh` to proceed or `Replayed` to bail. The cache
is deterministic, allocation-bounded, and adds zero deps. Tests fast-forward
time arithmetically without sleeping.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] Single call site: `ReplayCache::new(cap, ttl)` then `observe(nonce, now)`.
- [x] Defaults `DEFAULT_REPLAY_CAP=64` / `DEFAULT_REPLAY_TTL=10s` exported.

### Onboarding Clarity
- [x] Module-level docstring on `replay.rs` links to SPEC §4.2 and §6 T-002.
- [x] `Acceptance::Replayed` is the error signal — no `Result`-shaped boolean.

### Production-Ready Defaults
- [x] Defaults match SPEC §4.2 verbatim.
- [x] `cap == 0` is tolerated; `Duration::ZERO` is tolerated.

### Golden Path Quality
- [x] Five tests cover: fresh / exact-replay / LRU-eviction / TTL / interleaved.

### Decision Load
- [x] One constructor (`new`), one method (`observe`), two-variant enum.

### Progressive Complexity
- [x] No builder, no async, no traits.

### Error Quality
- [x] `Replayed` *is* the error condition; downstream chooses how to report it.

### Failure Safety
- [x] No `unwrap()` / `expect()` in production code paths.
- [x] No `unsafe` (workspace deny is active).

### Runtime Transparency
- [x] Observations are pure; no logging from the cache (caller logs).

### Debuggability
- [x] `Acceptance` derives `Debug`, `PartialEq`, `Eq`, `Copy`, `Clone`.

### Cross-Surface Consistency
- [x] Same crate is consumed by desktop PAM and Android (UniFFI), so the same
      cache semantics apply on both ends.

### Workflow Consistency
- [x] Lives in `crates/syauth-core/src/replay.rs`, in-file `#[cfg(test)] mod tests`,
      mirroring `frame.rs` and `bond.rs` from S-002 / S-005.

### Change Safety
- [x] New module; no existing public surface changes.

### Experimentation Safety
- [x] Cache is per-PAM-call; nothing persists.

### Interaction Latency
- [x] `observe` is O(cap) — cap is small (64); no allocations on the hot path
      after the initial `VecDeque` capacity reservation.

### Developer Feedback Speed
- [x] All tests run in well under a second on a developer machine.

### Team Scale
- [x] Module is small; shape is documented in journey + roadmap.

### System Scale
- [x] Memory bounded at `cap * (16 + Instant)` ≈ a couple hundred bytes.

### Right Behavior by Default
- [x] First observation of any nonce is `Fresh`; replays inside the window are
      `Replayed`. No configuration needed for the SPEC defaults.

### Anti-Bypass Design
- [x] Cache cannot bypass itself: every nonce flows through the same
      eviction-then-lookup-then-insert path.

## 4. Tests

All tests live in `crates/syauth-core/src/replay.rs` under `#[cfg(test)] mod tests`.

### TC-01: `fresh_nonce_accepted`

**Given** an empty `ReplayCache::new(DEFAULT_REPLAY_CAP, DEFAULT_REPLAY_TTL)`.
**When** the caller calls `observe(nonce, origin)`.
**Then** the return value is `Acceptance::Fresh`.

### TC-02: `exact_replay_rejected`

**Given** a cache that has already observed `nonce` at `origin`.
**When** the caller calls `observe(nonce, origin + ttl/2)` (still inside the
window).
**Then** the return value is `Acceptance::Replayed`.

### TC-03: `lru_eviction_by_capacity`

**Given** a cache with `cap == DEFAULT_REPLAY_CAP` and TTL much larger than
the test runtime, populated with `cap + 1` distinct nonces.
**When** the caller re-observes the *first* inserted nonce.
**Then** the return value is `Acceptance::Fresh` (it has been evicted).

### TC-04: `ttl_expiration_re_accepts`

**Given** a cache that has observed `nonce` at `origin`.
**When** the caller calls `observe(nonce, origin + ttl + DEFAULT_REPLAY_TTL_NUDGE)`.
**Then** the return value is `Acceptance::Fresh`.

### TC-05: `interleaved_fresh_and_replay`

**Given** an empty cache.
**When** the caller observes the sequence `A B A C B` at strictly
increasing `now`s, all inside the TTL window.
**Then** the outcomes are `Fresh Fresh Replayed Fresh Replayed` in order.

### TC-06: `cap_zero_accepts_everything_as_fresh`

**Given** `ReplayCache::new(0, DEFAULT_REPLAY_TTL)`.
**When** the caller calls `observe(nonce, origin)` and then `observe(nonce, origin)`
again.
**Then** both observations return `Acceptance::Fresh` (nothing is retained).

### TC-07: `replay_does_not_refresh_inserted_at`

**Given** a cache that observed `nonce` at `origin`.
**When** the caller calls `observe(nonce, origin + ttl/2)` (Replayed), and
then `observe(nonce, origin + ttl + DEFAULT_REPLAY_TTL_NUDGE)`.
**Then** the third observation returns `Acceptance::Fresh` — proving the
middle replay did not push the eviction deadline forward.

## Traceability
- Roadmap item: [`specs/syauth/ROADMAP.md` §S-003](../syauth/ROADMAP.md)
- SPEC: [`specs/syauth/SPEC.md`](../syauth/SPEC.md) §4.2 (replay cache size /
  TTL), §6 T-002 (Replay mitigation).
- Implementation files: `crates/syauth-core/src/replay.rs`, `crates/syauth-core/src/lib.rs`
  (module declaration and re-exports).
- Test files: `crates/syauth-core/src/replay.rs` `#[cfg(test)] mod tests`.
- Dependency surface: no new crates added. Cache is built on `std::time::{Duration, Instant}`
  and `std::collections::VecDeque` only.
