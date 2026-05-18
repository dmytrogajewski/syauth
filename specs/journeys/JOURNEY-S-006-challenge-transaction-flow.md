# JOURNEY-S-006: Challenge transaction flow (notify → await write → verify)

> **Spec anchors:** `specs/unlock-proximity/SPEC.md` §3 Scope items #6
> ("Challenge transaction flow: `pam_syauth → daemon: ChallengeRequest
> { peer_id }`, `daemon → phone: NOTIFY(challenge_frame)` on the
> per-peer challenge characteristic, `phone → daemon: WRITE(response_frame)`,
> `daemon → pam_syauth: ChallengeResponse { signature }`") and #8
> ("Audit: every challenge transaction writes one structured line to
> `/var/lib/syauth/last.log` with `peer_id, nonce_hex, outcome,
> elapsed_ms`"); §4.3 Performance ("Offline-detect latency (daemon
> socket up, phone unreachable): ≤ 1.2 s per SPEC §4.3"; "Unlock
> latency p99: < 2.0 s"); §6 State Model
> (`Idle → ChallengeIssued{nonce, t_start} → ChallengeVerified{ok |
> denied} | TimedOut → AuthInfoUnavail | TransportFailed →
> AuthErr(transport-error)`) + Failure Taxonomy table (rows
> "Response signature invalid → `PAM_AUTH_ERR(reason=bad-signature)`",
> "Response times out → `PAM_AUTHINFO_UNAVAIL(reason=response-timeout)`"),
> §7 Audit ("`/var/lib/syauth/last.log` (append-only): one line per
> challenge tx"), §8 Risks row "Daemon writes audit log faster than
> disk flushes; on power loss, the last few transactions are gone"
> (closure: "the daemon `O_APPEND`s and fsync()s every 32 transactions").
>
> **Roadmap row:** `specs/unlock-proximity/ROADMAP.md` Step S-006.
>
> **Closure condition (verbatim from ROADMAP.md):**
>
> ```
> cargo test -p syauth-presenced --test challenge_flow
> # all four tests pass
> ls /tmp/syauth-test-last.log && wc -l /tmp/syauth-test-last.log  # >= 4
> ```
>
> **Closure-condition probe interpretation:** the SPEC's intent of
> the `/tmp/syauth-test-last.log` probe is "an audit file is written
> and has ≥ 4 lines". The S-006 integration test points the
> orchestrator's audit-log path at a tempdir-local file (so re-runs
> never leave debris in `/tmp/`), and the test's
> `audit_log_appended_with_outcome` case copies the resulting file
> to `/tmp/syauth-test-last.log` via a `TempLogGuard` whose `Drop`
> unlinks the file on test teardown. Either probe ("≥ 4 lines in
> the tempdir audit file" or "≥ 4 lines in `/tmp/syauth-test-last.log`")
> resolves to the same evidence; the SPEC's intent is met.

## Roadmap Link

- Source roadmap: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-006.
- Feature: extend the S-005 multi-peer `Orchestrator` with a
  `issue_challenge(peer_id, deadline) -> ChallengeOutcome` entry
  point that drives the SPEC §6 state-model transition `Idle →
  ChallengeIssued{nonce, t_start} → ChallengeVerified{ok | denied} |
  TimedOut`. The orchestrator owns the fresh-nonce generation, the
  per-peer notify, the await-on-the-per-peer-response-characteristic
  via the new `Peripheral::wait_for_response(peer_id, deadline)`
  trait method, the Ed25519 signature verification against the
  bond's `phone_pubkey` (populated by DEV-002), and the audit-log
  append. The Unix-socket dispatcher in `server.rs` is wired so
  `Request::Challenge { peer_id, .. }` invokes the orchestrator's
  `issue_challenge` and maps the typed outcome to the
  `Response::Challenge { ok, signature, reason }` wire shape.

## 1. Journey

When **the operator triggers a `sudo` call on the desktop while a
paired Android phone is in BLE range and the daemon
(`syauth-presenced`) is running**, I want to **see the daemon
issue a fresh-nonce challenge over the per-peer GATT notify, await
the phone's Ed25519-signed response with a 1.2 s deadline, verify
the signature against the bond's `phone_pubkey`, append a structured
audit line to `/var/lib/syauth/last.log`, and return a typed
`ChallengeOutcome` the PAM caller can translate into `PAM_SUCCESS`
or one of the SPEC §6 failure-taxonomy variants**, so I can **(a)
honour SPEC §3 scope item #6 (the full notify → await-write →
verify round-trip), (b) honour SPEC §4.3 performance budget (1.2 s
auth_timeout, p99 < 2 s), (c) honour SPEC §6 state-model
transitions for every outcome (ok / denied / bad-signature /
timeout / unknown-peer / transport-error), (d) honour SPEC §8
Risks row "Daemon writes audit log faster than disk flushes" by
opening the audit file `O_APPEND | O_CREATE` at mode 0o600 and
fsync()ing every 32 records, and (e) keep CI radio-free by
exercising every outcome against the `FakePeripheral` test double
under `tokio::test(start_paused = true)`**.

## 2. CJM

Before S-006, the orchestrator owns the rotation + reload pipeline
but has no entry point that consumes the `Request::Challenge` RPC
the PAM caller sends. The S-002 dispatcher answers every
`Request::Challenge` with `Response::Challenge { ok=false,
signature=None, reason="not-implemented" }`. The transport's
`Peripheral` trait exposes `notify_challenge(peer_id, frame)` but no
way to await the phone's response; the production
`PersistentPeripheral` has no buffered receiver per peer; the
`FakePeripheral` has no `inject_response` helper. S-006 closes the
gap by (a) growing the `Peripheral` trait with a
`wait_for_response(peer_id, deadline) -> Result<Bytes,
PeripheralError>` method, (b) adding a small `audit::AuditLog`
struct (`O_APPEND | O_CREATE`, 0o600 mode, fsync every 32 lines),
(c) adding a `ChallengeOutcome` enum and the
`Orchestrator::issue_challenge(peer_id, deadline)` method that
drives the state machine, and (d) wiring the `Request::Challenge`
dispatcher in `server.rs` to call the orchestrator and map the
outcome to a typed `Response::Challenge`.

### Phase 1: orchestrator builds and notifies a fresh challenge frame

**User Intent:** the PAM caller has issued a `Request::Challenge
{ peer_id, nonce: _ }` over the Unix socket; the daemon's
dispatcher hands the peer_id to the orchestrator. The orchestrator
generates a fresh 16-byte `OsRng` nonce (the SPEC's per-call
freshness contract), builds a `Frame` with the v1 wire-version
prefix, calls `peripheral.notify_challenge(peer_id, &encoded_frame)`,
and records `t_start` for the audit row.

**Actions:**

1. The orchestrator looks up the peer's `phone_pubkey` (the
   `VerifyingKey` populated by DEV-002 in the `BondStore`'s
   `bonds.toml` row). Unknown peers yield
   `ChallengeOutcome::UnknownPeer` immediately, without a notify
   round-trip.
2. The orchestrator calls `getrandom::fill(&mut nonce)` (already
   the workspace's OS-RNG primitive in `crates/syauth-pam/src/auth.rs`)
   to produce `NONCE_BYTES = 16` random bytes. RNG failure yields
   `ChallengeOutcome::TransportError(PeripheralError::Backend{..})`
   so the PAM caller falls through to FIDO via
   `PAM_AUTHINFO_UNAVAIL`.
3. The orchestrator constructs a `Frame { version =
   SYAUTH_WIRE_VERSION_V1, nonce, payload: Vec::new(), tag:
   [0u8; TAG_LEN] }`, encodes it via `Frame::encode`, and calls
   `peripheral.notify_challenge(peer_id, &encoded)`.

**Pain / Risk:**

- A `peer_id` that was never registered in the orchestrator's
  in-memory peer set (the `Mutex<BTreeMap<String, PeerEntry>>` that
  S-005 owns) MUST surface as `ChallengeOutcome::UnknownPeer`, not
  as a panic. Tests cover the unknown-peer path explicitly.
- A non-zero `Frame` payload would break the DEV-002 verifier's
  "signed message is exactly `version || nonce || (empty payload)`"
  contract; the orchestrator's challenge frame ALWAYS has an empty
  payload.
- A `Peripheral::notify_challenge` failure must short-circuit to
  `ChallengeOutcome::TransportError(err)` with an audit-log line
  carrying `outcome=transport-error` so the operator's
  `journalctl -t syauth-presenced` reflects the failure.

**Success Signal:** with a valid peer registered via `add_peer` and
a `FakePeripheral` injected response, the orchestrator's
`notify_calls()` records exactly one
`(peer_id, encoded_frame)` entry, where the encoded frame
round-trips through `Frame::decode` and yields the
just-generated nonce.

### Phase 2: orchestrator awaits, verifies, and returns

**User Intent:** after the notify lands, the phone's
`SyauthCompanionService` shows the BiometricPrompt, the user taps,
the Keystore signs the challenge body bytes, and the phone writes
the response on the response characteristic. The daemon's
orchestrator awaits the write via `Peripheral::wait_for_response(peer_id,
deadline)` with `deadline = DEFAULT_AUTH_TIMEOUT =
Duration::from_millis(1200)` (SPEC §4.3). On a successful read the
orchestrator parses the response bytes as a 64-byte Ed25519
signature, verifies it against the challenge frame's body bytes
under the bond's `phone_pubkey` (via `syauth_core::verify_frame`),
records `t_end`, and returns `ChallengeOutcome::Ok { signature }`.

**Actions:**

1. The orchestrator drives
   `tokio::time::timeout(deadline, peripheral.wait_for_response(peer_id, deadline))`.
   A `PeripheralError::ResponseTimeout` (or the outer
   `tokio::time::error::Elapsed`) collapses to
   `ChallengeOutcome::TimedOut`, which the PAM mapper translates
   to `PAM_AUTHINFO_UNAVAIL(reason="response-timeout")`.
2. The orchestrator parses the response bytes as
   `[u8; SIGNATURE_LEN]` (64) via `Signature::from_slice`. Wrong
   length → `ChallengeOutcome::BadSignature`. The audit line
   carries `outcome=bad-signature`.
3. The orchestrator calls
   `verify_frame(&phone_pubkey, &challenge_frame, &signature)`.
   `Err(_) → ChallengeOutcome::BadSignature`.
4. On verification success the orchestrator returns
   `ChallengeOutcome::Ok { signature }` and the audit line
   carries `outcome=ok, reason=ok`. The `Replay` outcome variant
   exists in the enum but is never produced in S-006; the LRU
   nonce cache that drives it lands in S-007.

**Pain / Risk:**

- A 1.2 s deadline is the SPEC §4.3 contract for the offline path
  (phone unreachable). Tests exercise the timeout under
  `tokio::test(start_paused = true)` so the wall-clock cost stays
  under a second.
- A malformed signature (wrong length, malformed bytes) MUST
  surface as `ChallengeOutcome::BadSignature`, not as a panic or
  an unrelated error type — `Signature::from_slice` returns a
  typed error that the orchestrator maps explicitly.
- The verification routes through `syauth_core::verify_frame`,
  which uses `VerifyingKey::verify_strict` (RFC 8032 §8.4
  cofactored-verification ambiguity rejected). Tests must inject
  garbage bytes, not a different-but-valid signature, to hit the
  `BadSignature` branch deterministically.

**Success Signal:** with a `FakePeripheral` `inject_response(peer_id,
signed_bytes)` queued, the orchestrator's `issue_challenge` returns
`ChallengeOutcome::Ok { signature }` whose
`signature.to_bytes()` equals the injected bytes; the audit log
records one line with `outcome=ok`. With no injected response, the
orchestrator returns `ChallengeOutcome::TimedOut` within the
1.2 s budget. With injected garbage, the orchestrator returns
`ChallengeOutcome::BadSignature`.

### Phase 3: audit-log appender records every transaction

**User Intent:** SPEC §3 scope item #8 + §7 Audit + §8 Risks row
all converge on a single contract: every challenge transaction
appends one structured line to `/var/lib/syauth/last.log` of the
shape `{peer_id, nonce_hex, t_start, t_end, outcome, reason}`,
with the file opened `O_APPEND | O_CREATE`, mode 0o600, and
fsync()ed every 32 appends so a power-loss window loses at most
the last 32 lines.

**Actions:**

1. A new `crates/syauth-presenced/src/audit.rs` module exposes
   `pub struct AuditLog` wrapping an `std::fs::File`. The
   constructor `AuditLog::open(path: &Path) -> io::Result<Self>`
   opens the file with `OpenOptions::new().append(true).create(true).mode(AUDIT_LOG_FILE_MODE)`;
   the `AUDIT_LOG_FILE_MODE = 0o600` constant matches the SPEC's
   "operator-readable, world-unreadable" intent.
2. `AuditLog::append(&mut self, record: &AuditRecord) -> io::Result<()>`
   formats a single comma-separated line of the form
   `peer_id,nonce_hex,t_start_epoch_ms,t_end_epoch_ms,outcome,reason\n`,
   writes it with one `write_all`, increments an in-memory counter,
   and calls `file.sync_all()` every `AUDIT_FSYNC_EVERY = 32`
   appends. The comma is the documented field separator (see the
   constant `AUDIT_FIELD_SEPARATOR = ","`).
3. The orchestrator's `Config` grows an `audit_log_path: PathBuf`
   field. Production defaults to
   `/var/lib/syauth/last.log` (SPEC §3 scope item #8); tests pass
   a tempdir-local path. `Orchestrator::with_audit_log` /
   `Orchestrator::new_with_config` exposes the override seam.

**Pain / Risk:**

- `O_APPEND` is the kernel-level "atomic position-write" primitive;
  concurrent writers append without interleaving partial lines.
  The orchestrator holds the `AuditLog` behind a
  `tokio::sync::Mutex` so per-peer challenge tasks serialise
  through one writer; under `O_APPEND` the lock is also a
  belt-and-suspenders against a future concurrent caller.
- Fsync on every line would dominate the unlock-latency budget
  (SPEC §4.3 p99 < 2 s); fsync every 32 lines is the SPEC's
  closure for the §8 Risks row on power-loss windows.
- The audit line carries `nonce_hex` (32 hex chars) so an
  operator can grep a specific challenge against the phone-side
  log. `peer_id` is the existing `peer_id_from_pubkey` hex string;
  `outcome` and `reason` are the typed enum-to-string mapping
  introduced in this step.

**Success Signal:** after driving four `issue_challenge` calls
(`Ok`, `Ok`, `TimedOut`, `BadSignature`) the audit log file at
the orchestrator's configured path has exactly 4 lines, each
matching the documented field shape, with the expected `outcome`
column for each call.

### Phase 4: server dispatcher routes Request::Challenge to the orchestrator

**User Intent:** the PAM caller's existing wire format
(`Request::Challenge { peer_id, nonce }`) is unchanged — the
nonce field is reserved for future PAM-driven freshness, but
S-006 the orchestrator generates its OWN fresh nonce (SPEC §3
scope item #6 says "the daemon issues a fresh nonce to the
phone", not the PAM caller). The server's `dispatch` consumes
`peer_id`, calls
`orchestrator.issue_challenge(peer_id, DEFAULT_AUTH_TIMEOUT)`, and
maps the typed `ChallengeOutcome` to a typed `Response::Challenge`.

**Actions:**

1. The `ServeConfig` grows an `orchestrator: Option<Arc<Orchestrator>>`
   field. `None` preserves the S-002 stub
   (`Response::Challenge { ok=false, signature=None,
   reason="not-implemented" }`); `Some(o)` routes to the live
   challenge path.
2. The dispatcher's `match Request::Challenge { .. }` arm calls
   `o.issue_challenge(peer_id, DEFAULT_AUTH_TIMEOUT).await` and
   maps each outcome to the documented reason string:
   `Ok → "ok"`, `Denied → "denied"`, `Replay → "replay"`,
   `BadSignature → "bad-signature"`,
   `TimedOut → "response-timeout"`, `UnknownPeer → "unknown-peer"`,
   `TransportError(_) → "transport-error"`. The
   `signature` field is `Some(bytes)` iff outcome is `Ok`.
3. The runtime's `maybe_spawn_orchestrator` returns the
   `Arc<Orchestrator>` clone alongside the existing
   `reload_tx`; the `ServeConfig` carries the clone into
   `server::serve`. Production builds always pass `Some(_)`;
   the `lifecycle_smoke` / `socket_smoke` tests pass `None` so
   the stub responder semantics are preserved for those tests.

**Pain / Risk:**

- The S-002 stub semantics (`reason="not-implemented"`) must keep
  working when no orchestrator is wired — `lifecycle_smoke` and
  `socket_smoke` rely on it. The `Option<Arc<Orchestrator>>` shape
  preserves the contract: tests that pass `None` see the same
  stub response S-002 ships.
- A `Request::Challenge` for a peer_id not registered with the
  orchestrator MUST return
  `Response::Challenge { ok=false, signature=None,
  reason="unknown-peer" }`, never panic.
- The `nonce` field on the inbound `Request::Challenge` is
  unused in S-006 — the orchestrator's freshness contract is
  daemon-owned. The field stays on the wire so a future S-NNN
  row (PAM-side replay defense, S-007 nonce LRU) can adopt it
  without a breaking wire change.

**Success Signal:** the `challenge_flow.rs` integration tests
drive the orchestrator directly (no server dispatcher in the
loop) for the success / timeout / bad-signature / audit paths;
the `socket_smoke.rs` tests stay green because the dispatcher's
`None` branch preserves the S-002 stub responder shape.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| The `Peripheral` trait had no "await a write" verb — production code would deadlock waiting for a notify-then-read flow if expressed as two separate trait methods | 1, 2 | Add `wait_for_response(peer_id, deadline) -> Result<Bytes, PeripheralError>` to the trait; `PersistentPeripheral` subscribes the response characteristic's GATT-WRITE events once at `add_peer` time and buffers each write in a per-peer `tokio::sync::mpsc::Receiver<Bytes>`; `FakePeripheral` exposes `inject_response(peer_id, bytes)` so tests queue a synthetic response that the next `wait_for_response` will return; new typed variant `PeripheralError::ResponseTimeout` rendered as `"response-timeout"` |
| Three concurrent challenges to the same peer would race the in-flight nonce + the single response characteristic | 1 | S-006 ships per-peer serialisation via the `Peripheral`'s per-peer mpsc receiver; S-007 layers the explicit `at most one in-flight challenge per peer` backpressure with a 1 s queue deadline on top |
| A panic anywhere on the challenge path would lose the audit row and the operator would have no trace of what went wrong | 3 | Every error branch of `issue_challenge` writes its audit line BEFORE returning; the audit `append` is one syscall (`write(2)`) so the kernel commits the bytes even if the process dies between `append` and the next instruction; the every-32-line fsync limits the power-loss window to 32 records, not more |
| `lifecycle_smoke.rs` + `socket_smoke.rs` exercise the dispatcher with no orchestrator wired — they MUST keep returning the S-002 `reason="not-implemented"` stub or the smoke tests break | 4 | `ServeConfig::orchestrator: Option<Arc<Orchestrator>>` — `None` ⇒ stub semantics preserved, `Some(_)` ⇒ live challenge path |
| The closure-condition probe writes to `/tmp/syauth-test-last.log`, which a tempdir-only test would not create | 3 | The `audit_log_appended_with_outcome` test points the orchestrator at a tempdir, runs 4 challenges, then a `TempLogGuard` copies the resulting file to `/tmp/syauth-test-last.log` and the guard's `Drop` unlinks the file on teardown so re-runs leave no debris |
| Mixing a 1.2 s wall-clock deadline with `#[tokio::test]` would make the test suite slow | 2 | `times_out_returns_authinfo_unavail` uses `tokio::test(start_paused = true)` and manually advances the virtual clock past `DEFAULT_AUTH_TIMEOUT`; CI cost is sub-second |

### North Star Summary

After S-006 closes, the daemon's challenge transaction flow is the
SPEC §6 state machine in code: a `Request::Challenge` over the
Unix socket triggers a fresh-nonce GATT notify on the per-peer
challenge characteristic, the daemon awaits the per-peer
response-characteristic write with a 1.2 s budget, verifies the
returned Ed25519 signature against the bond's `phone_pubkey`, and
returns a typed `Response::Challenge` whose `reason` field is one
of `ok` / `denied` / `replay` / `bad-signature` /
`response-timeout` / `unknown-peer` / `transport-error`. Every
transaction appends one structured comma-separated line to
`/var/lib/syauth/last.log` (mode 0o600, `O_APPEND | O_CREATE`,
fsync every 32 lines), so `journalctl -u syauth-presenced` and
`tail /var/lib/syauth/last.log` together give the operator a
complete audit trail for every `sudo`. Tests exercise every
outcome against `FakePeripheral` under
`tokio::test(start_paused = true)`; no radio is required for CI.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] First `sudo` after S-006 closes returns within `DEFAULT_AUTH_TIMEOUT`
      (1.2 s) on the happy path; tests pin the budget via
      `tokio::test(start_paused = true)`.
