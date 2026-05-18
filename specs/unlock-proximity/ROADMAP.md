# ROADMAP: unlock-via-phone proximity + handshake path

Source spec: `specs/unlock-proximity/SPEC.md`. Read it first — every
DoR below assumes the §3 Proposal and §4 Technical Design are in your
head, and every closure condition points back to a SPEC clause (§3.2
D1–D8, §3.3 ML, §4.3 latency).

## How to read this roadmap

- **Steps S-001..S-019** decompose the SPEC's 24 in-scope items into
  independently shippable, independently testable increments.
- Each step has a **Description**, **DoR**, **DoD**, **Files likely
  affected**, and a **Closure condition** (a greppable test name or
  `cargo`/`gradle` invocation that proves the step is done).
- A step is closed when every DoD bullet is `[x]` AND its closure
  condition produces the documented output AND `make scope-discipline`,
  `make lint`, `make test`, `:app:assembleDebug`, and
  `:app:testDebugUnitTest` are all green (Android gates apply only to
  steps that touch phone code).
- No step depends on a later step. A step may depend on EARLIER steps
  via its DoR. Cross-cutting reorders (e.g., pulling S-008 before
  S-006) require updating both rows' DoRs in this file.
- **No estimations.** Per AGENTS.md, do not annotate steps with hours,
  days, story points, or t-shirt sizes. Each step is "open" or
  "closed"; size is whatever the journey doc says it is when the
  subagent finishes Part B.

## Existing assets to integrate (don't rebuild)

| Existing | Location | Plan |
|---|---|---|
| `BluerAdvertiser` | `crates/syauth-transport/src/bluez_advertise.rs:157` | Extract reusable peripheral/advertisement bits into a library API the daemon consumes; the per-PAM-call short-burst mode is deleted in S-009 |
| `BondStore`, `Bond`, `Frame`, `verify_response` | `crates/syauth-core/src/` | Reused by both daemon and (read-only) PAM client; no rewrites |
| `AndroidCdmPairCompanionScanner` | `syauth-android/.../pair/impl/` | Kept as belt-and-suspenders for the service watchdog (S-012); never the primary connection path |
| `DirectGattController.kt` | `syauth-android/.../bg/` | Removed in S-013 — replaced by `PersistentGattClient` |
| `pam_sm_authenticate` shell | `crates/syauth-pam/src/auth.rs` | Body rewritten in S-008 to talk to the daemon over Unix socket; the `BluerAdvertiser` call site at `auth.rs:575` goes away |
| `session_uuid_for(bond_key, minute)` | `crates/syauth-core/src/` | Unchanged; consumed by daemon's per-minute rotator (S-004) |
| `keys/<peer_id>.bin` writer | `crates/syauth-cli/src/pair.rs::write_pam_bond_key` (tonight) | Daemon reads the same files in S-005 |

## Dependency graph (high level)

```
  S-001 ── S-002 ── S-003 ── S-004 ── S-005 ── S-006 ── S-007
  scaffold  socket   BLE       rotate    multi    chal-     nonce
                     extract            peer     lenge      LRU/
                                                 flow       backpressure
                                                            │
                                                            ▼
                                                          S-008 ── S-009
                                                          PAM      install
                                                          rewrite  glue
                                                            │
                                                            ▼
   S-010 ── S-011 ── S-012 ── S-013 ── S-014 ── S-015
   GATT     fg-svc    boot+    remove    approval  biometric+
   client   parent    watchdog DirectGatt activity  Keystore
                                                            │
                                                            ▼
   S-016 ── S-017 ── S-018 ── S-019
   doctor   status    notif.   e2e
                      polish   bench/gate
```

S-010..S-015 can run in parallel with S-001..S-009 (no shared files).
S-018..S-019 require everything above them.

---

## Step S-001: Scaffold `syauth-presenced` crate + binary + systemd unit

**Description:** Create a new workspace member `crates/syauth-presenced/`
with a `main.rs` that parses `--socket`, `--bonds-file`, `--keys-dir`,
`--log-level` args, sets up `tracing_subscriber` to syslog tag
`syauth-presenced`, opens a single-instance lock at
`${XDG_RUNTIME_DIR}/syauth/presenced.pid`, and runs an empty tokio
loop until `SIGINT`/`SIGTERM`. Ship the systemd user unit at
`crates/syauth-presenced/dist/syauth-presenced.service` and a smoke
test that starts the binary, asserts the PID file appears, sends
`SIGTERM`, asserts clean exit.

**DoR:**
- Workspace `Cargo.toml` has space for a new member (it always does).
- AGENTS.md `Scope Discipline` re-read; no banned vocabulary.

**DoD:**
- [x] `crates/syauth-presenced/` exists with `Cargo.toml`, `src/main.rs`,
      `src/lib.rs`, `dist/syauth-presenced.service`.
- [x] Workspace `Cargo.toml` lists the new crate.
- [x] `cargo build -p syauth-presenced --release` succeeds.
- [x] `crates/syauth-presenced/tests/lifecycle_smoke.rs::starts_and_terminates_cleanly` passes.
- [x] `crates/syauth-presenced/tests/lifecycle_smoke.rs::refuses_second_instance` passes.
- [x] `make scope-discipline && make lint && make test` green.

**Files likely affected:**
- `Cargo.toml` (workspace)
- `crates/syauth-presenced/Cargo.toml` (new)
- `crates/syauth-presenced/src/main.rs` (new)
- `crates/syauth-presenced/src/lib.rs` (new)
- `crates/syauth-presenced/dist/syauth-presenced.service` (new)
- `crates/syauth-presenced/tests/lifecycle_smoke.rs` (new)

**Closure condition:**
```
cargo test -p syauth-presenced --test lifecycle_smoke
# both tests pass; binary exit code 0; pid file removed on shutdown
```

**Traceability:** journey at `specs/journeys/JOURNEY-S-001-scaffold-syauth-presenced.md`; implementation in `Cargo.toml`, `crates/syauth-presenced/Cargo.toml`, `crates/syauth-presenced/src/lib.rs`, `crates/syauth-presenced/src/main.rs`, `crates/syauth-presenced/src/runtime.rs`, `crates/syauth-presenced/src/lock.rs`, `crates/syauth-presenced/dist/syauth-presenced.service`, `crates/syauth-presenced/tests/lifecycle_smoke.rs`; closed 2026-05-18.

---

## Step S-002: CBOR-framed Unix-socket RPC server (stub responder)

**Description:** Add `crates/syauth-presenced/src/rpc.rs` defining the
typed `Request` / `Response` enum (`ChallengeRequest { peer_id, nonce }`,
`ChallengeResponse { ok, signature, reason }`, `StatusRequest`,
`StatusResponse { peers: Vec<PeerStatus>, started_at }`,
`ReloadRequest`, `ReloadResponse { ok }`) with `ciborium` encode/decode
and a 4-byte big-endian length prefix. The server binds
`${XDG_RUNTIME_DIR}/syauth/auth.sock` with mode `0600`, enforces
`SO_PEERCRED` matches the daemon's UID on every accept, and responds
to every `ChallengeRequest` with a stubbed `ChallengeResponse { ok:
false, reason: "not-implemented" }`. Concurrent accept cap = 4 per
SPEC §7 T-Daemon-DoS.

**DoR:** S-001 closed.

**DoD:**
- [x] `rpc.rs` defines the typed enum with `serde` derives.
- [x] Round-trip unit test in `rpc.rs::tests` for every variant.
- [x] `crates/syauth-presenced/tests/socket_smoke.rs::challenge_request_returns_stub` passes.
- [x] `crates/syauth-presenced/tests/socket_smoke.rs::rejects_non_matching_peer_credential` passes (drops connection if `SO_PEERCRED.uid` mismatches).
- [x] `crates/syauth-presenced/tests/socket_smoke.rs::concurrent_accept_cap_enforced` passes.
- [x] Socket file mode is `0600`; verified by the smoke test.
- [x] `make scope-discipline && make lint && make test` green.

**Files likely affected:**
- `crates/syauth-presenced/src/rpc.rs` (new)
- `crates/syauth-presenced/src/main.rs` (wire the server)
- `crates/syauth-presenced/tests/socket_smoke.rs` (new)

**Closure condition:**
```
cargo test -p syauth-presenced --test socket_smoke
# all three tests pass
```

**Traceability:** journey at `specs/journeys/JOURNEY-S-002-cbor-unix-socket-rpc-stub.md`; implementation in `crates/syauth-presenced/Cargo.toml`, `crates/syauth-presenced/src/lib.rs`, `crates/syauth-presenced/src/main.rs`, `crates/syauth-presenced/src/rpc.rs`, `crates/syauth-presenced/src/runtime.rs`, `crates/syauth-presenced/src/server.rs`, `crates/syauth-presenced/tests/socket_smoke.rs`; closed 2026-05-18.

---

## Step S-003: Extract BLE peripheral library API from `BluerAdvertiser`

