# JOURNEY-S-008: `pam_syauth` rewrite — Unix-socket client, no BlueZ

> **Spec anchors:** `specs/unlock-proximity/SPEC.md` §3 Scope items #11
> ("`pam_sm_authenticate` no longer drives BlueZ directly. It opens
> `${XDG_RUNTIME_DIR}/syauth/auth.sock`, issues `ChallengeRequest`,
> awaits a typed response with timeout = `auth_timeout`. On
> no-daemon / connect-refused / response-timeout it returns
> `PAM_AUTHINFO_UNAVAIL`, preserving the SPEC §3.2 D7 fall-through to
> `pam_unix`"), #12 ("The PAM module gains a `--socket` argument
> (default `${XDG_RUNTIME_DIR}/syauth/auth.sock`) so test harnesses
> can inject a mock daemon"), and #13 ("The PAM module's existing
> `BondStore::load` path is gone — the daemon owns the bond state;
> the PAM module is a thin RPC client"); §3 Decisions row "PAM ↔
> daemon transport" (length-prefixed CBOR over Unix socket); §4.3
> Performance ("Daemon-down latency: ≤ 50 ms (Unix socket connect
> fails fast)"); §6 Failure Taxonomy table (the exhaustive PAM-return
> mapping for `replay`, `bad-signature`, `denied`, `busy`,
> `response-timeout`, `offline`, `unknown-peer`, `transport-error`,
> `adapter-missing`).
>
> **Roadmap row:** `specs/unlock-proximity/ROADMAP.md` Step S-008.
>
> **Closure condition (verbatim from ROADMAP.md):**
>
> ```
> cargo test -p syauth-pam
> git grep -l "BluerAdvertiser" crates/syauth-pam/   # empty
> ```

## Roadmap Link

- Source roadmap: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-008.
- Feature: replace the body of
  `crates/syauth-pam/src/auth.rs::authenticate` with a blocking
  Unix-socket client that opens
  `${XDG_RUNTIME_DIR}/syauth/auth.sock`, sends `Request::Challenge {
  peer_id, nonce }`, awaits the typed `Response::Challenge { ok,
  signature, reason }`, and maps the daemon's `reason` string onto
  the PAM return-code matrix per SPEC §6 Failure Taxonomy. Drop the
  `BluerAdvertiser` import + the `acquire_peer` call site at
  `auth.rs:575`. Drop the `syauth_transport` dependency from
  `crates/syauth-pam/Cargo.toml`. Add a `socket=<path>` PAM argument
  parsed from the libpam `argv`. Ship a unit-test mock daemon
  (`MockDaemonHandle`) that replaces the legacy `BtPeer`-based
  scenario harness in `crates/syauth-pam/src/auth.rs::tests` and
  `crates/syauth-pam/tests/pam_e2e.rs`, plus a new
  `crates/syauth-pam/tests/pam_daemon_integration.rs` that spawns
  the real `syauth-presenced` binary in a hidden `--peripheral=fake`
  test mode against a tempdir socket and asserts a full
  challenge → response → `PAM_SUCCESS` round-trip.

## 1. Journey

When **the operator runs `sudo` against a Linux desktop whose PAM
stack carries `auth sufficient pam_syauth.so socket=…` ahead of the
FIDO and password modules**, I want to **see `pam_sm_authenticate`
open the daemon's Unix socket, send a typed CBOR challenge, await
the daemon's typed response within the SPEC §4.3 budget, and map
the response's `reason` field onto the PAM return-code matrix —
returning `PAM_SUCCESS` on `"ok"`, `PAM_AUTH_ERR` on
`"replay" | "bad-signature" | "denied"`, and `PAM_AUTHINFO_UNAVAIL`
on `"response-timeout" | "offline" | "busy" | "unknown-peer" |
"transport-error" | "adapter-missing"` plus on socket-missing /
connect-refused / write-fail (within ≤ 50 ms wall-clock)**, so I
can **(a) honour SPEC §3 scope item #11 (PAM is now a thin
Unix-socket RPC client; the daemon owns BlueZ), (b) honour SPEC §3
scope item #12 (the `socket=<path>` argument lets test harnesses
point PAM at a mock daemon), (c) honour SPEC §4.3 "daemon-down
latency ≤ 50 ms" so `sudo` falls through to FIDO without a
user-visible stall when the daemon is offline, (d) honour SPEC §6
Failure Taxonomy by routing every typed `reason` through a single
`outcome_reason_to_pam` mapper that returns the canonical PAM
return code, and (e) preserve the existing PAM unit-test surface
(22 unit + 11 e2e tests) by reworking the scenario harness around a
`MockDaemonHandle` whose public-shape coverage of
`pam_sm_authenticate` outcomes (success / replay /
response-timeout / transport-error / falls-through-when-offline /
falls-through-when-daemon-down) is unchanged**.