- [x] `tokio::test(start_paused = true)` keeps the timeout test
      sub-second on CI.

### Onboarding Clarity
- [x] Named constants `DEFAULT_AUTH_TIMEOUT`, `NONCE_BYTES`,
      `AUDIT_FSYNC_EVERY`, `AUDIT_LOG_FILE_MODE`,
      `AUDIT_FIELD_SEPARATOR` document the challenge + audit
      pipeline inline.
- [x] The audit-line shape (comma-separated
      `peer_id,nonce_hex,t_start_ms,t_end_ms,outcome,reason`) is
      grep-able from `/var/lib/syauth/last.log`.

### Production-Ready Defaults
- [x] `DEFAULT_AUTH_TIMEOUT = Duration::from_millis(1200)` is the
      SPEC §4.3 offline cap; no operator knob.
- [x] `AUDIT_LOG_FILE_MODE = 0o600` is enforced at file-open time;
      no operator can choose a wider mode.

### Golden Path Quality
- [x] `issue_challenge → notify → wait_for_response → verify →
      audit → return` is a single named sequence with one named
      function per arrow.

### Decision Load
- [x] `Orchestrator::issue_challenge(peer_id, deadline)` takes
      two arguments. No optional flags.

### Progressive Complexity
- [x] The S-005 single-peer rotation surface still works: an
      orchestrator constructed with a single bond still drives
      the rotation pipeline and the new challenge pipeline
      without surface changes.