**Description:** Refactor `crates/syauth-transport/src/bluez_advertise.rs`
to expose a library API the daemon can hold across many PAM calls,
without changing the per-PAM-call behavior of the existing
`BluerAdvertiser` used by `pam_syauth` today. Introduce a new
`PersistentPeripheral` type that owns a `bluer::Adapter`, a
`bluer::adv::AdvertisementHandle`, and a
`bluer::gatt::local::ApplicationHandle`, with methods `add_peer`,
`remove_peer`, `set_session_uuids`, and `notify_challenge`. Keep the
old `BluerAdvertiser` working for backward-compatibility with the
existing PAM module (it's deleted in S-009). Unit tests use a
`bluer`-free mock trait `Peripheral` so they run on CI without a
radio.

**DoR:** S-001 closed (you need the daemon shape locked).

**DoD:**
- [x] `crates/syauth-transport/src/peripheral.rs` (new) defines
      `trait Peripheral` with the four methods above.
- [x] `PersistentPeripheral` implements `Peripheral` over `bluer 0.17`.
- [x] Mock `FakePeripheral` for tests.
- [x] Existing `BluerAdvertiser` API surface UNCHANGED; existing PAM
      tests still pass (regression check).
- [x] `crates/syauth-transport/tests/peripheral_contract.rs::add_remove_peer_roundtrip`
      passes against `FakePeripheral`.
- [x] `crates/syauth-transport/tests/peripheral_contract.rs::set_session_uuids_replaces_advertisement`
      passes.
- [x] `make scope-discipline && make lint && make test` green.

**Files likely affected:**
- `crates/syauth-transport/src/peripheral.rs` (new)
- `crates/syauth-transport/src/lib.rs` (re-export)
- `crates/syauth-transport/src/bluez_advertise.rs` (cleanup only)
- `crates/syauth-transport/tests/peripheral_contract.rs` (new)

**Closure condition:**
```
cargo test -p syauth-transport --test peripheral_contract
# both tests pass
git grep -l "BluerAdvertiser" crates/syauth-pam/   # still 1+ files (PAM still uses it)
```

**Traceability:** journey at `specs/journeys/JOURNEY-S-003-peripheral-library-api.md`; implementation in `crates/syauth-transport/src/peripheral.rs`, `crates/syauth-transport/src/lib.rs`, `crates/syauth-transport/Cargo.toml`, `crates/syauth-transport/tests/peripheral_contract.rs`, `crates/syauth-pam/Cargo.toml`, `crates/syauth-cli/Cargo.toml`; closed 2026-05-18.

---

## Step S-004: Per-minute session-UUID rotation in the daemon

**Description:** Wire `syauth-presenced` to load ONE bond (the first
non-revoked entry in `/var/lib/syauth/bonds.toml`), keep a
`PersistentPeripheral` open for its lifetime, and rotate the
advertised `service_uuids` set on each wall-clock minute boundary
using `session_uuid_for(bond_key, minute)` from `syauth-core`. A
tokio `interval_at` aligned to the next minute drives rotation. On
each rotation, syslog `syauth-presenced: rotated id=<peer> minute=<N> uuid=<short>`.
SPEC §3.2 D8 single-bond case only — multi-peer arrives in S-005.

**DoR:** S-002 and S-003 closed.

**DoD:**
- [x] `crates/syauth-presenced/src/orchestrator.rs::Orchestrator`
      owns the `Peripheral` handle + the bond-list state + the
      rotation timer.
- [x] Rotation timer aligns to the next wall-clock minute boundary
      (not "every 60s from start").
- [x] `crates/syauth-presenced/tests/rotation.rs::rotates_at_minute_boundary`
      uses `tokio::time::pause` + `FakePeripheral` to assert exactly
      one `set_session_uuids` call per simulated minute, with
      `session_uuid_for` output verified.
- [x] `crates/syauth-presenced/tests/rotation.rs::syslog_emits_rotation_line`
      asserts the audit line shape.
- [x] `make scope-discipline && make lint && make test` green.

**Files likely affected:**
- `crates/syauth-presenced/src/orchestrator.rs` (new)
- `crates/syauth-presenced/src/main.rs` (wire it)
- `crates/syauth-presenced/tests/rotation.rs` (new)

**Closure condition:**
```
cargo test -p syauth-presenced --test rotation
# both tests pass
```

**Traceability:** journey at `specs/journeys/JOURNEY-S-004-session-uuid-rotation.md`; implementation in `crates/syauth-presenced/src/orchestrator.rs`, `crates/syauth-presenced/src/lib.rs`, `crates/syauth-presenced/src/runtime.rs`, `crates/syauth-presenced/Cargo.toml`, `crates/syauth-presenced/tests/rotation.rs`; closed 2026-05-18.

---

## Step S-005: Multi-peer advertise + bonds.toml watch + SIGHUP reload

**Description:** Extend the orchestrator to handle N bonded peers
simultaneously. The advertisement carries the union of all per-peer
rotating UUIDs. Three sources trigger a bond-list refresh: `SIGHUP`,
the `Reload` RPC over the socket, and `inotify` on the bonds file
(belt-and-suspenders). On reload the orchestrator diffs the new bond
set against the live `Peripheral` peer set and emits the minimal
`add_peer` / `remove_peer` calls. SPEC scope items §3 #4 and #10.

**DoR:** S-004 closed.

**DoD:**
- [x] `Orchestrator::reload_bonds(&BondStore)` does a precise diff.
- [x] SIGHUP triggers `reload_bonds` (signal handler in main loop).
- [x] `Reload` RPC triggers `reload_bonds` and returns `ok=true`.
- [x] inotify-on-bonds.toml triggers `reload_bonds` (debounced 200 ms).
- [x] `crates/syauth-presenced/tests/multi_peer.rs::three_bonds_advertise_three_uuids`
      passes.
- [x] `crates/syauth-presenced/tests/multi_peer.rs::reload_removes_revoked_bond`
      passes.
- [x] `crates/syauth-presenced/tests/multi_peer.rs::sighup_reloads_bond_set`
      passes.
- [x] `make scope-discipline && make lint && make test` green.

**Files likely affected:**
- `crates/syauth-presenced/src/orchestrator.rs`
- `crates/syauth-presenced/src/main.rs`
- `crates/syauth-presenced/tests/multi_peer.rs` (new)

**Closure condition:**
```
cargo test -p syauth-presenced --test multi_peer
# all three tests pass
```

**Traceability:** journey at `specs/journeys/JOURNEY-S-005-multi-peer-bonds-reload.md`; implementation in `crates/syauth-presenced/src/orchestrator.rs`, `crates/syauth-presenced/src/runtime.rs`, `crates/syauth-presenced/src/server.rs`, `crates/syauth-presenced/src/lib.rs`, `crates/syauth-presenced/Cargo.toml`, `crates/syauth-core/src/bond.rs` (`Bond::is_revoked`), `crates/syauth-presenced/tests/multi_peer.rs`; closed 2026-05-18.

---

## Step S-006: Challenge transaction flow (notify → await write → verify)

**Description:** Wire `ChallengeRequest { peer_id }` to the
orchestrator's challenge state machine: build a `Frame` with a
fresh 16-byte `OsRng` nonce, call `peripheral.notify_challenge(peer_id, frame)`,
await a write on the per-peer response characteristic with deadline =
`auth_timeout` (defaults to 1.2 s per SPEC §4.3), verify the response
signature against the bond's `phone_pubkey` (already populated by
DEV-002), and return `ChallengeResponse { ok, signature, reason }`.
On every transaction outcome (ok / denied / replay / bad-sig / timeout)
append one line to `/var/lib/syauth/last.log`:
`{peer_id, nonce_hex, t_start, t_end, outcome, reason}`.
The `Peripheral` trait grows a `wait_for_response(peer_id, deadline) -> Result<Bytes, TransportErr>`
method to keep tests radio-free.

**DoR:** S-005 closed.

**DoD:**
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

**Files likely affected:**
- `crates/syauth-presenced/src/orchestrator.rs`
- `crates/syauth-presenced/src/audit.rs` (new)
- `crates/syauth-transport/src/peripheral.rs` (new method)
- `crates/syauth-presenced/tests/challenge_flow.rs` (new)

**Closure condition:**
```
cargo test -p syauth-presenced --test challenge_flow
# all four tests pass
ls /tmp/syauth-test-last.log && wc -l /tmp/syauth-test-last.log  # >= 4
```

**Traceability:** journey at `specs/journeys/JOURNEY-S-006-challenge-transaction-flow.md`; implementation in `crates/syauth-presenced/src/orchestrator.rs` (`ChallengeOutcome`, `Orchestrator::issue_challenge`, `DEFAULT_AUTH_TIMEOUT`, `NONCE_BYTES`, `OUTCOME_REASON_*`), `crates/syauth-presenced/src/audit.rs` (new — `AuditLog`, `AuditRecord`, `AUDIT_FSYNC_EVERY = 32`, `AUDIT_LOG_FILE_MODE = 0o600`, `AUDIT_FIELD_SEPARATOR = ","`), `crates/syauth-presenced/src/server.rs` (wires `Request::Challenge` through `Orchestrator::issue_challenge`), `crates/syauth-presenced/src/runtime.rs` (threads `Arc<Orchestrator>` into `ServeConfig`, opens `/var/lib/syauth/last.log` at cold-start), `crates/syauth-presenced/src/lib.rs` (re-exports), `crates/syauth-presenced/Cargo.toml` (adds `getrandom`, `hex`), `crates/syauth-transport/src/peripheral.rs` (`Peripheral::wait_for_response`, `PeripheralError::ResponseTimeout`, per-peer mpsc plumbing, `FakePeripheral::inject_response`), `crates/syauth-presenced/tests/challenge_flow.rs` (new — four DoD test cases); closed 2026-05-18.

