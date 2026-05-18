# JOURNEY-S-002: CBOR-framed Unix-socket RPC server (stub responder)

> **Spec anchors:** `specs/unlock-proximity/SPEC.md` §3 Decisions row
> "PAM ↔ daemon transport" (the
> `${XDG_RUNTIME_DIR}/syauth/auth.sock` + length-prefixed CBOR contract),
> §4 Architecture "Data flow per unlock" (the daemon as the only
> reader/writer on the socket), and §7 Trust Boundaries +
> §7 T-Local-Privilege-Escalation + §7 T-Daemon-DoS (the `SO_PEERCRED`
> UID-match rule and the concurrent-accept cap of 4).
>
> **Roadmap row:** `specs/unlock-proximity/ROADMAP.md` Step S-002.
>
> **Closure condition (verbatim from ROADMAP.md):**
> `cargo test -p syauth-presenced --test socket_smoke`
> — all three tests pass.

## Roadmap Link

- Source roadmap: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-002.
- Feature: the daemon's typed Unix-socket RPC surface plus the accept
  loop. This step ships the wire format (typed `Request` / `Response`
  enum, ciborium encode/decode, 4-byte big-endian length prefix), the
  socket bind with `0600` mode, the `SO_PEERCRED` UID-match check on
  every accept, and the `tokio::sync::Semaphore(4)` concurrent-accept
  cap. `ChallengeRequest` is answered with a stub
  `ChallengeResponse { ok: false, reason: "not-implemented" }`; later
  S-004..S-006 rows wire the rotation timer + the real challenge state
  machine on top of this skeleton.

## 1. Journey

When **`pam_syauth` (the PAM module) needs a typed, ACL-gated control
channel to the daemon so the PAM module itself does no radio work**,
I want to **open `${XDG_RUNTIME_DIR}/syauth/auth.sock`, send a
length-prefixed CBOR `ChallengeRequest`, and receive a typed
`ChallengeResponse` (today the stub `ok=false reason=not-implemented`,
later the real signed-by-phone response)**, so I can **rely on a
single, mockable transport for every later step — S-006 wires the
real challenge state machine without retouching the framing, S-007
adds backpressure on the same connection, and S-008 rewrites
`pam_sm_authenticate` against this exact wire format with zero
guesswork**.

## 2. CJM

S-001 left a daemon process that boots, takes the single-instance
pidfile lock, idles in an empty tokio loop, and shuts down on
SIGINT/SIGTERM. The daemon owns no I/O yet — the SPEC's "open
`${XDG_RUNTIME_DIR}/syauth/auth.sock`" cold-start step is half-done
(lock acquired, bind not yet). S-002 closes the second half: the
socket is bound, the kernel-side `SO_PEERCRED` ACL is enforced on
every accept, and the daemon answers every well-framed
`ChallengeRequest` with a typed stub response so the wire format is
locked in before the orchestrator + the real challenge flow arrive.
Friction today: there is no transport for the PAM module to talk to,
so neither S-008's PAM rewrite nor any integration test can be
written. This journey removes that friction.

### Phase 1: `pam_syauth` opens the socket and the daemon accepts

**User Intent:** prove the daemon binds the socket where the SPEC
says it does, with the SPEC's mode, and answers every typed request
with a typed response.

**Actions:**

- The PAM module (or, today, a smoke-test client running as the same
  UID) opens `${XDG_RUNTIME_DIR}/syauth/auth.sock` and sends a
  length-prefixed CBOR `Request::Challenge { peer_id, nonce }`.
- The daemon's accept loop pulls the connection off the queue, runs
  the `SO_PEERCRED` UID check, decodes the frame, and writes a
  `Response::Challenge { ok=false, signature=None, reason="not-implemented" }`
  back on the same connection.
- The client decodes the response and asserts the typed shape.

**Pain / Risk:**

- The socket file is created with the default umask (often
  `0o022` → mode `0o644`), so a non-owner local process can read /
  connect. Defense: explicit `chmod(0o600)` after bind.
- The 4-byte length prefix is signed/unsigned-mismatched between
  encoder and decoder, so a 3 GiB allocation request from a malformed
  client wedges the daemon. Defense: a named `MAX_FRAME_LEN` constant
  (chosen 64 KiB — three orders of magnitude above any legitimate
  CBOR-encoded request) rejected with a typed `FrameError::TooLarge`.
- The CBOR encoder writes a tagless variant and the decoder cannot
  tell `ChallengeResponse { ok=false }` from `ReloadResponse { ok=false }`.
  Defense: `#[serde(tag = "kind")]` on both enums so the wire format
  carries a discriminator.

**Success Signal:** TC-01 passes — `challenge_request_returns_stub`
sees the typed stub response within the test's 5-second budget.

### Phase 2: A non-matching UID connects and is dropped