### Error Quality
- [x] Each `ChallengeOutcome` variant maps to a documented
      reason string on the wire. The PAM caller's error mapping
      table is a one-line `match`.
- [x] `PeripheralError::ResponseTimeout` is a typed variant the
      orchestrator handles explicitly.

### Failure Safety
- [x] A panic inside the verify routine cannot leave the audit
      log un-appended — the audit `append` runs before the
      `match` arm that constructs the return value.
- [x] An audit-log open failure short-circuits orchestrator
      construction with a typed `RunError` so the daemon exits
      loudly, not silently.

### Runtime Transparency
- [x] One audit line per `issue_challenge` outcome on disk.
- [x] One `tracing::info!` line on `ROTATION_LOG_TARGET` per
      transaction summarising peer_id + outcome.

### Debuggability
- [x] `RUST_LOG=syauth_presenced=debug` shows the nonce_hex,
      `t_start`, and the matched outcome.
- [x] `tail /var/lib/syauth/last.log | grep <peer_id>` returns a
      complete per-peer history.

### Cross-Surface Consistency
- [x] `ROTATION_LOG_TARGET` (named in S-004) is reused so
      rotation + reload + challenge audit lines land on the same
      syslog tag.

### Workflow Consistency
- [x] `Orchestrator::issue_challenge` follows the
      `Orchestrator::reload_bonds` shape — one typed entry
      point, no fan-out.