---

## Step S-007: Nonce LRU + per-peer backpressure + queue deadline

**Description:** Add the in-memory LRU of last 64 nonces per peer
(SPEC §6 idempotency). A response whose nonce was already seen returns
`replay` outcome and `PAM_AUTH_ERR` via S-008. Add per-peer
backpressure: at most one in-flight challenge per peer; subsequent
`ChallengeRequest`s for the same peer wait in a queue with a 1 s
deadline; on overflow the daemon returns `ChallengeResponse { ok=false,
reason: "busy" }` and the PAM module maps it to `PAM_AUTHINFO_UNAVAIL`.

**DoR:** S-006 closed.

**DoD:**
- [x] `NonceCache` per-peer LRU (cap 64) implemented in `orchestrator.rs`.
- [x] Per-peer `Semaphore(1)` gates concurrent challenges.
- [x] `crates/syauth-presenced/tests/replay.rs::repeated_nonce_returns_replay`
      passes.
- [x] `crates/syauth-presenced/tests/replay.rs::lru_evicts_oldest_nonce_at_cap_65`
      passes.
- [x] `crates/syauth-presenced/tests/backpressure.rs::second_in_flight_request_returns_busy_after_1s`
      passes.
- [x] `make scope-discipline && make lint && make test` green.

**Files likely affected:**
- `crates/syauth-presenced/src/orchestrator.rs`
- `crates/syauth-presenced/tests/replay.rs` (new)
- `crates/syauth-presenced/tests/backpressure.rs` (new)

**Closure condition:**
```
cargo test -p syauth-presenced --test replay --test backpressure
# all three tests pass
```

**Traceability:** journey at `specs/journeys/JOURNEY-S-007-nonce-lru-backpressure.md`; implementation in `crates/syauth-presenced/src/orchestrator.rs` (`NonceCache`, `NONCE_LRU_CAP`, `BUSY_QUEUE_DEADLINE`, `BUSY_REASON`, `OUTCOME_REASON_BUSY`, `ChallengeOutcome::Busy`, per-peer `PeerEntry::nonce_cache` + `PeerEntry::challenge_slot`, `Orchestrator::issue_challenge` semaphore-gated wrapper, `Orchestrator::issue_challenge_with_nonce` test-only entry point, `Orchestrator::run_challenge` body with post-verify replay check, `Orchestrator::acquire_challenge_slot`), `crates/syauth-presenced/src/lib.rs` (re-exports `BUSY_QUEUE_DEADLINE`, `BUSY_REASON`, `NONCE_LRU_CAP`, `NonceCache`, `OUTCOME_REASON_BUSY`), `crates/syauth-presenced/tests/replay.rs` (new — `repeated_nonce_returns_replay`, `lru_evicts_oldest_nonce_at_cap_65`), `crates/syauth-presenced/tests/backpressure.rs` (new — `second_in_flight_request_returns_busy_after_1s`); closed 2026-05-18.

---

## Step S-008: `pam_syauth` rewrite — Unix-socket client, no BlueZ

**Description:** Replace the body of `crates/syauth-pam/src/auth.rs::authenticate`
with a Unix-socket client that opens
`${XDG_RUNTIME_DIR}/syauth/auth.sock`, sends `ChallengeRequest { peer_id }`,
awaits the typed response with `auth_timeout`, and maps outcomes to
PAM return codes per SPEC §6 Failure Taxonomy. Remove the
`BluerAdvertiser` call site at `auth.rs:575`. On `connect()` refused,
socket missing, write fail, or response timeout → `PAM_AUTHINFO_UNAVAIL`
within ≤ 50 ms (SPEC §4.3 daemon-down latency). The `--socket` PAM
argument lets test harnesses point at a mock daemon.

**DoR:** S-006 closed (PAM needs at least a real daemon to talk to;
S-007 polish is welcome but not required because backpressure is
exercised at the daemon, not PAM).

**DoD:**
- [x] `crates/syauth-pam/src/auth.rs` no longer imports
      `syauth_transport::BluerAdvertiser` (verify with `git grep`).
- [x] `pam_sm_authenticate` honors a `socket=<path>` argument from
      `/etc/pam.d/sudo`.
- [x] `crates/syauth-pam/src/auth.rs::tests::authenticate_falls_through_when_daemon_socket_missing`
      asserts `PAM_AUTHINFO_UNAVAIL` within 50 ms.
- [x] `crates/syauth-pam/src/auth.rs::tests::authenticate_returns_success_on_daemon_ok`
      drives a unit-test mock daemon and asserts `PAM_SUCCESS`.
- [x] `crates/syauth-pam/src/auth.rs::tests::authenticate_maps_busy_to_authinfo_unavail`
      asserts mapping.
- [x] `crates/syauth-pam/src/auth.rs::tests::authenticate_maps_replay_to_auth_err`
      asserts mapping.
- [x] `crates/syauth-pam/tests/pam_daemon_integration.rs::end_to_end_against_real_daemon_binary`
      spawns the real `syauth-presenced` binary, drives a full
      challenge, asserts success.
- [x] `make scope-discipline && make lint && make test` green.

**Files likely affected:**
- `crates/syauth-pam/src/auth.rs`
- `crates/syauth-pam/src/lib.rs` (drop transport feature flag if any)
- `crates/syauth-pam/Cargo.toml` (drop transport dep)
- `crates/syauth-pam/tests/pam_daemon_integration.rs` (new)

**Closure condition:**
```
cargo test -p syauth-pam
git grep -l "BluerAdvertiser" crates/syauth-pam/   # empty
```

**Traceability:** journey at `specs/journeys/JOURNEY-S-008-pam-unix-socket-client.md`; implementation in `crates/syauth-pam/src/auth.rs` (rewritten around blocking Unix-socket client: `outcome_reason_to_pam`, `DAEMON_CONNECT_TIMEOUT`, `DAEMON_WRITE_TIMEOUT`, `DAEMON_RESPONSE_BUDGET`, `DAEMON_FAST_FAIL_SLACK`, `OUTCOME_REASON_OFFLINE`, `OUTCOME_REASON_ADAPTER_MISSING`, `REASON_NO_BONDS_CONFIGURED`, `REASON_NO_BONDED_PEER`, `daemon_round_trip`, `MockDaemonHandle`; `BluerAdvertiser` / `BtPeer` / `ReplayCache` / `verify_frame` / `verify_tag` / `MOCK_PEER` / `KEYSTORE_FOR_TESTS` / `install_mock_peer` / `install_test_keystore` / `replay_seed` / `acquire_peer` all deleted), `crates/syauth-pam/src/config.rs` (rewritten — `socket_path`, `PAM_SOCKET_ARG_PREFIX`, `XDG_RUNTIME_DIR_ENV`, `DEFAULT_RUNTIME_FALLBACK_PREFIX`, `RUNTIME_SUBDIR`, `DEFAULT_SOCKET_BASENAME`, `Config::resolve_socket_path`, `Config::from_pam_argv`, `Config::with_socket_path`; legacy `mock_peer_enabled`, `adapter_id`, `response_timeout`, `DEFAULT_ADAPTER_NAME`, `DEFAULT_RESPONSE_TIMEOUT`, `TEST_MOCK_ENV_VAR`, `TEST_MOCK_ENV_ENABLED_VALUE`, `ADAPTER_ENV_VAR`, `Config::from_env`, `Config::from_env_with_build_flags` deleted), `crates/syauth-pam/src/entry.rs` (`pam_sm_authenticate` reads libpam `argv` via `collect_pam_argv` and routes through `Config::from_pam_argv`), `crates/syauth-pam/src/lib.rs` (module-doc rewritten for S-008 shape), `crates/syauth-pam/Cargo.toml` (drops `syauth-transport`, `tokio`, `async-trait`, dev-only `ed25519-dalek`/`async-trait` swapped to integration-test-only deps; adds `syauth-presenced` as the canonical wire-format dep), `crates/syauth-pam/tests/pam_e2e.rs` (14-test scenario harness reworked around `MockDaemon` bound on tempdir socket — every SPEC §6 reason mapping pinned), `crates/syauth-pam/tests/pam_daemon_integration.rs` (new — spawns the real `syauth-presenced` binary with `--peripheral=fake` + `--inject-response` + `--test-fixed-nonce`, asserts `AuthOutcome::Success`), `crates/syauth-presenced/src/rpc.rs` (`read_frame_blocking` / `write_frame_blocking` so daemon and PAM share one wire-format module), `crates/syauth-presenced/src/runtime.rs` (`PeripheralMode::Real|Fake`, `InjectedResponse`, `seed_fake_peripheral`, `Config::test_fixed_nonce`; cold-start seed bond now calls `peripheral.add_peer`), `crates/syauth-presenced/src/server.rs` (`ServeConfig::test_fixed_nonce` plumbing into `issue_challenge_with_nonce`), `crates/syauth-presenced/src/main.rs` (hidden `--peripheral=<mode>`, `--inject-response <peer:hex>`, `--test-fixed-nonce <hex>` flags), `crates/syauth-presenced/src/lib.rs` (re-exports `read_frame_blocking`, `write_frame_blocking`, `PeripheralMode`, `InjectedResponse`); closed 2026-05-18. **Deviation:** PAM still calls `BondStore::load` to pick the `peer_id` for the wire frame — see JOURNEY-S-008 §Deviations.