**User Intent:** prove the daemon refuses connections whose
`SO_PEERCRED.uid` doesn't match the daemon's expected UID, so a
non-root local process on the same host cannot impersonate
`pam_syauth` (SPEC §7 T-Local-Privilege-Escalation).

**Actions:**

- The smoke test injects a deliberately-unreachable expected UID
  (`expected_uid = Some(0)` for a test that runs as a non-root user,
  guaranteeing a mismatch without needing the test to run as root or
  fork a child as a different uid).
- A client connects, sends a well-framed request.
- The daemon reads the peer credentials, sees `uid != expected_uid`,
  logs a `tracing::warn!`, and drops the connection without reading
  the request body.
- The client's read of the response returns EOF (`0` bytes) within
  the test budget.

**Pain / Risk:**

- The daemon reads-then-checks (instead of check-then-read), so a
  malicious frame is decoded before the ACL fires. Defense: the
  per-connection task does `getsockopt(PeerCredentials)` as its
  FIRST action, before any read.
- `getsockopt(PeerCredentials)` on a non-Linux platform is not
  defined; the daemon's tests pass on Linux only. Defense:
  `#[cfg(target_os = "linux")]` on the per-connection task; the
  workspace is Linux-only per SPEC §1 — this is intentional.
- The check passes silently when both UIDs are 0 (the root case),
  hiding a real misconfiguration. Defense: the typed
  `expected_uid: Option<u32>` field defaults to the daemon's own
  `geteuid()` so a misconfigured operator can't accidentally accept
  every peer.

**Success Signal:** TC-02 passes —
`rejects_non_matching_peer_credential` sees the connection EOF'd
without a response body.

### Phase 3: Concurrent accepts saturate the cap and the 5th is queued

**User Intent:** prove the SPEC §7 T-Daemon-DoS cap of 4 concurrent
accept slots is enforced. The daemon shall not serve more than 4
connections in parallel; the 5th sits in the kernel listen queue
until a permit is released.

**Decision (mirrors the prompt's "pick one"):** the
`tokio::sync::Semaphore(4)` is acquired BEFORE the per-connection
handler runs, so the 5th connection's permit-acquire awaits
indefinitely (no immediate-reject). The test asserts the 5th
connection has neither received a response nor been EOF'd while the
first 4 are stuck holding their permits. Releasing one permit
unblocks the 5th and it sees the stub response. This is the
"queue, do not reject" semantic — it matches the SPEC's "rate-limits
new connections" framing (cap with backpressure, not cap with
synchronous refuse) and keeps `pam_syauth`'s connect/write/read
sequence working under transient bursts without an extra retry loop
in the PAM module.

**Actions:**

- The smoke test opens 4 connections and writes a `Challenge` frame
  on each, then deliberately does NOT read the response so the
  daemon's per-connection task is parked on the write back (each
  task still holds its semaphore permit).
- The test opens a 5th connection and writes a frame.
- The test asserts the 5th connection's read times out within a
  short window (it is queued behind the semaphore) — the daemon has
  not closed it.
- The test releases one of the first 4 by reading-then-closing it;
  the 5th's read now returns the stub response within the budget.

**Pain / Risk:**

- A bug where the semaphore is acquired inside the per-connection
  task instead of around it means the 5th task is still spawned and
  reads from the connection — the cap appears to work in test (no
  response) but is actually unbounded under load. Defense: the
  permit is acquired in the accept loop, before `tokio::spawn`.
- The per-connection task panics; the permit is never released and
  the cap shrinks monotonically. Defense: `OwnedSemaphorePermit` is
  moved into the task and the permit's `Drop` releases it even on
  panic (tokio semantics).
- The 5th connection is closed by the kernel before the permit
  releases because the listen-backlog is too small (default `128` on
  Linux is fine; but if a future change lowers it, the test surfaces
  it). Defense: keep the default backlog (`UnixListener::bind`
  inherits `SOMAXCONN`).

**Success Signal:** TC-03 passes —
`concurrent_accept_cap_enforced` sees four in-flight responses, one
pending, and then progress on the pending after a permit releases.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Default umask leaks the socket file as `0o644` | 1 | Explicit `fchmodat(0o600)` after `bind`; a smoke-test assertion on the `st_mode & 0o777` value pins this forever |
| The 4-byte length prefix is unbounded → DoS via 4 GiB allocation request | 1 | Named `MAX_FRAME_LEN = 64 KiB` constant + a typed `FrameError::TooLarge { len, max }` |
| `SO_PEERCRED` check applied after a read → first malformed frame is processed before ACL fires | 2 | `getsockopt(PeerCredentials)` is the per-connection task's first action, before any byte is read |
| Concurrent-accept cap accidentally implemented inside the per-connection task → unbounded spawn | 3 | `Semaphore::acquire_owned` runs in the accept loop, between `listener.accept()` and `tokio::spawn` |