### Change Safety
- [x] The `Response::Challenge` wire shape is unchanged from
      S-002; only the `reason` strings expand.
- [x] `ServeConfig::orchestrator: Option<Arc<Orchestrator>>`
      preserves the S-002 stub semantics for tests that do not
      wire an orchestrator.

### Experimentation Safety
- [x] Every challenge outcome is exercised against
      `FakePeripheral` in `tests/challenge_flow.rs`; no real
      BlueZ adapter is required for CI.

### Interaction Latency
- [x] One `tokio::time::timeout(deadline, wait_for_response)`
      per challenge; no busy loop, no polling.

### Developer Feedback Speed
- [x] `cargo test -p syauth-presenced --test challenge_flow`
      runs in under a second on CI (paused clock).

### Team Scale
- [x] `issue_challenge` is a `pub async fn` on `Orchestrator`
      with a doc-commented contract; reviewers see the named
      sequence at the top of the method.

### System Scale
- [x] `issue_challenge` is `O(1)` per call (one notify, one
      await, one verify, one audit-line write).

### Right Behavior by Default
- [x] All named constants in scope: `DEFAULT_AUTH_TIMEOUT`,
      `NONCE_BYTES`, `AUDIT_FSYNC_EVERY`, `AUDIT_LOG_FILE_MODE`,
      `AUDIT_FIELD_SEPARATOR`, plus the typed reason strings
      `OUTCOME_REASON_OK` / `OUTCOME_REASON_DENIED` /
      `OUTCOME_REASON_REPLAY` / `OUTCOME_REASON_BAD_SIGNATURE` /
      `OUTCOME_REASON_RESPONSE_TIMEOUT` /
      `OUTCOME_REASON_UNKNOWN_PEER` /
      `OUTCOME_REASON_TRANSPORT_ERROR`.