---

## Step S-009: `syauth install-presenced` + retire short-burst advertise

**Description:** Add the `install-presenced` subcommand to
`syauth-cli` (mirrors `install-pam`): copies the daemon binary to
`/usr/local/libexec/syauth-presenced`, installs the systemd user
unit to `~/.config/systemd/user/syauth-presenced.service`, runs
`systemctl --user daemon-reload`, and enables + starts the unit.
`syauth install-pam` grows a `--with-presenced=true` default so the
two installs are bundled. Delete the old short-burst
`BluerAdvertiser::connect` path from `crates/syauth-transport/src/bluez_advertise.rs`
(safe now that S-008 removed the only caller).

**DoR:** S-008 closed.

**DoD:**
- [x] `syauth install-presenced` subcommand exists with help snapshot.
- [x] `syauth install-pam` calls into `install-presenced` unless
      `--with-presenced=false` is passed.
- [x] `BluerAdvertiser::connect` (the per-PAM-call advertise burst)
      and `BluerAdvertiseSession` are deleted; `PersistentPeripheral`
      from S-003 is the only path.
- [x] `crates/syauth-cli/tests/install_presenced_flow.rs::install_writes_unit_and_starts_service`
      passes (uses a `tempdir` for `XDG_CONFIG_HOME`).
- [x] `crates/syauth-cli/tests/snapshots/cli__install_presenced_help_snapshot.snap`
      reviewed + accepted by the user.
- [x] `make scope-discipline && make lint && make test` green.

**Files likely affected:**
- `crates/syauth-cli/src/install.rs` (or wherever `install-pam` lives)
- `crates/syauth-cli/src/lib.rs` (new subcommand)
- `crates/syauth-cli/src/main.rs` (dispatch)
- `crates/syauth-cli/tests/install_presenced_flow.rs` (new)
- `crates/syauth-cli/tests/snapshots/cli__install_presenced_help_snapshot.snap` (new)
- `crates/syauth-transport/src/bluez_advertise.rs` (delete the burst path)

**Closure condition:**
```
cargo test -p syauth-cli --test install_presenced_flow
git grep -n "fn connect" crates/syauth-transport/src/bluez_advertise.rs   # empty
```

**Traceability:** journey at
`specs/journeys/JOURNEY-S-009-install-presenced-retire-burst.md`;
implementation in `crates/syauth-cli/src/install_presenced.rs` (new —
`InstallPresencedOpts`, `InstallPresencedOutcome`,
`InstallPresencedError`, `install_presenced`,
`DEFAULT_DAEMON_BIN_PATH`, `SYSTEMD_USER_UNIT_NAME`,
`SYSTEMD_USER_UNIT_BUNDLED`, `DAEMON_BIN_NAME`,
`SYSTEMD_USER_UNIT_SUBDIR`, `XDG_CONFIG_HOME_ENV`,
`XDG_CONFIG_HOME_FALLBACK_SUBDIR`, `WOULD_RUN_PREFIX`,
`resolve_unit_dir`, `resolve_source_binary`, `atomic_write_text`,
`rewrite_exec_start`, `run_systemctl`);
`crates/syauth-cli/src/lib.rs` (module wired);
`crates/syauth-cli/src/main.rs` (new `Cmd::InstallPresenced` arm,
`run_install_presenced`, `report_install_presenced`; `run_install`
chains `install_presenced` when `--with-presenced=true`);
`crates/syauth-cli/src/install_pam.rs` (`InstallOpts` grows
`with_presenced` default true, `presenced_dry_run`,
`presenced_unit_dir`, `presenced_from`);
`crates/syauth-cli/tests/install_presenced_flow.rs` (new — pins the
tempdir / `--from` / `--dry-run` invariant);
`crates/syauth-cli/tests/install_pam.rs` (existing tests pinned
with `--with-presenced=false`; new `tc11_install_pam_bundles_presenced_by_default`
exercises the bundled `--with-presenced=true` path with
`--presenced-dry-run`);
`crates/syauth-cli/tests/cli.rs` (new `install_presenced_help_snapshot`);
`crates/syauth-cli/tests/snapshots/cli__install_presenced_help_snapshot.snap`
(new); `crates/syauth-cli/tests/snapshots/cli__help_snapshot.snap`
and `crates/syauth-cli/tests/snapshots/cli__install_pam_help_snapshot.snap`
(refreshed for the new subcommand and flag set);
`crates/syauth-transport/src/bluez_advertise.rs` (rewritten —
`BluerAdvertiser::connect`, `BluerAdvertiser::connect_inner`,
`BluerAdvertiseSession`, its `Session` impl, the
`ensure_subscribed_and_ready` helper, the
`ADVERTISE_READ_BUFFER_BYTES` / `ADVERTISE_CONNECTABLE` constants,
the `BtPeer` impl, and the radio-dependent
`connect_rejects_when_not_paired` unit test deleted;
`build_unlock_services` moved into the `#[cfg(test)]` module so the
DEV-004 link-encryption flag assertion still pins the LESC contract;
`new_sync`, `rotating_uuid_for`, `current_minute_from`,
`ADVERTISE_LOCAL_NAME`, `ADVERTISE_DISCOVERABLE` preserved for
`PersistentPeripheral`);
`crates/syauth-transport/src/lib.rs` (drops
`ADVERTISE_READ_BUFFER_BYTES` re-export); closed 2026-05-18.

---

## Step S-010: Phone `PersistentGattClient` with `autoConnect=true`

**Description:** New file
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/PersistentGattClient.kt`.
Owns one `BluetoothGatt` per bonded peer, opened with
`BluetoothDevice.connectGatt(context, autoConnect=true, callback,
TRANSPORT_LE)`. On `onServicesDiscovered` calls
`setCharacteristicNotification(challengeChar, true)` and writes the
CCCD descriptor with `CCCD_ENABLE_NOTIFY`. Exposes a callback
`onChallenge(peerId, frameBytes)`. Robolectric unit tests use the
Robolectric Bluetooth shadows; no real radio needed.

**DoR:** none beyond "Android app compiles today" — this step does
NOT depend on the daemon; the file is wired into the service in S-011.

**DoD:**
- [x] `PersistentGattClient.kt` exists with the contract above.
- [x] `PersistentGattClientTest::auto_connect_true_passed_to_connectGatt`
      passes.
- [x] `PersistentGattClientTest::on_services_discovered_subscribes_via_cccd`
      passes.
- [x] `PersistentGattClientTest::on_characteristic_changed_invokes_onChallenge`
      passes.
- [x] `PersistentGattClientTest::write_response_targets_response_characteristic`
      passes.
- [x] `:app:assembleDebug` succeeds.
- [x] `:app:testDebugUnitTest` green.

**Files likely affected:**
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/PersistentGattClient.kt` (new)
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/PersistentGattClientTest.kt` (new)

**Closure condition:**
```
./gradlew :app:testDebugUnitTest --tests "*PersistentGattClientTest*"
```

**Traceability:** journey at
`specs/journeys/JOURNEY-S-010-persistent-gatt-client.md`;
implementation in
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/PersistentGattClient.kt`
(new — `PersistentGattClient`, `GattOpener`, `DefaultGattOpener`,
companion constants `PERSISTENT_GATT_LOG_TAG`, `CCCD_UUID`,
`CCCD_ENABLE_NOTIFY`, `AUTO_CONNECT_TRUE`);
`syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/PersistentGattClientTest.kt`
(new — pins all four DoD test cases under Robolectric SDK 34);
no other files modified (`DirectGattController.kt` survives until
S-013; `SyauthCompanionService.kt` is rewired in S-011); closed
2026-05-18.

---

## Step S-011: `SyauthCompanionService` → long-running foreground `Service`

**Description:** Swap the parent class from `CompanionDeviceService`
to plain `Service`. Add `startForeground(NOTIFICATION_ID, notification)`
with `foregroundServiceType="connectedDevice"`. Create a low-priority
notification channel "syauth phone-as-key active" that the operator
can mute after first ack. Inject one `PersistentGattClient` per bonded
peer at `onCreate`; tear them down in `onDestroy`. Manifest declares
the service with the right `foregroundServiceType`. The CDM-style
`onDeviceAppeared` parent-class behavior is replaced by an explicit
`startService` from `MainActivity` (which still happens on first
launch after pairing) and `BOOT_COMPLETED` (in S-012).

**DoR:** S-010 closed.

**DoD:**
- [x] `SyauthCompanionService` extends `Service`, not
      `CompanionDeviceService`.
- [x] Manifest declares `foregroundServiceType="connectedDevice"`.
- [x] `MainActivity` calls `startForegroundService(intent)` when a
      bond exists.