## 2. CJM

Before S-008, the PAM module spun up a tokio runtime per call,
loaded `bonds.toml`, looked up a bond key, built a v1 challenge
frame, instantiated a `syauth_transport::BluerAdvertiser`, ran a
`connect → send_frame → recv_frame` round-trip on the BlueZ
adapter, verified the response signature + tag locally, and mapped
the result onto a PAM return code. That model is structurally
incompatible with the desktop architecture S-001..S-007 delivered:
the BlueZ adapter is owned by the long-running `syauth-presenced`
daemon, not by `pam_syauth.so`. Two BlueZ owners in one BLE stack
fight over advertising and characteristic registration; the PAM
module's per-call advertise also defeats the phone-side
`autoConnect=true` persistent GATT connection — the SPEC §3 #1
load-bearing primitive for the sub-2-second unlock budget. S-008
closes the gap by reducing `pam_sm_authenticate` to a thin
Unix-socket RPC client: open the socket, write one CBOR-framed
`Request::Challenge { peer_id, nonce }`, read one CBOR-framed
`Response::Challenge { ok, signature, reason }`, map the typed
outcome onto the SPEC §6 Failure Taxonomy via a single
`outcome_reason_to_pam` mapper, audit-log one line, return.

### Phase 1: sudo on a daemon-up system succeeds

**User Intent:** the operator runs `sudo whoami` while the
`syauth-presenced` user-service is running and a paired Android
phone is in BLE range. The operator's `/etc/pam.d/sudo` has
`auth sufficient pam_syauth.so` ahead of FIDO and password.
`pam_sm_authenticate` opens `${XDG_RUNTIME_DIR}/syauth/auth.sock`,
writes a typed `Request::Challenge { peer_id, nonce: [0u8; 16] }`,
the daemon's orchestrator notifies on the per-peer GATT
characteristic, the phone shows BiometricPrompt, the user taps,
the daemon verifies the signature and writes back
`Response::Challenge { ok: true, signature: Some(64 bytes),
reason: "ok" }`. `pam_sm_authenticate` returns `PAM_SUCCESS`.

**Actions:**

1. `pam_sm_authenticate` parses its `argv` for a `socket=<path>`
   argument. Absent → default to
   `${XDG_RUNTIME_DIR}/syauth/auth.sock` (with the `/run/user/$UID`
   fallback from `Config::resolve_socket_path` when
   `XDG_RUNTIME_DIR` is unset).
2. The module loads `bonds.toml` (still `BondStore::load`),
   picks the first non-revoked peer, captures its `peer_id`, and
   drops the bond_key / pubkey lookup paths — the daemon owns
   those.
3. The module opens a blocking `std::os::unix::net::UnixStream`
   to the configured path with `connect_timeout = DAEMON_CONNECT_TIMEOUT`
   (50 ms).
4. The module writes a length-prefixed CBOR frame via
   `write_frame_blocking(&mut stream, &Request::Challenge { peer_id,
   nonce })` (the nonce is a fresh 16-byte buffer from `getrandom::fill`
   so the wire field is non-trivial even though the daemon
   ignores it in S-006 and generates its own fresh nonce).
5. The module reads one length-prefixed CBOR frame via
   `read_frame_blocking::<Response>(&mut stream)` with a
   `read_timeout = DAEMON_RESPONSE_BUDGET` (1200 ms — the
   `DEFAULT_AUTH_TIMEOUT` plus headroom, NOT the SPEC §4.3
   user-attention cap).
6. The module passes `response.reason` through
   `outcome_reason_to_pam(reason)` to compute the PAM code.
   `Response::Challenge { ok: true, reason: "ok" }` →
   `PAM_SUCCESS`.

**Pain / Risk:**

- A daemon that returns `ok: true` but a non-"ok" reason is
  malformed; the PAM module trusts `reason` over `ok` (the
  match arm on `reason` is the canonical path) so the
  return code is always derived from the typed reason string.
- A daemon that returns a CBOR variant the PAM module does not
  understand (e.g., a future `Response::Status` returned to a
  `Request::Challenge`) is malformed; ciborium's typed decode
  raises a `FrameError::Decode`, which the PAM module maps to
  `transport-error` → `PAM_AUTHINFO_UNAVAIL`.
- The `socket=<path>` argument MUST be parsed defensively — a
  caller that passes `socket=` with no value, or
  `socket=/path with spaces/sock`, must not panic; the module's
  parser accepts the suffix verbatim and the connect attempt
  fails fast if the path is invalid.