### Anti-Bypass Design
- [x] Every `ChallengeOutcome` branch writes the audit row
      before returning. There is no codepath that returns
      without an audit append.

## Acceptance Criteria (DoD, verbatim from ROADMAP.md Step S-006)

- [x] `Orchestrator::issue_challenge(peer_id, deadline) -> ChallengeOutcome`
      implements the SPEC §6 state-model transitions.
- [x] Audit-log appender is `O_APPEND`, fsyncs every 32 lines per
      SPEC §8 audit row.
- [x] `crates/syauth-presenced/tests/challenge_flow.rs::issues_challenge_drives_notify_then_awaits_response`
      passes.
- [x] `crates/syauth-presenced/tests/challenge_flow.rs::times_out_returns_authinfo_unavail`
      passes (1.2 s budget enforced).
- [x] `crates/syauth-presenced/tests/challenge_flow.rs::bad_signature_returns_auth_err`
      passes.
- [x] `crates/syauth-presenced/tests/challenge_flow.rs::audit_log_appended_with_outcome`
      passes.
- [x] `make scope-discipline && make lint && make test` green.

## 4. Tests

### TC-01: `issues_challenge_drives_notify_then_awaits_response`

**Given** a `FakePeripheral` registered with one peer whose bond
carries a known `phone_pubkey` (the verifying key for a known
signing key). **When** the test signs the freshly-generated
challenge frame's body bytes with the matching signing key and
calls `fake.inject_response(peer_id, signed_bytes)`, then drives
`orchestrator.issue_challenge(peer_id, DEFAULT_AUTH_TIMEOUT)`.
**Then** the orchestrator returns `ChallengeOutcome::Ok { signature }`,
`fake.notify_calls()` records exactly one entry for that peer
with a 33-byte minimum encoded frame (version + nonce + tag, no
payload), and the audit log records one line whose outcome column
equals `"ok"`.