- [x] Low-priority notification channel exists; first creation logs once.
- [x] `SyauthCompanionServiceTest::starts_foreground_with_connected_device_type`
      passes (Robolectric).
- [x] `SyauthCompanionServiceTest::injects_one_gatt_client_per_bond`
      passes.
- [x] `SyauthCompanionServiceTest::stops_clients_on_destroy`
      passes.
- [x] `:app:assembleDebug` and `:app:testDebugUnitTest` green.

**Files likely affected:**
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthCompanionService.kt`
- `syauth-android/app/src/main/AndroidManifest.xml`
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/SyauthCompanionServiceTest.kt` (new or extended)

**Closure condition:**
```
./gradlew :app:testDebugUnitTest --tests "*SyauthCompanionServiceTest*"
```

**Traceability:** journey at
`specs/journeys/JOURNEY-S-011-service-foreground-lifecycle.md`;
implementation in
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthCompanionService.kt`
(reshaped — parent class swapped from `CompanionDeviceService` to
`android.app.Service`; new symbols `ManagedClient`,
`GattClientFactory`, `BondListProvider`, `PersistentManagedClient`,
`NOTIFICATION_CHANNEL_ID = "syauth-presence"`,
`NOTIFICATION_CHANNEL_NAME = "syauth phone-as-key active"`,
`NOTIFICATION_CHANNEL_DESCRIPTION`, `NOTIFICATION_ID = 1001`,
`FOREGROUND_SERVICE_TYPE`, `NOTIFICATION_TITLE`,
`NOTIFICATION_BODY`, `NOTIFICATION_ICON`,
`lastForegroundType`, `ensureNotificationChannel`,
`buildForegroundNotification`, `startForegroundCompat`,
`injectClientsForBonds`, `defaultBondListProvider`,
`handleDeviceAppeared` / `handleDeviceDisappeared` — the old
`onDeviceAppeared` / `onDeviceDisappeared` overrides renamed and
preserved for the instrumented `CdmLifecycleTest`; new companion
seams `gattClientFactory`, `bondListProvider`; `resetSeams` now
clears the two new seams);
`syauth-android/app/src/main/AndroidManifest.xml` (`<service>`
declaration: `exported="false"`, intent-filter removed, BIND
permission removed, `foregroundServiceType="connectedDevice"`
preserved);
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
(new `installPersistentClientFactory` +
`startSyauthCompanionForegroundService` helpers wired inside the
`record != null` branch of `onCreate`; imports for
`BondListProvider`, `GattClientFactory`, `PersistentGattClient`,
`PersistentManagedClient`);
`syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/SyauthCompanionServiceTest.kt`
(new — pins all three DoD test cases under Robolectric SDK 34);
`syauth-android/app/src/androidTest/kotlin/com/sy/syauth/android/bg/CdmLifecycleTest.kt`
(refactored to call the renamed `handleDeviceAppeared` /
`handleDeviceDisappeared` hooks); closed 2026-05-18.

---

## Step S-012: `BOOT_COMPLETED` receiver + WorkManager 15-min watchdog

**Description:** Add `BootCompletedReceiver` that fires on
`ACTION_BOOT_COMPLETED` and (if a bond exists) calls
`startForegroundService`. Add a `WorkManager` `PeriodicWorkRequest`
(15-min interval, the Android floor) that checks
`SyauthCompanionService.isRunning()` and re-launches if not. The
existing `AndroidCdmPairCompanionScanner` proximity-observation
callback becomes a third resurrection trigger — if `onDeviceAppeared`
fires for a bonded peer while the service is dead, restart it. This is
SPEC scope items #14 and #19 ("CDM kept as belt-and-suspenders").

**DoR:** S-011 closed.

**DoD:**
- [x] `BootCompletedReceiver` registered in manifest with
      `RECEIVE_BOOT_COMPLETED`.
- [x] `SyauthWatchdogWorker` periodic worker scheduled at first
      `MainActivity.onCreate` (if a bond exists).
- [x] CDM `onDeviceAppeared` triggers a restart when the service is
      dead.
- [x] `BootCompletedReceiverTest::boot_with_bond_starts_service`
      passes.
- [x] `BootCompletedReceiverTest::boot_without_bond_no_op`
      passes.
- [x] `SyauthWatchdogWorkerTest::resurrects_killed_service`
      passes.
- [x] `:app:assembleDebug` and `:app:testDebugUnitTest` green.

**Files likely affected:**
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/BootCompletedReceiver.kt` (new)
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthWatchdogWorker.kt` (new)
- `syauth-android/app/src/main/AndroidManifest.xml`
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/AndroidCdmPairCompanionScanner.kt` (resurrect hook)
- Robolectric tests under `app/src/test/`.

**Closure condition:**
```
./gradlew :app:testDebugUnitTest --tests "*BootCompletedReceiverTest*" --tests "*SyauthWatchdogWorkerTest*"
```

**Traceability:** journey at
`specs/journeys/JOURNEY-S-012-boot-receiver-watchdog.md`;
implementation in
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/BootCompletedReceiver.kt`
(new — `BootCompletedReceiver`, `BOOT_COMPLETED_ACTION`,
`BOOT_RECEIVER_LOG_TAG`);
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthWatchdogWorker.kt`
(new — `SyauthWatchdogWorker`, `WATCHDOG_INTERVAL = Duration.ofMinutes(15)`,
`WATCHDOG_WORK_NAME = "syauth-watchdog"`, `WATCHDOG_LOG_TAG`);
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/Resurrection.kt`
(new — shared `resurrectIfDead(context)` helper that fans in from
the receiver, the worker, and the CDM hook; `RESURRECT_LOG_TAG`);
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthCompanionService.kt`
(new `isRunning: AtomicBoolean` companion field set/cleared in
`onCreate` / `onDestroy`; `handleDeviceAppeared` now calls
`resurrectIfDead(applicationContext)` as the third trigger);
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/AndroidCdmPairCompanionScanner.kt`
(new `onProximityObservedForBondedPeer(context)` public hook that
delegates to the shared helper);
`syauth-android/app/src/main/AndroidManifest.xml`
(adds `<uses-permission android:name="android.permission.RECEIVE_BOOT_COMPLETED"/>`
and the `<receiver>` declaration for `.bg.BootCompletedReceiver`
with the `BOOT_COMPLETED` intent-filter);
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
(new `scheduleSyauthWatchdog()` helper invoked inside the existing
`record != null` branch of `onCreate`; imports
`SyauthWatchdogWorker`, `WATCHDOG_INTERVAL`, `WATCHDOG_WORK_NAME`);
`syauth-android/app/build.gradle.kts` (adds
`androidx.work:work-runtime-ktx:2.9.0` to `implementation` and
`androidx.work:work-testing:2.9.0` to `testImplementation`);
`syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/BootCompletedReceiverTest.kt`
(new — pins `boot_with_bond_starts_service` and
`boot_without_bond_no_op` under Robolectric SDK 34);
`syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/SyauthWatchdogWorkerTest.kt`
(new — pins `resurrects_killed_service` via
`TestListenableWorkerBuilder` under Robolectric SDK 34); closed
2026-05-18.

---

## Step S-013: Remove `DirectGattController` (tonight's hot-fix path)

**Description:** Delete `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/DirectGattController.kt`
and the `installGattControllerFactory` wire-up in `MainActivity`. The
`GattControllerFactory` extension point on `SyauthCompanionService`
goes away — the service constructs `PersistentGattClient` directly.
This is SPEC scope item #19 verbatim: "the tonight's hot-fix CDM-only
path is removed".

**DoR:** S-011 closed (and S-010 by transitivity).

**DoD:**
- [x] `DirectGattController.kt` deleted.
- [x] `MainActivity` no longer references `gattControllerFactory` or
      `installGattControllerFactory`.
- [x] `SyauthCompanionService.gattControllerFactory` field removed.
- [x] `git grep -n "DirectGattController" syauth-android/` returns empty.
- [x] `:app:assembleDebug` and `:app:testDebugUnitTest` green (no
      tests should break — DirectGattController had no tests).

**Files likely affected:**
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/DirectGattController.kt` (DELETE)
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthCompanionService.kt`
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`

**Closure condition:**
```
git ls-files syauth-android/ | grep DirectGattController   # empty
./gradlew :app:assembleDebug
./gradlew :app:testDebugUnitTest
```