**Success Signal:** the unit test
`authenticate_returns_success_on_daemon_ok` spawns a mock daemon
on a tempdir socket, drives `authenticate(&cfg)`, and asserts the
outcome is `AuthOutcome::Success { peer_id }`. The integration
test `end_to_end_against_real_daemon_binary` spawns the real
`syauth-presenced` binary against a tempdir socket, points PAM at
the same socket, and asserts `AuthOutcome::Success`.

### Phase 2: sudo on a daemon-down system falls through to FIDO/password within ≤ 50 ms

**User Intent:** the operator runs `sudo whoami` while
`syauth-presenced` is stopped (e.g., `systemctl --user stop
syauth-presenced`) or has never been started. `pam_sm_authenticate`
must fail fast so the next module in the stack
(`pam_u2f.so` → `pam_unix.so`) runs without a user-visible delay.
SPEC §4.3 caps daemon-down latency at 50 ms.

**Actions:**

1. The module computes the socket path (default or `socket=<arg>`).
2. The module calls
   `UnixStream::connect_timeout(&path, DAEMON_CONNECT_TIMEOUT)`.
   ENOENT (socket missing), ECONNREFUSED (no listener), and
   ETIMEDOUT all surface as `io::Error`.
3. The module returns `AuthOutcome::AuthInfoUnavail { reason:
   "transport-error", peer_id: None }`.
   `outcome_reason_to_pam("transport-error")` → `PAM_AUTHINFO_UNAVAIL`.
4. PAM's loader runs the next module in the stack (configured by
   `auth sufficient pam_syauth.so` followed by `auth sufficient
   pam_u2f.so` and `auth required pam_unix.so`).

**Pain / Risk:**

- A 50 ms `connect_timeout` is the SPEC's hard budget. The test
  `authenticate_falls_through_when_daemon_socket_missing`
  measures wall-clock from before the `authenticate` call to
  after, and asserts ≤ 50 ms (with a small headroom — the
  default + a 30 ms test-harness slack).
- A socket file that exists but no daemon is listening
  (left-over file after a crash) gets ECONNREFUSED instantly;
  the test must cover both ENOENT and ECONNREFUSED, and both
  must land within the 50 ms budget.
- A daemon that accepts the connection but never replies (slow
  daemon) is the Phase-3 case, NOT this phase — that path
  returns `response-timeout` after `DAEMON_RESPONSE_BUDGET`
  (1200 ms), not within 50 ms.

**Success Signal:** the unit test
`authenticate_falls_through_when_daemon_socket_missing` points
PAM at a `/nonexistent/path` socket, measures the wall-clock cost
of one `authenticate(&cfg)` call, and asserts both
`AuthInfoUnavail { reason: "transport-error" }` AND elapsed
`≤ DAEMON_CONNECT_TIMEOUT + DAEMON_FAST_FAIL_SLACK`.

### Phase 3: attacker replays a captured response → AUTH_ERR with reason=replay

**User Intent:** an attacker has captured a previous
`(challenge, response)` pair off the BLE link (or off the daemon's
audit log) and replays the response bytes on the phone's response
characteristic before the legitimate phone responds. The daemon's
LRU nonce cache (S-007) sees the nonce-on-nonce collision and
returns `Response::Challenge { ok: false, signature: None, reason:
"replay" }`. The PAM module maps `"replay"` to `PAM_AUTH_ERR` so
the sudo prompt fails closed (the stack STOPS — no
fall-through to FIDO, because a replay is attack-shaped, not
peer-offline-shaped).

**Actions:**

1. The PAM module sends `Request::Challenge { peer_id, nonce }`.
2. The daemon notifies, the replayed response arrives, the
   daemon's nonce LRU rejects it, and the daemon writes back
   `Response::Challenge { ok: false, signature: None, reason:
   "replay" }`.
3. The PAM module's `outcome_reason_to_pam("replay")` returns
   `PAM_AUTH_ERR`.
4. The audit log records `failure <peer_id>` plus the
   `reason=replay` segment so `tail /var/lib/syauth/last.log |
   grep replay` lights up.

**Pain / Risk:**

- The PAM module MUST NOT silently downgrade `"replay"` to
  `PAM_AUTHINFO_UNAVAIL` — that would let a relay attacker
  fall through to a weaker auth method by triggering a replay
  on every sudo. `outcome_reason_to_pam` pins `"replay"` →
  `PAM_AUTH_ERR`.
- The PAM module ALSO maps `"bad-signature"` and `"denied"` to
  `PAM_AUTH_ERR` (attack-shaped or user-rejected). Every other
  daemon reason (`"response-timeout"`, `"offline"`, `"busy"`,
  `"unknown-peer"`, `"transport-error"`, `"adapter-missing"`)
  maps to `PAM_AUTHINFO_UNAVAIL` so the stack falls through.
- An unknown `reason` string (forward-compat: a future daemon
  returns a reason this version of PAM does not recognise) is
  defensively mapped to `PAM_AUTH_ERR` — failing closed is the
  conservative reading when the wire surface drifts.