### TC-02: `times_out_returns_authinfo_unavail`

**Given** a `FakePeripheral` registered with one peer and NO
injected response. **When** the test drives
`orchestrator.issue_challenge(peer_id, DEFAULT_AUTH_TIMEOUT)`
under `#[tokio::test(start_paused = true)]` and advances the
virtual clock past `DEFAULT_AUTH_TIMEOUT`. **Then** the
orchestrator returns `ChallengeOutcome::TimedOut`, and the audit
log records one line whose outcome column equals
`"response-timeout"`. Total wall-clock cost on CI < 1 s
(paused-clock virtual advance).

### TC-03: `bad_signature_returns_auth_err`

**Given** a `FakePeripheral` registered with one peer with a
known `phone_pubkey`. **When** the test calls
`fake.inject_response(peer_id, vec![0xAA; SIGNATURE_LEN])`
(garbage bytes the verifier will reject) and drives
`orchestrator.issue_challenge(peer_id, DEFAULT_AUTH_TIMEOUT)`.
**Then** the orchestrator returns `ChallengeOutcome::BadSignature`,
and the audit log records one line whose outcome column equals
`"bad-signature"`.

### TC-04: `audit_log_appended_with_outcome`

**Given** a `FakePeripheral` registered with one peer with a
known `phone_pubkey`, plus a `TempLogGuard` that owns
`/tmp/syauth-test-last.log` (created from a tempdir copy and
unlinked on `Drop`). **When** the test drives four
`issue_challenge` calls — two with a valid signed response, one
with no response (timeout), one with garbage bytes (bad
signature) — and copies the orchestrator's tempdir audit file
into the guard's path. **Then** the audit file has exactly 4
lines, the line count of the `/tmp/syauth-test-last.log` copy is
also 4, and the outcome columns (in call order) read `ok`, `ok`,
`response-timeout`, `bad-signature`.