**Traceability:** journey at
`specs/journeys/JOURNEY-S-013-remove-direct-gatt-controller.md`;
implementation deletes
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/DirectGattController.kt`
and the matching `CdmLifecycleTest.kt` androidTest, and edits
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthCompanionService.kt`
(removed: `GattControllerFactory` interface, `controllers` map,
`handleDeviceAppeared`, `handleDeviceDisappeared`, `handleChallenge`,
companion-object `gattControllerFactory` field and its `resetSeams()`
entry, `AssociationInfo` import), plus
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
(removed: `installGattControllerFactory` helper and its call from
`installCompanionSeams`). Comment scrubs in
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/PersistentGattClient.kt`
and `bg/BleScanController.kt` so the closure-condition grep is
mechanically empty.

---

## Step S-014: `ChallengeApprovalActivity` (transparent, over-keyguard)

**Description:** New `ChallengeApprovalActivity` is a transparent,
no-history, single-instance activity that the service launches via
`PendingIntent` on `onCharacteristicChanged`. Manifest declares
`USE_FULL_SCREEN_INTENT`, `showWhenLocked=true`,
`turnScreenOn=true`. The activity displays the bond's hostname +
short `peer_id` as the prompt description (SPEC §9 Q2 answer):
`"$hostname is requesting sudo (peer_id $short)"`. Cancel button
writes a "denied" frame back on the response characteristic via the
service. This step does NOT yet invoke the biometric prompt — that
arrives in S-015 — so the activity ends as a placeholder UI that
returns "ok" on tap (gated behind a debug build flag for testing).

**DoR:** S-011 closed.

**DoD:**
- [x] `ChallengeApprovalActivity.kt` exists with the manifest
      attributes above.
- [x] Service launches it via `PendingIntent.getActivity` on a fresh
      challenge.
- [x] Cancel sends a denied frame back through the same GATT
      connection.
- [x] `ChallengeApprovalActivityTest::launches_over_keyguard`
      passes.
- [x] `ChallengeApprovalActivityTest::cancel_writes_denied_frame`
      passes.
- [x] `ChallengeApprovalActivityTest::hostname_shown_in_prompt`
      passes.
- [x] `:app:assembleDebug` and `:app:testDebugUnitTest` green.

**Files likely affected:**
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/ChallengeApprovalActivity.kt` (new)
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthCompanionService.kt`
- `syauth-android/app/src/main/AndroidManifest.xml`
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/ChallengeApprovalActivityTest.kt` (new)

**Closure condition:**
```
./gradlew :app:testDebugUnitTest --tests "*ChallengeApprovalActivityTest*"
```

**Traceability:** journey at
`specs/journeys/JOURNEY-S-014-challenge-approval-activity.md`;
implementation in
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/ChallengeApprovalActivity.kt`
(new — `ChallengeApprovalActivity`, `EXTRA_PEER_ID = "syauth.peerId"`,
`EXTRA_HOSTNAME = "syauth.hostname"`,
`EXTRA_CHALLENGE_BYTES = "syauth.challengeBytes"`,
`APPROVAL_LOG_TAG`, `DENIED_FRAME_REASON = "denied"`,
`SIGNATURE_LEN = 64`, `SHORT_PEER_ID_LEN = 8`,
`DENIED_FRAME_BYTES = ByteArray(SIGNATURE_LEN) { 0 }`,
`CancelSink` fun-interface, `ChallengeApprovalActivity.cancelSink`
companion seam, `ChallengeApprovalActivity.resetSeams()`,
`lastShowWhenLockedFlag` / `lastTurnScreenOnFlag` /
`lastPromptText` recording fields the DoD tests read,
`onApproveClicked` placeholder gated behind `BuildConfig.DEBUG`,
`onCancelClicked` invokes the sink + finish);
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthCompanionService.kt`
(new companion helper `launchApprovalActivity(context, peerId, challengeBytes)`
that builds the `Intent` with the S-014 extras and dispatches via
`PendingIntent.getActivity` with
`FLAG_UPDATE_CURRENT | FLAG_IMMUTABLE`; new file-scope
`APPROVAL_PENDING_REQUEST_CODE = 0x5A14`; new imports
`android.app.PendingIntent`, `android.content.Context`);
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/PersistentGattClient.kt`
(new file-scope `PersistentGattClientRegistry` object with
`put` / `lookup` / `remove` / `reset` so the activity's cancel sink
can resolve the per-peer client and call `writeResponse`);
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
(`installPersistentClientFactory` now passes a real `onChallenge`
that calls `SyauthCompanionService.launchApprovalActivity` and
populates `PersistentGattClientRegistry`;
`installCompanionSeams` installs the
`ChallengeApprovalActivity.cancelSink`; renamed imports of
`APPROVE_EXTRA_*` constants);
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/ApproveNotification.kt`
(pre-existing `EXTRA_CHALLENGE_B64` / `EXTRA_HOSTNAME` /
`EXTRA_PEER_ID` top-level constants renamed to `APPROVE_EXTRA_*`
to disambiguate from the S-014 activity-launch extras);
`syauth-android/app/src/main/AndroidManifest.xml`
(adds `<uses-permission android:name="android.permission.USE_FULL_SCREEN_INTENT"/>`
and the `<activity android:name=".bg.ChallengeApprovalActivity"
android:exported="false" android:launchMode="singleInstance"
android:noHistory="true" android:showOnLockScreen="true"
android:turnScreenOn="true"
android:theme="@style/Theme.SyauthTranslucent"
android:configChanges="orientation|screenSize" />` declaration);
`syauth-android/app/src/main/res/values/themes.xml`
(adds `Theme.SyauthTranslucent` parented to
`android:Theme.Translucent.NoTitleBar`);
`syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/ChallengeApprovalActivityTest.kt`
(new — pins all three DoD test cases under Robolectric SDK 34);
`syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/ApproveNotificationTest.kt`
(updated to track the `APPROVE_EXTRA_*` rename); closed
2026-05-18.

---

## Step S-015: `BiometricPrompt(AUTH_BIOMETRIC_STRONG, per-use)` + Keystore sign

**Description:** Inside `ChallengeApprovalActivity`, replace the
debug "ok" tap with a real `BiometricPrompt` whose
`PromptInfo.allowedAuthenticators = BIOMETRIC_STRONG`. On success,
unlock the bond's Ed25519 Keystore key with
`BiometricPrompt.CryptoObject(signature)` (the key was generated
in DEV-002 with
`setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG)`),
sign the challenge frame, and write the response on the GATT
response characteristic via the service. On biometric fail / cancel,
write a "denied" frame. SPEC §3 D6 contract; SPEC §7 T-Relay defense
hinges on this step.

**DoR:** S-014 closed.

**DoD:**
- [x] `BiometricPrompt` constructed with `BIOMETRIC_STRONG`.
- [x] `CryptoObject(signature)` binds the keystore key to a single
      use per prompt.
- [x] Response frame is the Ed25519 signature over the challenge bytes
      (verified by the existing `verify_response` from syauth-core).
- [x] `BiometricPromptTest::strong_authenticator_required`
      passes.
- [x] `BiometricPromptTest::per_use_keystore_unlock`
      passes (asserts a fresh `BiometricPrompt` round per sign).
- [x] `BiometricPromptTest::cancel_writes_denied`
      passes.
- [x] `KeystoreSignTest::signs_challenge_with_bond_key`
      passes (Robolectric Keystore shadow + the real bond record).
- [x] `:app:assembleDebug` and `:app:testDebugUnitTest` green.

**Files likely affected:**
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/ChallengeApprovalActivity.kt`
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/KeystoreKeyGenerator.kt`
  (verify per-use params; no code change expected — assertion test only)
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/BiometricPromptTest.kt` (new)
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/KeystoreSignTest.kt` (new)

**Closure condition:**
```
./gradlew :app:testDebugUnitTest --tests "*BiometricPromptTest*" --tests "*KeystoreSignTest*"
```