### North Star Summary

After S-002 closes, `${XDG_RUNTIME_DIR}/syauth/auth.sock` exists at
mode `0o600`, accepts up to 4 concurrent connections (queueing the
5th), rejects connections whose `SO_PEERCRED.uid` doesn't match the
daemon's expected UID, and answers every well-framed
`Request::Challenge` with a typed stub `Response::Challenge { ok=false,
reason="not-implemented" }`. The wire format (length-prefixed CBOR
with a `kind` discriminator) is the single transport for every
later step — S-005's `Reload` RPC, S-006's real challenge flow,
S-008's PAM rewrite, and S-017's `status` query all reuse this
exact framing with zero changes to the bytes on the wire.

## 3. UX Implementation and Assessment

### Time to First Value

- [x] `cargo test -p syauth-presenced --test socket_smoke` runs the
      three smoke tests against the in-process daemon in a few seconds.
- [x] A smoke client can connect, send, and receive a typed response
      without touching BlueZ or any radio.

### Onboarding Clarity

- [x] Wire format is one named module (`rpc.rs`) with one
      `MAX_FRAME_LEN` constant and one `LENGTH_PREFIX_BYTES` constant —
      no magic numbers.
- [x] Every `Request` / `Response` variant has a doc comment naming
      the SPEC clause that motivates it.

### Production-Ready Defaults

- [x] Socket mode `0o600`; verified by the smoke test reading
      `st_mode & 0o777`.
- [x] Concurrent-accept cap = 4; verified by TC-03.
- [x] Expected UID defaults to `geteuid()`; the test-only
      `expected_uid: Option<u32>` override is the single seam tests
      use.

### Golden Path Quality

- [x] TC-01 exercises the full bind → accept → decode → encode → reply
      path against the same code the daemon binary runs.

### Decision Load

- [x] No new env vars; the `--socket` flag from S-001 remains the
      single knob and tests inject `Config::socket` directly.

### Progressive Complexity

- [x] The stub responder has one branch (`Request::Challenge`); every
      other variant returns a typed `Response::*` shape so S-004..S-007
      can replace branches one at a time without touching the framing.

### Error Quality

- [x] `FrameError::TooLarge { len, max }` names the offending size
      and the cap, not just "frame too large".
- [x] Peer-credential mismatch is a structured `tracing::warn!` with
      `uid=`, `expected_uid=`, `peer_pid=` fields so the operator can
      grep the journal for impersonation attempts.

### Failure Safety