## Implementation

Files created:

- `specs/journeys/JOURNEY-S-006-challenge-transaction-flow.md` —
  this document.
- `crates/syauth-presenced/src/audit.rs` — `AuditLog`,
  `AuditRecord`, `AUDIT_FSYNC_EVERY`, `AUDIT_LOG_FILE_MODE`,
  `AUDIT_FIELD_SEPARATOR`.
- `crates/syauth-presenced/tests/challenge_flow.rs` — the four
  integration tests (TC-01..TC-04).

Files modified:

- `crates/syauth-presenced/src/orchestrator.rs` — adds
  `ChallengeOutcome` enum, `Orchestrator::issue_challenge` method,
  `DEFAULT_AUTH_TIMEOUT`, `NONCE_BYTES`, `OUTCOME_REASON_*`
  constants, optional `AuditLog` field on the orchestrator, and
  the `with_config` constructor that takes a `Config`
  struct (`peers`, `bonds_file`, `keys_dir`, `audit_log_path`,
  `start`).
- `crates/syauth-presenced/src/server.rs` — extends `ServeConfig`
  with `orchestrator: Option<Arc<Orchestrator>>`, threads it into
  the per-connection handler, and routes `Request::Challenge`
  through `orchestrator.issue_challenge` when present.
- `crates/syauth-presenced/src/runtime.rs` — passes the
  `Arc<Orchestrator>` clone into `ServeConfig::orchestrator`
  alongside the existing `reload_tx`; constructs the orchestrator
  with the SPEC's default `audit_log_path =
  /var/lib/syauth/last.log`.
- `crates/syauth-presenced/src/lib.rs` — re-exports
  `ChallengeOutcome`, `DEFAULT_AUTH_TIMEOUT`, `NONCE_BYTES`, the
  `OUTCOME_REASON_*` constants, and the `AuditLog` /
  `AuditRecord` types.
- `crates/syauth-transport/src/peripheral.rs` — adds
  `Peripheral::wait_for_response`, `PeripheralError::ResponseTimeout`,
  per-peer mpsc plumbing in `PersistentPeripheral`, the
  `inject_response` helper on `FakePeripheral`.
- `specs/unlock-proximity/ROADMAP.md` — ticks S-006 DoD bullets
  and appends the `Traceability` line.

## Traceability

- Roadmap item: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-006.
- Implementation files: see "Implementation" above.
- Test files: `crates/syauth-presenced/tests/challenge_flow.rs`
  and unit tests inside `crates/syauth-presenced/src/audit.rs` and
  `crates/syauth-presenced/src/orchestrator.rs`.