**Traceability:** journey at
`specs/journeys/JOURNEY-S-015-biometric-keystore-sign.md`;
implementation in
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/ChallengeApprovalActivity.kt`
(modified — adds `BiometricGate` interface, `BiometricGateCallback`,
`ResponseSink`, `STRONG_AUTHENTICATOR`, `PROMPT_TITLE_RES`,
`PROMPT_SUBTITLE_FMT`, `PROMPT_NEGATIVE_RES`,
`EXTRA_KEYSTORE_ALIAS`, `ED25519_ALGORITHM`, `KEYSTORE_PROVIDER`,
top-level `signChallenge(privateKey, challengeBytes)` helper,
`buildPromptInfo(activity, hostname, shortPeerId)` helper,
`buildApprovalIntent(...)` helper, production
`AndroidBiometricGate` class, `responseSink` + `biometricGate`
companion seams; parent class changed from `ComponentActivity` to
`FragmentActivity` so `androidx.biometric:BiometricPrompt` can
bind to the fragment manager);
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/KeystoreKeyGenerator.kt`
(modified — `buildEd25519SpecBuilder` now calls
`.setUserAuthenticationParameters(KEYSTORE_AUTH_VALIDITY_PER_USE_SECONDS,
KeyProperties.AUTH_BIOMETRIC_STRONG)` explicitly per SPEC §3 Scope
item 20, on top of the existing `setUserAuthenticationRequired(true)`;
new file-scope `KEYSTORE_AUTH_VALIDITY_PER_USE_SECONDS = 0`);
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/SyauthCompanionService.kt`
(modified — new `KeystoreAliasResolver` interface +
`keystoreAliasResolver` companion seam + `resetSeams()` reset;
`launchApprovalActivity` now populates the `EXTRA_KEYSTORE_ALIAS`
extra via the resolver and delegates intent construction to the
shared `buildApprovalIntent` helper);
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
(modified — `installCompanionSeams` now installs the
`KeystoreAliasResolver` and the production `ResponseSink` that
routes the BiometricPrompt response back through
`PersistentGattClient.writeResponse`);
`syauth-android/app/src/main/res/values/strings.xml`
(modified — adds `syauth_biometric_prompt_title`,
`syauth_biometric_prompt_subtitle_fmt`,
`syauth_biometric_prompt_cancel`);
`syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/BiometricPromptTest.kt`
(new — three Robolectric SDK-34 tests:
`strong_authenticator_required`, `per_use_keystore_unlock`,
`cancel_writes_denied`);
`syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/KeystoreSignTest.kt`
(new — `signs_challenge_with_bond_key` Robolectric SDK-34 test
using a host-JVM Ed25519 keypair through the shared
`signChallenge` helper);
`syauth-android/app/src/test/kotlin/com/sy/syauth/android/pair/KeystoreKeyGeneratorTest.kt`
(modified — adds `base_builder_pins_biometric_strong_per_use`
assertion pinning per-use validity and `AUTH_BIOMETRIC_STRONG`).

---

## Step S-016: `sy syauth doctor`

**Description:** New `syauth doctor` subcommand inspects: daemon
liveness (PID file + socket reachability), bonds file presence and
parseability, keys file mode 0600 and contents readable, BlueZ
adapter `Powered=true`, systemd user unit state, last 10 lines of
`/var/lib/syauth/last.log`, and surfaces the `XDG_RUNTIME_DIR` /
SSH-session caveat from SPEC §8 risks. Output is greppable
key=value pairs so `sy syauth doctor | grep daemon=` lights up
operator dashboards.

**DoR:** S-008 closed (so there's a daemon to check).

**DoD:**
- [x] `syauth doctor` subcommand exists.
- [x] `syauth doctor --json` emits a typed JSON object for tooling.
- [x] `crates/syauth-cli/tests/doctor_flow.rs::reports_daemon_up_when_socket_responds`
      passes.
- [x] `crates/syauth-cli/tests/doctor_flow.rs::reports_daemon_down_when_socket_missing`
      passes.
- [x] `crates/syauth-cli/tests/doctor_flow.rs::flags_keys_file_not_0600`
      passes (uses a tempdir keys file with 0644).
- [x] `crates/syauth-cli/tests/snapshots/cli__doctor_help_snapshot.snap`
      reviewed.
- [x] `make scope-discipline && make lint && make test` green.

**Files likely affected:**
- `crates/syauth-cli/src/doctor.rs` (new)
- `crates/syauth-cli/src/lib.rs`
- `crates/syauth-cli/src/main.rs`
- `crates/syauth-cli/tests/doctor_flow.rs` (new)
- `crates/syauth-cli/tests/snapshots/cli__doctor_help_snapshot.snap` (new)

**Closure condition:**
```
cargo test -p syauth-cli --test doctor_flow
```

**Traceability:** journey at
`specs/journeys/JOURNEY-S-016-syauth-doctor.md`; implementation in
`crates/syauth-cli/src/doctor.rs` (new — `DoctorOpts`, `DoctorReport`,
`DaemonState`, `BondsReport`, `KeysReport`, `KeyFileReport`,
`XdgRuntimeDirReport`, `run_doctor`, `build_report`, `write_keyvalue`,
`write_json`; named constants `DEFAULT_BONDS_FILE`,
`DEFAULT_KEYS_DIR`, `DEFAULT_AUDIT_LOG_FILE`,
`EXPECTED_KEYS_FILE_MODE = 0o600`, `DOCTOR_LAST_LOG_TAIL = 10`,
`DAEMON_CONNECT_TIMEOUT = 50 ms`); wired into
`crates/syauth-cli/src/lib.rs` (`pub mod doctor`) and
`crates/syauth-cli/src/main.rs` (new `Cmd::Doctor(DoctorOpts)`
variant + `run_doctor_cli` dispatcher);
`crates/syauth-cli/Cargo.toml` (new deps `serde`, `serde_json`,
`syauth-presenced`, `nix`); integration tests in
`crates/syauth-cli/tests/doctor_flow.rs` (new — pins
`reports_daemon_up_when_socket_responds`,
`reports_daemon_down_when_socket_missing`,
`flags_keys_file_not_0600`, `json_mode_emits_typed_object`);
`crates/syauth-cli/tests/cli.rs` (new `doctor_help_snapshot` test);
snapshot files
`crates/syauth-cli/tests/snapshots/cli__doctor_help_snapshot.snap`
(new) and
`crates/syauth-cli/tests/snapshots/cli__help_snapshot.snap` (updated
to include the new `doctor` subcommand); closed 2026-05-18.

---

## Step S-017: Extend `sy syauth status` (daemon liveness + per-peer metrics)

**Description:** Extend the existing `syauth status` subcommand to
ask the daemon (over the same Unix socket via `StatusRequest`) for
per-peer liveness: time-since-last-challenge, time-since-last-connect,
current rotating UUID, count of in-flight challenges. Falls back to
"daemon-down: <reason>" if the socket is unreachable. The waybar
pill in the `sy` repo's roadmap consumes this output.

**DoR:** S-016 closed (the socket-probing primitive is shared with
doctor).

**DoD:**
- [x] `syauth status` reports per-peer columns when daemon is up.
- [x] `syauth status --watch` polls every 1 s and redraws.
- [x] `syauth status --json` emits typed JSON.
- [x] `crates/syauth-cli/tests/status_flow.rs::reports_per_peer_liveness`
      passes.
- [x] `crates/syauth-cli/tests/status_flow.rs::falls_back_when_daemon_down`
      passes.
- [x] `crates/syauth-cli/tests/snapshots/cli__status_snapshot.snap`
      updated + reviewed.
- [x] `make scope-discipline && make lint && make test` green.

**Files likely affected:**
- `crates/syauth-cli/src/status.rs`
- `crates/syauth-cli/tests/status_flow.rs`
- `crates/syauth-cli/tests/snapshots/cli__status_snapshot.snap`

**Closure condition:**
```
cargo test -p syauth-cli --test status_flow
```

**Traceability:** journey at
`specs/journeys/JOURNEY-S-017-status-extension.md`; implementation
extends `crates/syauth-cli/src/status.rs` (new clap options
`--socket`, `--watch`, `--json`; `CliStatusReport` / `DaemonProbeState`
typed report; `probe_daemon` shares the S-016 reason vocabulary;
`WATCH_INTERVAL = Duration::from_secs(1)` polling loop with a
SIGINT handler installed via `signal-hook`),
`crates/syauth-presenced/src/orchestrator.rs` (`peers_snapshot()`
inspection method + per-peer `PeerLiveness` timestamps stamped at
slot acquisition + free helpers `stamp_liveness`, `ms_since`,
`challenge_slot_in_flight`), `crates/syauth-presenced/src/rpc.rs`
(`PeerStatus` re-shaped to `last_challenge_ms_ago`,
`last_connect_ms_ago`, `current_session_uuid`,
`in_flight_challenges`), `crates/syauth-presenced/src/server.rs`
(`ServeConfig::started_at` field; `Request::Status` arm calls
`Orchestrator::peers_snapshot()`), and
`crates/syauth-cli/Cargo.toml` (added `signal-hook = "0.3"`); new
test `crates/syauth-cli/tests/status_flow.rs` (3 cases including
the closure-condition pair), new orchestrator integration test
`crates/syauth-presenced/tests/peers_snapshot.rs`, renamed
snapshot `crates/syauth-cli/tests/snapshots/cli__status_snapshot.snap`
(was `cli__status_help_snapshot.snap` — orphaned and removed);
closed 2026-05-18.

---

## Step S-018: Phone-side challenge notification + audit polish

**Description:** SPEC scope item #23: phone writes a low-priority
notification per challenge transaction (rate-limited to 1 per 5 s,
suppressed entirely if the operator dismisses the channel). Notification
shows: hostname, peer_id short, outcome (granted / denied / timed-out).
Tapping the notification deep-links to a per-transaction history view
on the Home route. This is the UX-visible audit signal that mirrors the
desktop's `last.log`.

**DoR:** S-015 closed.

**DoD:**
- [x] Per-challenge notification posted on a low-priority channel.
- [x] Rate limiter caps to 1 / 5 s.
- [x] History view in `MainActivity` renders the last 50 transactions
      pulled from a small Room table. (Substituted with a JSONL store
      on disk — see journey "Deviations".)
- [x] `ChallengeNotificationTest::posts_per_challenge`
      passes.
- [x] `ChallengeNotificationTest::rate_limited_to_one_per_five_seconds`
      passes.
- [x] `ChallengeHistoryTest::renders_last_fifty`
      passes.
- [x] `:app:assembleDebug` and `:app:testDebugUnitTest` green.

**Files likely affected:**
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/ChallengeNotification.kt` (new)
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/history/ChallengeHistoryDao.kt` (new)
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
- Tests under `app/src/test/`.

**Closure condition:**
```
./gradlew :app:testDebugUnitTest --tests "*ChallengeNotificationTest*" --tests "*ChallengeHistoryTest*"
```

**Traceability:** journey at
`specs/journeys/JOURNEY-S-018-phone-notification-history.md`;
implementation in
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/ChallengeNotification.kt`
(new — `NOTIFICATION_CHANNEL_HISTORY = "syauth-challenge-history"`,
`NOTIFICATION_RATE_LIMIT = Duration.ofSeconds(5)`,
`HISTORY_OUTCOME_GRANTED` / `HISTORY_OUTCOME_DENIED` /
`HISTORY_OUTCOME_TIMED_OUT`,
`HISTORY_ROUTE_INTENT_SCHEME = "syauth"`,
`HISTORY_ROUTE_INTENT_HOST = "history"`,
`ChallengeNotificationDispatcher` class with `Clock` seam, atomic
`lastPostMs` rate gate, and `dispatch(hostname, peerId, outcome)`
that appends an audit row and (subject to the rate limiter)
posts a low-priority notification);
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/history/ChallengeHistoryDao.kt`
(new — `HISTORY_TABLE_NAME = "challenge_history"`,
`HISTORY_DISPLAY_LIMIT = 50`, `MAX_HISTORY_FILE_RECORDS = 200`,
`HISTORY_FILE_EXTENSION = ".jsonl"`,
`ChallengeHistoryRecord` data class with six snake-case-aligned
fields, `ChallengeHistoryDao` class with append-only `insert` and
descending-order `recent(limit)` against
`${filesDir}/challenge_history.jsonl`; deviation from "Room table"
SPEC wording — see journey doc);
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
(modified — adds `NavRoutes.HISTORY` route, `HistoryRoute`
composable rendering `dao.recent(HISTORY_DISPLAY_LIMIT)` as a
`LazyColumn`, `parseHistoryDeepLink(intent)` parser,
`NavigateOnHistoryDeepLink` side-effect composable; wraps the
production `cancelSink` / `responseSink` to call the dispatcher
after `writeResponse` returns with the outcome derived from the
response bytes — `denied` if equal to `DENIED_FRAME_BYTES`,
`granted` otherwise);
`syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/ChallengeNotificationTest.kt`
(new — TC-01 `posts_per_challenge` + TC-02
`rate_limited_to_one_per_five_seconds` under Robolectric SDK 34);
`syauth-android/app/src/test/kotlin/com/sy/syauth/android/history/ChallengeHistoryTest.kt`
(new — TC-03 `renders_last_fifty` pure-JVM test); closed
2026-05-18.

---

## Step S-019: E2E latency benchmark + SPEC §4.3 gate

**Description:** Ship `scripts/e2e-unlock.sh` that drives
`pamtester syauth-test $USER authenticate` 100 times against the
connected R5CY214FQHM phone with `SYAUTH_REAL_RADIOS=1`, extracts
elapsed-ms from `/var/lib/syauth/last.log`, computes p50 / p95 / p99,
and fails the build if **p50 > 1.5 s** OR **p99 > 2.0 s** (the SPEC
§4.3 contract). Marked `#[ignore]` in CI; runnable on the
operator's developer machine via `make e2e-unlock`.