- [x] Socket file is unlinked on daemon shutdown (the existing
      pidfile cleanup path is mirrored by an RAII guard for the
      `UnixListener`'s path).
- [x] Per-connection task panic does not deadlock the cap; the
      `OwnedSemaphorePermit`'s `Drop` releases it.

### Runtime Transparency

- [x] One `tracing::info!` per accept; one `tracing::warn!` per ACL
      rejection; one `tracing::debug!` per request decoded.

### Debuggability

- [x] The `Request` / `Response` enums implement `Debug`; tracing
      events can log the typed shape directly without re-deriving.

### Cross-Surface Consistency

- [x] Wire format byte-for-byte mirrors the framing the
      `syauth-mobile` UniFFI surface already uses (4-byte BE length +
      CBOR payload, per the SPEC's "matches the existing UniFFI frame
      style" remark in §3 scope item #5).

### Workflow Consistency

- [x] Same `clap` flag (`--socket`) drives the daemon and the smoke
      test client.

### Change Safety

- [x] `#[serde(tag = "kind")]` means adding a new `Request` variant
      in S-005 (the `Reload` RPC) does NOT break wire-compat with
      existing `Challenge` callers.

### Experimentation Safety

- [x] Tests bind their socket in a `tempdir`, never touch
      `${XDG_RUNTIME_DIR}/syauth/auth.sock` on the developer's box.

### Interaction Latency

- [x] Stub responder writes the response in one frame; the round-trip
      is bounded by the local kernel's socket buffer, not the daemon's
      logic.

### Developer Feedback Speed

- [x] All three smoke tests share a `start_daemon()` helper that
      returns once the socket file exists, so the per-test wall-clock
      is dominated by the test's assertions, not by daemon boot.

### Team Scale

- [x] The `rpc` module is `pub` so the PAM crate (S-008) and the
      `syauth-cli status` subcommand (S-017) re-use the same types
      from one crate; no duplicated wire-format crates.

### System Scale

- [x] One tokio runtime, one accept loop, `Semaphore(4)` cap — adding
      more peers in S-005 / more variants in S-006 does not change the
      shape of this loop.

### Right Behavior by Default

- [x] Socket is `0o600`. UID check is on. Frame cap is on. Concurrent
      cap is on. No flag is required to opt into any of these.

### Anti-Bypass Design

- [x] The UID check fires before any read. `expected_uid` defaults to
      `geteuid()` so a misconfigured operator can't accidentally
      accept every peer; the test-only override is gated behind a
      typed `Option<u32>` field on `Config`, not a string-parsed CLI
      flag.

## 4. Tests

### TC-01: `challenge_request_returns_stub`

**Given** a daemon started with `Config::socket` pointing at a
tempdir socket path and `Config::expected_uid = None` (defaults to
`geteuid()` of the test process).

**When** a smoke-test client connects to the socket, sends a CBOR-
framed `Request::Challenge { peer_id: "test-peer", nonce: [0u8; 16] }`
with the 4-byte big-endian length prefix, and reads the typed
response.

**Then** the response decodes to `Response::Challenge { ok: false,
signature: None, reason: "not-implemented" }`, the socket file on
disk has mode `0o600`, and the daemon is still accepting new
connections (the cap was not consumed past this connection).

### TC-02: `rejects_non_matching_peer_credential`

**Given** a daemon started with `Config::expected_uid = Some(0)` (an
unreachable UID for a non-root test process; smoke-test scaffolding
guarantees the test runs as a non-root user — Cargo's CI does too).

**When** the smoke-test client connects to the socket and sends a
well-formed `Request::Challenge` frame.

**Then** the client's read of the response returns EOF (`0` bytes)
within the test's 1-second budget — the daemon dropped the
connection without writing a response. The daemon is still accepting
new connections (a second connection from the same client, against
the same mismatched `expected_uid`, is also EOF'd; the daemon did
not crash).

### TC-03: `concurrent_accept_cap_enforced`

**Given** a daemon started with default `Config::expected_uid` (the
process's own UID, so the ACL passes) and the SPEC's accept cap of 4.

**When** the smoke-test client opens 4 connections, sends a
`Challenge` frame on each, and reads the stub responses to confirm
all 4 are in-flight. Then opens a 5th connection and writes a
`Challenge` frame, then tries to read a response with a 200 ms
deadline.

**Then** the 5th connection's read times out (the daemon has not
written a response — the 5th is queued behind the semaphore). When
the test drops one of the first 4 connections (releasing its
permit), the 5th's next read returns the typed stub response within
the test's 1-second budget.

## Implementation

Files created:
- `crates/syauth-presenced/src/rpc.rs` — typed `Request` / `Response`
  enums with `#[serde(tag = "kind")]`, ciborium encode/decode helpers
  `encode_frame` / `decode_frame` with a 4-byte big-endian length
  prefix and a named `MAX_FRAME_LEN = 64 * 1024` cap, async
  `read_frame` / `write_frame` helpers over any `AsyncRead` /
  `AsyncWrite`, plus per-variant round-trip unit tests in
  `mod tests`.
- `crates/syauth-presenced/src/server.rs` — `serve()` async surface
  that owns a `tokio::net::UnixListener`, applies a named
  `LISTEN_MODE = 0o600` to the bound socket, enforces
  `SO_PEERCRED` matches the configured expected UID on every accept,
  acquires a permit from the `Semaphore(CONCURRENT_ACCEPT_CAP = 4)`
  before spawning the per-connection task, dispatches
  `Request::Challenge` to a stub responder that emits
  `Response::Challenge { ok=false, reason="not-implemented" }`, and
  on shutdown unlinks the socket file via an RAII `SocketGuard`.
- `crates/syauth-presenced/tests/socket_smoke.rs` — TC-01, TC-02,
  TC-03 driving the in-process daemon via `syauth_presenced::serve`
  on a tempdir-rooted socket path.

Files modified:
- `crates/syauth-presenced/Cargo.toml` — adds `ciborium`, `serde`,
  `serde_bytes`, and the `socket` feature for `nix`. `tokio` gains
  the `net` and `io-util` features (for `UnixListener` and the
  async read/write helpers).
- `crates/syauth-presenced/src/lib.rs` — re-exports the `rpc` and
  `server` modules' public types.
- `crates/syauth-presenced/src/runtime.rs` — `Config` gains
  `expected_uid: Option<u32>` and `run()` calls `server::serve` on
  the existing tokio runtime; the empty heartbeat loop from S-001
  becomes a `select!` between the signal stream and the accept
  loop.
- `specs/unlock-proximity/ROADMAP.md` — ticks every S-002 DoD bullet
  and appends a `Traceability` line per the orchestrator's contract.

## Traceability

- Roadmap item: `specs/unlock-proximity/ROADMAP.md` Step S-002.
- Implementation files: see "Implementation" section above.
- Test files: `crates/syauth-presenced/tests/socket_smoke.rs` plus
  the `#[cfg(test)] mod tests` inside `crates/syauth-presenced/src/rpc.rs`.