**Success Signal:** the unit test
`authenticate_maps_replay_to_auth_err` spawns a mock daemon that
replies `Response::Challenge { ok: false, signature: None,
reason: "replay" }` and asserts the outcome is
`AuthOutcome::AuthErr { reason: "replay", peer_id: Some(_) }`.
Symmetric assertions exist for `"bad-signature"` and
`"denied"`. Symmetric `AuthInfoUnavail` assertions exist for
`"response-timeout"`, `"offline"`, `"busy"`, `"unknown-peer"`,
`"transport-error"`, and `"adapter-missing"`.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| The legacy `auth.rs` body carried 853 lines of frame-building, transport handling, replay-cache wiring, and BluerAdvertiser instantiation — all of which the daemon now owns | 1 | The S-008 rewrite collapses `authenticate_inner` to ~60 lines: load `bonds.toml`, pick a `peer_id`, open the socket, write one frame, read one frame, map the `reason` to a typed `AuthOutcome`, return. The single `outcome_reason_to_pam(reason) -> PamCode` mapper is the canonical SPEC §6 contract in code |
| Two PAM tests in `tests/pam_e2e.rs` (`tc09_revoked_peer_never_touches_radio`, `tc10_setcred_returns_pam_success`) exercise paths that don't depend on the transport substrate; they MUST keep working under the new model | 1 | The revoked-peer path is unchanged (no eligible bond → `AuthInfoUnavail("no bonded peer")` before any socket op); the setcred path is unchanged (no transport involved). The other nine pam_e2e tests drive scenario-shaped daemon responses via `MockDaemonHandle`, preserving the public-shape coverage of every SPEC §6 reason |
| `BondStore::load` STAYS in the PAM module for `peer_id` resolution — the SPEC §3 scope item #13 says "the daemon owns the bond state" but PAM still needs to know which `peer_id` to challenge | 1 | Documented deviation: PAM loads `bonds.toml` only to pick a `peer_id`; it no longer reads `keys/<peer_id>.bin` (the daemon does) or verifies signatures (the daemon does). The bond_key / pubkey lookup paths are deleted. This deviation is noted explicitly so a reviewer doesn't go looking for the missing keystore code |
| Wire-frame helpers (`read_frame`, `write_frame`) in `rpc.rs` are tokio-based, but the PAM module is a blocking, non-async cdylib loaded into sudo's process — bringing a tokio runtime into `libpam_syauth.so` would add 30+ MB of binary bloat for one round-trip | 1, 2, 3 | Add `pub fn read_frame_blocking<R: Read>(r: &mut R) -> Result<T, FrameError>` and `pub fn write_frame_blocking<W: Write>(w: &mut W, value: &T) -> Result<(), FrameError>` to `crates/syauth-presenced/src/rpc.rs`. The encode / decode helpers (`encode_frame`, `decode_frame`) already exist and are sync — the new blocking I/O helpers reuse them. Daemon (tokio) and PAM (blocking) share one canonical wire-format module |
| The `pam_daemon_integration.rs` test needs to spawn the real daemon binary but the daemon's production path opens BlueZ — CI has no radio | 1 | The daemon binary grows a hidden `--peripheral=fake` flag (`hide = true`, like the existing `--pidfile`). When set, `runtime::run` routes `maybe_spawn_orchestrator` through `FakePeripheral::new()` instead of `PersistentPeripheral::new(DEFAULT_ADAPTER_NAME)`, and the test's helper drives the fake side via `FakePeripheral::inject_response(peer_id, signed_bytes)` exposed on a small control channel (a second hidden flag `--inject-response <peer_id>:<hex>` that pre-seeds the fake's response queue before the accept loop starts) |
| The legacy `MOCK_PEER` / `KEYSTORE_FOR_TESTS` slots in `auth.rs` are obsolete after the rewrite — they're scaffolding around the BluerAdvertiser path that's gone | 1 | Delete `MOCK_PEER`, `KEYSTORE_FOR_TESTS`, `install_mock_peer`, `install_test_keystore`, the `BOND_KEY_PREFIX`, the `KeyStore` import surface, and the `replay_seed` module. The S-008 mock-daemon harness owns the new test surface; the old slots remain only as `// GAP: S-019` placeholders for the future kernel-keyring path, which is not in S-008's scope |
| Each `Config::adapter_id`, `Config::mock_peer_enabled`, `Config::response_timeout` knob existed to control the BluerAdvertiser path; they no longer apply once the daemon owns the transport | 1 | Replace them with `Config::socket_path` (the new `socket=<path>` argument's resolved value), `Config::auth_timeout` (still capped at `DAEMON_RESPONSE_BUDGET` for the daemon round-trip), and drop the rest. The legacy `DEFAULT_RESPONSE_TIMEOUT`, `DEFAULT_ADAPTER_NAME`, `TEST_MOCK_ENV_VAR`, `TEST_MOCK_ENV_ENABLED_VALUE`, `ADAPTER_ENV_VAR` constants are removed |

### North Star Summary

After S-008 closes, `pam_sm_authenticate` is a thin Unix-socket
RPC client: open a 0o600-mode socket, write one CBOR-framed
`Request::Challenge`, read one CBOR-framed `Response::Challenge`,
map `response.reason` through a single
`outcome_reason_to_pam(reason: &str) -> PamCode` mapper, return.
`PAM_SUCCESS` on `"ok"`; `PAM_AUTH_ERR` on `"replay" |
"bad-signature" | "denied"`; `PAM_AUTHINFO_UNAVAIL` on
`"response-timeout" | "offline" | "busy" | "unknown-peer" |
"transport-error" | "adapter-missing"`; defensive `PAM_AUTH_ERR`
on any unknown reason. Socket missing / connect-refused / write
fail → `PAM_AUTHINFO_UNAVAIL` within 50 ms (the SPEC §4.3
daemon-down latency cap). The PAM module no longer imports
`syauth_transport::BluerAdvertiser` (verified by `git grep -l
"BluerAdvertiser" crates/syauth-pam/`). The `socket=<path>`
argument lets test harnesses point PAM at a mock daemon
(`MockDaemonHandle` for unit tests, the real `syauth-presenced`
binary in `--peripheral=fake` mode for the integration test).

## 3. UX Implementation and Assessment

### Time to First Value
- [x] First `sudo` after S-008 closes returns within the SPEC
      §4.3 budget on the happy path; the integration test
      `end_to_end_against_real_daemon_binary` pins the round-trip
      under 2 s against the real daemon binary.
- [x] Daemon-down case fails fast within 50 ms (the
      `authenticate_falls_through_when_daemon_socket_missing`
      test asserts the wall-clock budget).

### Onboarding Clarity
- [x] Named constants `DAEMON_CONNECT_TIMEOUT`,
      `DAEMON_WRITE_TIMEOUT`, `DAEMON_RESPONSE_BUDGET`,
      `OUTCOME_REASON_*` document the mapping inline.
- [x] The PAM `socket=<path>` argument is the only operator-facing
      knob added; documented in `docs/pam-syauth.md`.

### Production-Ready Defaults
- [x] Default socket path is
      `${XDG_RUNTIME_DIR}/syauth/auth.sock` (with
      `/run/user/$UID` fallback) — matches the daemon's default.
- [x] Default `DAEMON_RESPONSE_BUDGET = 1200 ms` matches the
      daemon's `DEFAULT_AUTH_TIMEOUT` (no operator knob).

### Golden Path Quality
- [x] One typed RPC per PAM call: `open → write_frame_blocking →
      read_frame_blocking → outcome_reason_to_pam → return`.
- [x] No retries inside one PAM call (SPEC §6 contract).

### Decision Load
- [x] The PAM `socket=<path>` argument is the only knob; the
      module's default DOES NOT require operator configuration.

### Progressive Complexity
- [x] The PAM module's public surface is unchanged: `extern "C"
      pam_sm_authenticate` still takes the libpam ABI and still
      delegates to `auth::authenticate(&cfg)`.

### Error Quality
- [x] Every `Response::Challenge { reason }` value the daemon
      emits has an `OUTCOME_REASON_*` constant; the
      `outcome_reason_to_pam` mapper is a single `match`.
- [x] Connect errors, write errors, read errors, and decode
      errors all map to `"transport-error"` →
      `PAM_AUTHINFO_UNAVAIL`.

### Failure Safety
- [x] A panic anywhere in `auth::authenticate` is caught by
      `entry::run_entry`'s `catch_unwind` and translated to
      `PAM_AUTH_ERR` per the existing S-008-skeleton contract.
- [x] An unknown `reason` string is defensively mapped to
      `PAM_AUTH_ERR` — failing closed on wire-format drift is
      the SPEC §6 conservative reading.

### Runtime Transparency
- [x] One audit-log line per PAM call recording the outcome.
- [x] One syslog line per PAM call recording the reason +
      peer_id.

### Debuggability
- [x] The integration test's tempdir houses the daemon's
      `auth.sock`, `bonds.toml`, `keys/`, and `last.log`; an
      operator running the test under `--nocapture` sees the
      full audit trail.

### Cross-Surface Consistency
- [x] `OUTCOME_REASON_*` constants are re-exported from
      `syauth-presenced::orchestrator` so both daemon and PAM
      read the same strings.

### Workflow Consistency
- [x] `pam_sm_authenticate` shape (parse argv → load Config →
      call `auth::authenticate(&cfg)` → map outcome) is unchanged
      from the S-009-era skeleton.

### Change Safety
- [x] The PAM ABI (`pub unsafe extern "C" fn
      pam_sm_authenticate`) is unchanged.
- [x] The `Response::Challenge` wire shape is unchanged from
      S-002.

### Experimentation Safety
- [x] `MockDaemonHandle` lets tests drive scenario-shaped
      responses without binding a real Unix socket on the
      operator's tmpfs.

### Interaction Latency
- [x] One `connect_timeout(50ms)` + one `write_all` + one
      `read_exact` per PAM call. No polling, no retries.

### Developer Feedback Speed
- [x] `cargo test -p syauth-pam` runs in seconds (every test
      uses a tempdir socket; no real radio).

### Team Scale
- [x] `read_frame_blocking` / `write_frame_blocking` live in
      `syauth-presenced::rpc` so both daemon (tokio) and PAM
      (blocking) share the canonical wire-format module.

### System Scale
- [x] Adding a new `Response::Challenge::reason` value requires
      only adding an `OUTCOME_REASON_*` constant + a new arm in
      `outcome_reason_to_pam`. No other PAM code changes.

### Right Behavior by Default
- [x] All named constants in scope: `DAEMON_CONNECT_TIMEOUT`,
      `DAEMON_WRITE_TIMEOUT`, `DAEMON_RESPONSE_BUDGET`,
      `DAEMON_FAST_FAIL_SLACK`, `PAM_SOCKET_ARG_PREFIX`, plus
      the typed reason constants `OUTCOME_REASON_OK`,
      `OUTCOME_REASON_DENIED`, `OUTCOME_REASON_REPLAY`,
      `OUTCOME_REASON_BAD_SIGNATURE`,
      `OUTCOME_REASON_RESPONSE_TIMEOUT`,
      `OUTCOME_REASON_OFFLINE`, `OUTCOME_REASON_BUSY`,
      `OUTCOME_REASON_UNKNOWN_PEER`,
      `OUTCOME_REASON_TRANSPORT_ERROR`,
      `OUTCOME_REASON_ADAPTER_MISSING`.

### Anti-Bypass Design
- [x] Every PAM-return path flows through
      `outcome_reason_to_pam`. There is no codepath that returns
      a PAM code without going through the mapper.

## Acceptance Criteria (DoD, verbatim from ROADMAP.md Step S-008)

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

## 4. Tests

### TC-01: `authenticate_falls_through_when_daemon_socket_missing`

**Given** a `Config` whose `socket_path` points at a
nonexistent file under a tempdir. **When** the test calls
`authenticate(&cfg)` and measures the wall-clock cost.
**Then** the outcome is `AuthOutcome::AuthInfoUnavail { reason:
"transport-error", peer_id: None }` AND the wall-clock cost is
≤ `DAEMON_CONNECT_TIMEOUT + DAEMON_FAST_FAIL_SLACK`.

### TC-02: `authenticate_returns_success_on_daemon_ok`

**Given** a `MockDaemonHandle` bound to a tempdir socket that
replies `Response::Challenge { ok: true, signature: Some([0xAA;
SIGNATURE_LEN].to_vec()), reason: "ok" }` to any incoming
`Request::Challenge`. **When** the test calls
`authenticate(&cfg)` with `cfg.socket_path` pointing at the
mock daemon and a `bonds.toml` carrying one Bonded peer.
**Then** the outcome is `AuthOutcome::Success { peer_id }`
where `peer_id` matches the bond.

### TC-03: `authenticate_maps_busy_to_authinfo_unavail`

**Given** a `MockDaemonHandle` that replies
`Response::Challenge { ok: false, signature: None, reason:
"busy" }`. **When** the test calls `authenticate(&cfg)`.
**Then** the outcome is `AuthOutcome::AuthInfoUnavail { reason:
"busy", peer_id: Some(_) }` AND `outcome.to_pam_code()` returns
`PAM_AUTHINFO_UNAVAIL`.

### TC-04: `authenticate_maps_replay_to_auth_err`

**Given** a `MockDaemonHandle` that replies
`Response::Challenge { ok: false, signature: None, reason:
"replay" }`. **When** the test calls `authenticate(&cfg)`.
**Then** the outcome is `AuthOutcome::AuthErr { reason:
"replay", peer_id: Some(_) }` AND `outcome.to_pam_code()`
returns `PAM_AUTH_ERR`.

### TC-05: `authenticate_maps_bad_signature_to_auth_err`

**Given** a `MockDaemonHandle` that replies
`Response::Challenge { ok: false, signature: None, reason:
"bad-signature" }`. **When** the test calls
`authenticate(&cfg)`. **Then** the outcome is
`AuthOutcome::AuthErr { reason: "bad-signature", peer_id:
Some(_) }`.

### TC-06: `authenticate_maps_denied_to_auth_err`

**Given** a `MockDaemonHandle` that replies
`Response::Challenge { ok: false, signature: None, reason:
"denied" }`. **When** the test calls `authenticate(&cfg)`.
**Then** the outcome is `AuthOutcome::AuthErr { reason:
"denied", peer_id: Some(_) }`.

### TC-07: `authenticate_maps_response_timeout_to_authinfo_unavail`

**Given** a `MockDaemonHandle` that replies
`Response::Challenge { ok: false, signature: None, reason:
"response-timeout" }`. **When** the test calls
`authenticate(&cfg)`. **Then** the outcome is
`AuthOutcome::AuthInfoUnavail { reason: "response-timeout",
peer_id: Some(_) }`.

### TC-08: `authenticate_maps_offline_to_authinfo_unavail`

**Given** a `MockDaemonHandle` that replies
`Response::Challenge { ok: false, signature: None, reason:
"offline" }`. **When** the test calls `authenticate(&cfg)`.
**Then** the outcome is `AuthOutcome::AuthInfoUnavail { reason:
"offline", peer_id: Some(_) }`.

### TC-09: `authenticate_maps_unknown_peer_to_authinfo_unavail`

**Given** a `MockDaemonHandle` that replies
`Response::Challenge { ok: false, signature: None, reason:
"unknown-peer" }`. **When** the test calls
`authenticate(&cfg)`. **Then** the outcome is
`AuthOutcome::AuthInfoUnavail { reason: "unknown-peer",
peer_id: Some(_) }`.

### TC-10: `authenticate_maps_transport_error_to_authinfo_unavail`

**Given** a `MockDaemonHandle` that replies
`Response::Challenge { ok: false, signature: None, reason:
"transport-error" }`. **When** the test calls
`authenticate(&cfg)`. **Then** the outcome is
`AuthOutcome::AuthInfoUnavail { reason: "transport-error",
peer_id: Some(_) }`.

### TC-11: `authenticate_maps_unknown_reason_defensively_to_auth_err`

**Given** a `MockDaemonHandle` that replies
`Response::Challenge { ok: false, signature: None, reason:
"future-version-skew" }`. **When** the test calls
`authenticate(&cfg)`. **Then** the outcome is
`AuthOutcome::AuthErr` (defensive failing-closed mapping for
forward-compat).

### TC-12: `pam_sm_authenticate_parses_socket_argument`

**Given** an `argv` containing `["socket=/tmp/x.sock"]`. **When**
the test calls `Config::from_pam_argv(&argv)`. **Then**
`cfg.socket_path == PathBuf::from("/tmp/x.sock")`.

### TC-13: `outcome_reason_to_pam_pins_failure_taxonomy`

**Given** each canonical reason string defined in SPEC §6
Failure Taxonomy. **When** the test calls
`outcome_reason_to_pam(reason)`. **Then** the return code
matches the SPEC table for every reason.

### TC-14: `end_to_end_against_real_daemon_binary` (integration)

**Given** the real `syauth-presenced` binary spawned with
`--socket <tempdir>/auth.sock`, `--bonds-file <tempdir>/bonds.toml`,
`--keys-dir <tempdir>/keys`, `--audit-log <tempdir>/last.log`,
`--pidfile <tempdir>/presenced.pid`, `--peripheral=fake`, and
`--inject-response <peer_id>:<hex-of-signed-response>`. **When**
the test waits for the socket to bind, writes a fake bond to
`bonds.toml` (signed by a known Ed25519 keypair), points PAM at
the same socket, and calls `authenticate(&cfg)`. **Then** the
outcome is `AuthOutcome::Success { peer_id }` matching the bond.

## Implementation

Files created:

- `specs/journeys/JOURNEY-S-008-pam-unix-socket-client.md` —
  this document.
- `crates/syauth-pam/tests/pam_daemon_integration.rs` — the
  integration test (TC-14) that spawns the real daemon binary.

Files modified:

- `crates/syauth-pam/src/auth.rs` — rewrite of `authenticate`
  around a blocking Unix-socket client: drops the
  `BluerAdvertiser` import, the `BtPeer` / `Session` round-trip,
  the `BondStore`-driven bond_key + pubkey lookup, the
  `verify_frame` + `verify_tag` calls, the `ReplayCache`, and
  the `MOCK_PEER` / `KEYSTORE_FOR_TESTS` slots. Adds the
  `outcome_reason_to_pam` mapper and a small
  `daemon_round_trip` helper that opens the socket, writes a
  `Request::Challenge`, and reads a `Response::Challenge`.
- `crates/syauth-pam/src/config.rs` — drops
  `mock_peer_enabled`, `adapter_id`, `response_timeout`,
  `DEFAULT_ADAPTER_NAME`, `ADAPTER_ENV_VAR`,
  `TEST_MOCK_ENV_VAR`, `TEST_MOCK_ENV_ENABLED_VALUE`,
  `DEFAULT_RESPONSE_TIMEOUT`. Adds `socket_path: PathBuf`,
  `PAM_SOCKET_ARG_PREFIX`, `XDG_RUNTIME_DIR_ENV`,
  `DEFAULT_RUNTIME_FALLBACK_PREFIX`,
  `Config::resolve_socket_path` and
  `Config::from_pam_argv(argv)`.
- `crates/syauth-pam/src/entry.rs` — `pam_sm_authenticate` now
  reads the libpam `argv` via a small parser and builds the
  `Config` from `Config::from_pam_argv`. The
  `unsafe extern "C" fn` signature is unchanged.
- `crates/syauth-pam/src/lib.rs` — unchanged module list; the
  module-doc paragraph is rewritten to reflect the
  Unix-socket-client shape.
- `crates/syauth-pam/Cargo.toml` — drops the `syauth-transport`
  dependency (production + dev), the `tokio` dependency, the
  `async-trait` dependency, the `ed25519-dalek` dev-dependency,
  the `getrandom` dependency. Adds `syauth-presenced = { path =
  "../syauth-presenced", default-features = false }` (for the
  typed `Request`/`Response` + `read_frame_blocking` /
  `write_frame_blocking` helpers) and the `tempfile`
  dev-dependency stays for the daemon-spawn integration test.
- `crates/syauth-pam/tests/pam_e2e.rs` — reworked around
  `MockDaemonHandle`: the 11 e2e tests now drive scenario-shaped
  daemon responses through the mock daemon instead of through
  the `MOCK_PEER` / `BtPeer` injection slot. The shape of every
  test (one assertion per SPEC §4.3 scenario) stays the same;
  only the substrate changes.
- `crates/syauth-presenced/src/rpc.rs` — adds
  `pub fn read_frame_blocking<R: Read, T: for<'de>
  Deserialize<'de>>(r: &mut R) -> Result<T, FrameError>` and
  `pub fn write_frame_blocking<W: Write, T: Serialize>(w: &mut
  W, value: &T) -> Result<(), FrameError>` so the PAM
  (blocking) module and the daemon (tokio) share one wire-format
  module.
- `crates/syauth-presenced/src/main.rs` — adds the hidden
  `--peripheral=fake` flag (S-008 test seam) and the hidden
  `--inject-response <peer_id>:<hex>` flag; wires them into
  `runtime::Config::peripheral_mode` /
  `runtime::Config::inject_response`.
- `crates/syauth-presenced/src/runtime.rs` — adds
  `Config::peripheral_mode` (production / fake), threads it
  into `maybe_spawn_orchestrator` so the orchestrator's
  `Peripheral` is `PersistentPeripheral` in production and
  `FakePeripheral` under `--peripheral=fake`. Adds
  `Config::inject_response` so the test seam can pre-seed the
  fake's response queue before the accept loop starts.
- `crates/syauth-presenced/src/lib.rs` — re-exports
  `read_frame_blocking`, `write_frame_blocking`,
  `PeripheralMode`.
- `specs/unlock-proximity/ROADMAP.md` — ticks S-008 DoD bullets
  and appends the `Traceability` line.

### Deviations

This step has one documented deviation from SPEC §3 scope item #13
("The PAM module's existing `BondStore::load` path is gone — the
daemon owns the bond state; the PAM module is a thin RPC client").

PAM still calls `BondStore::load` to look up the first non-revoked
peer's `peer_id` so the `Request::Challenge { peer_id, nonce }`
wire frame carries a real identifier. The bond_key and pubkey
lookup paths ARE gone (the daemon owns the heavy crypto). The
alternative the SPEC paragraph implies is for the daemon to accept
`Request::Challenge { peer_id: None }` meaning "you pick, daemon"
— which pushes the user-account ↔ peer routing into the daemon's
state machine, which is out of S-008's scope (the daemon currently
takes a `peer_id: String` per `crates/syauth-presenced/src/rpc.rs`
S-002 wire format). The S-008 reading: keep the
`BondStore::load → peer_id` call site in PAM, drop the bond_key /
pubkey lookups, and document the deviation here for a future row
to revisit if the SPEC paragraph is re-litigated.

## Traceability

- Roadmap item: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-008.
- Implementation files: see "Implementation" above.
- Test files: `crates/syauth-pam/src/auth.rs` unit-tests
  module + `crates/syauth-pam/tests/pam_e2e.rs` +
  `crates/syauth-pam/tests/pam_daemon_integration.rs`.