**DoR:** S-008 and S-015 closed (full path operational end-to-end).

**DoD:**
- [x] `scripts/e2e-unlock.sh` exists, executable, idempotent.
- [x] `Makefile` has `make e2e-unlock` target gated behind
      `SYAUTH_REAL_RADIOS=1`.
- [x] Script outputs a JSON summary `{p50_ms, p95_ms, p99_ms,
      n_failures, n_timeouts}`.
- [x] Script exits non-zero on p50 > 1500 ms OR p99 > 2000 ms.
- [~] One hand-run with hardware shows p50 < 1.5 s, p99 < 2.0 s;
      output pasted into the closure entry in `docs/known-gaps.md`
      (or into the journey doc if no DEV-NNN gap is open).
      REQUIRES OPERATOR — phone + laptop in hand. See
      `specs/journeys/JOURNEY-S-019-e2e-latency-gate.md` Closure
      Appendix; the operator pastes the JSON line there when they
      drive the real-radio probe.
- [x] `make scope-discipline && make lint && make test` green
      (script-only step + one Rust integration test for the
      percentile math + JSON shape + exit-code matrix; no
      production Rust/Kotlin code changes).

**Files likely affected:**
- `scripts/e2e-unlock.sh` (new)
- `Makefile` (new target)
- `crates/syauth-cli/tests/e2e_unlock_script.rs` (new — hermetic
  fixture test that shells out to the script with a synthetic
  audit log + `SYAUTH_E2E_PREPOPULATED=1`; exercises TC-01..TC-03 +
  TC-06 + TC-07 from the journey doc).

**Closure condition:**
```
SYAUTH_REAL_RADIOS=1 make e2e-unlock
# output: {p50_ms: <1500, p95_ms: <some, p99_ms: <2000, n_failures: 0, n_timeouts: 0}
# exit 0
```

**Traceability:** journey at
`specs/journeys/JOURNEY-S-019-e2e-latency-gate.md`; implementation
in `scripts/e2e-unlock.sh` (new — named constants `ITERATIONS`,
`P50_BUDGET_MS=1500`, `P99_BUDGET_MS=2000`, `AUDIT_LOG_PATH`,
`PAM_SERVICE`, `PAM_USER`, `PAMTESTER_BIN`, `PREPOPULATED_MODE`;
exit-code constants `EX_OK=0`, `EX_GATE_FAIL=1`, `EX_PREFLIGHT=2`;
audit-column constants `AUDIT_FIELD_SEPARATOR`, `AUDIT_COL_T_START`,
`AUDIT_COL_T_END`, `AUDIT_COL_REASON`, `AUDIT_REASON_RESPONSE_TIMEOUT`;
helpers `log_stderr`, `fail_preflight`, `resolve_pamtester`,
`percentile_from_sorted`; pre-flight enforces
`SYAUTH_REAL_RADIOS=1`, `pamtester` presence, audit-log readability;
drive loop snapshots `START_LINES` / `END_LINES` around
`$PAMTESTER_BIN $PAM_SERVICE $PAM_USER authenticate`; parse uses
`awk -F,` over the new audit-log slice, nearest-rank percentile
on the sorted elapsed-ms array; emits exactly one JSON line on
stdout matching the SPEC contract); `Makefile` (new `e2e-unlock`
target gated behind `ifneq ($(SYAUTH_REAL_RADIOS),1)` mirroring
`e2e-real`'s pattern); `crates/syauth-cli/tests/e2e_unlock_script.rs`
(new — fixture-driven tests pinning `tc01_under_budget_exits_zero`,
`tc02_p99_over_budget_exits_one`, `tc03_p50_over_budget_exits_one`,
`tc06_preflight_missing_real_radios_exits_two`,
`tc07_preflight_missing_audit_log_exits_two`; uses
`SYAUTH_E2E_PREPOPULATED=1` so the percentile math + JSON shape
are CI-enforceable without real radios).

---

## Out-of-scope (covered by SPEC §3.2 Anti-goals)

Per the SPEC, these are NOT roadmap items:

- Phone advertising any UUID (§3.2 D8 privacy).
- UWB ranging.
- Wi-Fi / mDNS rendezvous.
- Time-windowed Keystore auth (any non-per-use config).
- Background `BluetoothLeScanner` foreground scan.

If a future need arises, file a new SPEC; do not extend this roadmap.

---

## Cross-cutting gates (apply to every step)

- `make scope-discipline` — no banned vocabulary (`v0.1 demo`,
  `v0.2 will…`, "for now", "first cut" outside an existing SPEC item),
  no un-rowed `SPEC-DEVIATION`s.
- `make lint` — `cargo fmt --check`, `cargo clippy -- -D warnings`,
  `ktlint`.
- `make test` — all crates' unit + integration suites.
- `:app:assembleDebug` + `:app:testDebugUnitTest` for steps touching
  `syauth-android/`.
- Test-count regression check: each step's closing `make test` total
  ≥ baseline at step open.

## Traceability

| Step | SPEC scope items | SPEC clause |
|---|---|---|
| S-001 | #1 | §3 Approach / §4 Architecture |
| S-002 | #5 | §3 Decisions row "PAM ↔ daemon transport" |
| S-003 | (refactor) | §4 Architecture "Modules affected" |
| S-004 | #2, #3 | §3.2 D8 rotation cadence |
| S-005 | #4, #10 | §3 Decisions / §6 Rehydration |
| S-006 | #6, #8 | §6 State model / §7 Audit |
| S-007 | #7 | §6 Idempotency, §6 Failure taxonomy |
| S-008 | #11, #12, #13 | §3.2 D7, §6 Failure taxonomy |
| S-009 | #9 | §4 Migration / §3 Decisions "Daemon process model" |
| S-010 | #15, #16 | §3 Approach phone-side |
| S-011 | #14 | §3 Decisions "Phone connection lifecycle" |
| S-012 | #14, #19 | §3 Decisions "Phone fallback" |
| S-013 | #19 | §3.2 D8 "no fallback hot-fix CDM-only path" |
| S-014 | #17 | §3 Approach phone-side, §9 Q2 |
| S-015 | #17, #18, #20, #21 | §3.2 D6, §7 T-Relay |
| S-016 | #24 | §8 Risks "Audit-log discovery" |
| S-017 | #24 | §3 Approach observability |
| S-018 | #23 | §3 scope item #23 |
| S-019 | (gate) | §4.3 Performance |

## Changelog

- 2026-05-18 — Roadmap authored from
  `specs/unlock-proximity/SPEC.md`. 19 steps. No prior roadmap to
  merge.
