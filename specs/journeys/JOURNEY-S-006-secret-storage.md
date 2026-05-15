# JOURNEY-S-006: Linux secret storage — kernel keyring with libsecret fallback

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md §S-006](../syauth/ROADMAP.md)
- Spec sections: SPEC §D6 (bond-key storage), §4.4 (durability), §6 (T-007
  root-key extraction residual).
- Feature: a portable `KeyStore` abstraction over the Linux kernel keyring
  (primary) with a libsecret/secret-service DBus fallback, both returning
  every secret wrapped in `zeroize::Zeroizing<Vec<u8>>` so it is wiped on
  drop.

## 1. Journey

When **I am the host-side `syauth` code (the `pam_sm_authenticate` entry
point in S-008/S-009, the CLI `pair` flow in S-011, the bond layer in
S-005) about to read the host's Ed25519 private key, or a per-peer bond
symmetric key, out of secure storage**, I want **one synchronous trait
`KeyStore` with `put` / `get` / `remove` and a single `detect()` factory
that probes the kernel keyring first, falls back to libsecret if the
kernel keyring is unreachable (e.g. container without `CONFIG_KEYS`), and
returns `SecretError::NotImplemented` if neither backend works** — and I
want every retrieved byte sequence to live inside `Zeroizing<Vec<u8>>`
**so I can sign one PAM challenge and have the key material wiped from
RAM before the PAM call returns**, without polluting the protocol layer
with DBus or syscall details and without forcing the PAM hot path to
spin up its own runtime decisions.

## 2. CJM

The downstream user is host-side syauth code (PAM module, CLI, pairing
flow) that needs read-on-demand access to a small set of long-lived
secrets keyed by a bond id (32-char lowercase hex from
`peer_id_from_pubkey`, plus `host` for the host's own private key). The
caller does not care which kernel facility actually persisted the bytes;
it cares only that:

- the bytes round-trip exactly,
- the secret is wiped from RAM as soon as the value goes out of scope,
- the call is synchronous (PAM is sync C, runtime-per-call is acceptable
  for infrequent calls but a runtime decision must not bleed into the
  trait surface),
- the backend was already selected at process start, not re-probed every
  call.

### Phase 1: Choose a backend at startup

**User Intent:** Pick whichever Linux secret store actually works on
this box, log the choice once, and move on.

**Actions:** Call `secrets::detect()` (or
`secrets::detect_with_logger(|line| tracing::info!(line))` to plumb the
log into the tracing subscriber the host already configured).

**Pain / Risk:**
- Container without `CONFIG_KEYS` compiled in: the kernel keyring
  syscalls return `ENOSYS`/`EOPNOTSUPP`. Mitigated by probing with a
  cheap operation (`KeyRing::from_special_id(Session, false)` then
  `search` for a never-existed key, accepting `KeyDoesNotExist` as
  "kernel keyring is reachable").
- Headless server without DBus session bus: secret-service `connect`
  errors. Mitigated by probing `SecretService::connect(Plain)`; on
  success the backend is selectable.
- Both backends fail. Mitigated by returning
  `SecretError::NotImplemented`; the caller decides whether to abort
  pairing or run a degraded PAM stack (today PAM denies the auth — fail
  closed).
- Silent fallback to `InMemoryKeyStore` in production. Banned:
  `detect()` never returns the in-memory store; that exists only as a
  test seam.

**Success Signal:** `detect()` returns `Ok(Box<dyn KeyStore>)` and emits
exactly one log line of the form `"syauth: using kernel keyring
backend"` or `"syauth: using libsecret (secret-service) backend"`.

### Phase 2: Put a secret at pairing time

**User Intent:** Persist a newly-derived per-peer symmetric key so the
PAM hot path can read it on every unlock.

**Actions:** `store.put(&peer_id, &secret_bytes)`.

**Pain / Risk:**
- `secret_bytes` is dropped without zeroization. Mitigated by the
  caller: `put` takes `&[u8]`; the caller is expected to hold the
  source value in a `Zeroizing<Vec<u8>>` and pass `&buf[..]`. We do not
  zero-out the caller's buffer for them — the trait does not own the
  source.
- Double-put for the same id silently appends a second record on
  libsecret. Mitigated by `replace = true` on `create_item`.
- libsecret items leak the secret bytes via DBus property reads from a
  malicious bus listener. The encrypted session
  (`EncryptionType::Dh`, the crate's default secure path) transports
  the bytes encrypted on the wire; nothing we can do at this layer
  defeats a root-on-the-session-bus attacker, and SPEC §6 lists this as
  accepted residual.

**Success Signal:** `get(&peer_id)` after `put` returns
`Ok(Some(Zeroizing<Vec<u8>>))` whose contents byte-equal the original.

### Phase 3: Get on the PAM hot path

**User Intent:** Read the bond key once per `pam_sm_authenticate`, use
it for MAC verification, drop it, return from PAM.

**Actions:** `let secret = store.get(&peer_id)?;` then use
`&*secret` against `compute_tag` / `verify_tag`, then let the binding
go out of scope.

**Pain / Risk:**
- Returning `Vec<u8>` instead of `Zeroizing<Vec<u8>>` — the bytes
  outlive the function. Mitigated by the trait signature.
- `clone()` of the `Zeroizing<Vec<u8>>` keeps a second copy alive.
  Mitigated by documenting "never clone" in the trait rustdoc and not
  cloning ourselves.
- Reading a secret that does not exist (unbonded peer). Mitigated by
  returning `Ok(None)`, never an error.

**Success Signal:** A unit test that calls `get(unknown)` asserts
`Ok(None)`; a roundtrip test asserts the returned `Zeroizing<Vec<u8>>`
contains the same bytes that were `put`.

### Phase 4: Remove on revoke

**User Intent:** When `syauth revoke <peer>` runs, the bond key is
deleted from secure storage in the same transaction as the
`mark_revoked` write to `bonds.toml`.

**Actions:** `store.remove(&peer_id)`.

**Pain / Risk:**
- Remove of a non-existent id should not error — `revoke` is
  idempotent per S-012 DoD. Mitigated by mapping
  `KeyError::KeyDoesNotExist` / `Error::NoResult` to `Ok(())`.
- libsecret item is locked by the user's session keychain prompt.
  Out of scope for v0.1 (we use the default collection which is
  unlocked alongside the user session); documented as a known
  limitation.

**Success Signal:** `get(&peer_id)` after `remove` returns
`Ok(None)`; a follow-up `remove(&peer_id)` is `Ok(())` (idempotent).

### Phase 5: Async-vs-sync at the PAM boundary

The PAM C ABI is synchronous. Making `KeyStore` async would force every
call site to either be inside a tokio runtime block or to enter one
ad-hoc, which on the PAM hot path means `Runtime::new()` per call. The
kernel keyring backend is naturally synchronous (raw `keyctl(2)` calls
return immediately). The libsecret backend wraps DBus, which is
inherently async; the upstream `secret-service` crate exposes a
`blocking` submodule built on `zbus::blocking` that handles this for
us. We pick the blocking API for the trait impl. **The cost** of the
blocking API is one short-lived DBus connection per call — fine for an
infrequent operation (pairing, revoke) and acceptable for the once-per
PAM-call read on the unlock hot path. The kernel keyring is used in
production and is microseconds-fast; libsecret is the rare fallback.

### Phase 6: Zeroization contract

`zeroize::Zeroizing<Vec<u8>>` wraps a `Vec<u8>` and, on drop, calls
`zeroize()` to write zeros over the buffer. The contract is:

- Every return path that holds raw secret bytes wraps them in
  `Zeroizing::new(Vec<u8>)` before they cross a function boundary.
- We never `clone()` a `Zeroizing<Vec<u8>>` ourselves.
- We never log a secret byte — `SecretError::Backend(msg)` carries the
  upstream library's `to_string()` only, which by inspection of
  `linux-keyutils::KeyError` and `secret_service::Error` never includes
  the secret payload.
- Drop-wiping is best-effort: Rust may have moved the bytes elsewhere
  in the lifetime of the `Vec`. The `zeroize` crate's `Zeroizing`
  guarantee is "the *final* buffer that `Vec::drop` sees is zeroed";
  earlier copies created by reallocation, function-arg moves, or
  compiler tail-call elision are out of scope. This is the strongest
  guarantee a non-`unsafe` library can offer in Rust today and matches
  the SPEC §6 T-007 acceptance.

### Phase 7: Testing seam — `InMemoryKeyStore`

**User Intent:** Test upstream code (PAM module, CLI) without touching
the kernel keyring or DBus.

**Actions:** Construct `InMemoryKeyStore::new()` in a unit test and
pass it as the `Box<dyn KeyStore>` the system under test expects.

**Pain / Risk:**
- `InMemoryKeyStore` leaks into production through `detect()`. Banned
  by DoD; `detect()` returns `NotImplemented` when no real backend is
  available — production builds either find a real backend or refuse
  to run.
- `InMemoryKeyStore` is not zeroize-aware. Mitigated: the inner
  `HashMap` stores `Zeroizing<Vec<u8>>`, so dropping the store wipes
  every value.

**Success Signal:** All unit tests in `secrets.rs::tests` pass against
`InMemoryKeyStore` and exercise put / get / get-missing /
double-put-overwrites / remove.

### Phase 8: Hermetic integration test against the real kernel keyring

**User Intent:** Pin the "kernel keyring backend actually works against
the Linux kernel" invariant without writing to the system keyring or
the user-persistent keyring.

**Actions:** `tests/keyring_linux.rs`, `#[cfg(target_os = "linux")]`,
uses `KeyRingIdentifier::Session` (process-local), each id is prefixed
with `syauth-test-{nanos}-{n}:` so two concurrent test runs cannot
collide. An RAII guard drop-cleans the key even on test panic.

**Pain / Risk:**
- Test leaves keys behind in the session keyring. Mitigated by RAII
  cleanup (`Drop` calls `remove` ignoring errors).
- Test runs in a container without `CONFIG_KEYS`. Mitigated: probe at
  the top of the test; if `KeyStore::detect` returns
  `NotImplemented` *and* no kernel backend was selectable, the test
  prints a skip line and returns `Ok(())` cleanly. We do NOT mark the
  test `#[ignore]` because the file is gated on `target_os = "linux"`;
  the skip is dynamic.
- Test writes to `KeyRingIdentifier::User` (`@u`). Banned: the impl
  always targets `Session`, and the test asserts that the produced
  key cannot be seen from a freshly-acquired session keyring of a
  different identifier (smoke).

**Success Signal:** Running the test on a Linux box with
`/proc/keys` accessible exercises put → get → remove against the
session keyring and reports zero leftover keys; running it in a
container without the keyring facility prints a skip line and exits
0.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Two backends, two error types | 1, 2-4 | Single `SecretError` enum normalizes both upstream errors via their `Display` strings |
| Async secret-service vs sync trait | 5 | Use upstream `secret_service::blocking` module; document the runtime cost |
| Secret bytes might outlive intent | 6 | `Zeroizing<Vec<u8>>` on every return path; no clones in our code |
| Production silently runs without persistence | 1 | `detect()` returns `NotImplemented` instead of falling through to in-memory |
| Test pollutes shared keyring | 8 | Process-local Session keyring, unique nanos-prefixed ids, RAII cleanup, skip-on-container |
| Future caller writes new code that bypasses the trait | all | The trait is the only public way to reach `linux-keyutils` or `secret-service` from this crate — the modules are private impls behind `pub mod secrets` |

### North Star Summary

A first-time syauth installer pairs a phone, the bond key is written
into the kernel keyring (one log line confirms which backend),
subsequent PAM unlocks read the key in microseconds, the key is
zeroized as the PAM call returns, and a `syauth revoke` removes the
key idempotently — all without the PAM module knowing whether the
underlying store is `keyctl(2)` or DBus.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] `detect()` is a single call, returns at most after one syscall +
      one DBus probe attempt.
- [x] `InMemoryKeyStore::new()` is parameterless.

### Onboarding Clarity
- [x] Three trait methods (`put` / `get` / `remove`) plus one
      factory function (`detect`).
- [x] `SecretError` has exactly two variants (`Backend` / `NotImplemented`)
      both with clear meaning.

### Production-Ready Defaults
- [x] `KernelKeyring` targets `KeyRingIdentifier::Session` per SPEC
      D6 — never `User` (which is shared across processes).
- [x] `SecretService` uses `EncryptionType::Dh` for transport
      encryption.
- [x] All returned secrets are `Zeroizing<Vec<u8>>`.

### Golden Path Quality
- [x] One `KeyStore` trait drives all three call sites (pair, unlock,
      revoke).
- [x] Roundtrip test against the real kernel keyring under
      `tests/keyring_linux.rs` is the truth-in-the-source.

### Decision Load
- [x] Caller never names a backend — they call `detect()` and the
      first working one wins.

### Progressive Complexity
- [x] `InMemoryKeyStore` for unit tests.
- [x] `KernelKeyring` for production on Linux with `CONFIG_KEYS`.
- [x] `SecretService` for everything else.

### Error Quality
- [x] `SecretError::Backend(String)` wraps the upstream error's
      `to_string()`; never contains secret bytes.
- [x] `SecretError::NotImplemented` is a distinct variant so
      callers can pattern-match on it.

### Failure Safety
- [x] `remove` on a missing id is `Ok(())` (idempotent).
- [x] `get` on a missing id is `Ok(None)` (never an error).
- [x] `detect()` never silently falls through to in-memory.

### Runtime Transparency
- [x] One log line at `detect()` time; the backend identity is
      observable.

### Debuggability
- [x] `BackendKind` enum (Kernel / SecretService / InMemory) is
      a separate public type returned by `detect_with_logger`'s
      logger context, observable in tests via the `&str` log line.

### Cross-Surface Consistency
- [x] Same trait used by PAM (S-008), CLI (S-011/S-012), pairing
      (S-011).

### Workflow Consistency
- [x] Constants `KEYRING_ID_PREFIX = "syauth:"`,
      `SECRET_SERVICE_COLLECTION = "syauth"`,
      `SECRET_SERVICE_ATTR_KIND = "kind"`,
      `SECRET_SERVICE_ATTR_KIND_VALUE = "syauth-bond"`,
      `SECRET_SERVICE_ATTR_ID = "id"`,
      `SECRET_SERVICE_CONTENT_TYPE = "application/octet-stream"`,
      `LOG_LINE_KERNEL`, `LOG_LINE_SECRET_SERVICE` are named at
      module scope per AGENTS.md TDD rules.

### Change Safety
- [x] Adding a new backend means adding one `impl KeyStore` and
      one branch in `detect`; the trait surface does not move.

### Experimentation Safety
- [x] Tests use `InMemoryKeyStore` or unique per-process keyring
      ids; never the system/user keyring.

### Interaction Latency
- [x] Kernel keyring: microseconds.
- [x] Libsecret: one DBus call per operation; acceptable for the
      pairing/revoke flow and the once-per-PAM unlock read.

### Developer Feedback Speed
- [x] `SecretError` impls `Display` + `Debug` via `thiserror`.

### Team Scale
- [x] Pure-Rust crate, no platform forks; non-Linux build cleanly
      excludes `KernelKeyring` via `#[cfg(target_os = "linux")]`.

### System Scale
- [x] No per-process global state; `Box<dyn KeyStore>` is the
      handle.

### Right Behavior by Default
- [x] First call to `detect()` returns the kernel keyring backend
      on any Linux box with `CONFIG_KEYS`.

### Anti-Bypass Design
- [x] `KernelKeyring` and `SecretService` are public types, but the
      only canonical constructor is `detect()` — direct construction
      is allowed for advanced callers (and is the only thing that
      makes the integration test possible without re-running probe
      logic).

## 4. Tests

### TC-01: inmemory_roundtrip

**Given** a fresh `InMemoryKeyStore`.
**When** `put("test-id", b"secret-bytes")` is called, then
`get("test-id")`.
**Then** the result is `Ok(Some(z))` where `z: Zeroizing<Vec<u8>>`
and `&*z == b"secret-bytes"`.

### TC-02: inmemory_get_missing_returns_none

**Given** a fresh `InMemoryKeyStore`.
**When** `get("never-put")` is called.
**Then** the result is `Ok(None)`.

### TC-03: inmemory_double_put_overwrites

**Given** a store with `put("id", b"first")` already invoked.
**When** `put("id", b"second")` is invoked, then `get("id")`.
**Then** the returned bytes equal `b"second"`.

### TC-04: inmemory_remove_makes_get_return_none

**Given** a store with `put("id", b"x")` already invoked.
**When** `remove("id")` is invoked, then `get("id")`.
**Then** the result is `Ok(None)`.

### TC-05: inmemory_remove_missing_is_ok

**Given** a fresh `InMemoryKeyStore`.
**When** `remove("never-put")` is invoked.
**Then** the result is `Ok(())`.

### TC-06: zeroizing_smoke_field_type

**Given** the public API surface.
**When** the compiler checks the `KeyStore::get` signature.
**Then** the return type is
`Result<Option<Zeroizing<Vec<u8>>>, SecretError>` (compile-time
contract; assertion-by-construction).

### TC-07: detect_with_logger_returns_inmemory_fallback_is_rejected

**Given** that we cannot fake a kernel keyring at unit-test time
(it's a syscall) and we likewise cannot guarantee absence of DBus
at unit-test time.
**When** the unit test only verifies that the *signature* of
`detect_with_logger` accepts a `Fn(&str)` closure.
**Then** a minimal smoke that calls `detect_with_logger(|_| {})`
and asserts the result is *either* `Ok(_)` or
`Err(SecretError::NotImplemented)` — never a panic. The real
integration test exercises the Kernel path in
`tests/keyring_linux.rs`.

### TC-08: kernel_keyring_roundtrip (integration, Linux only)

**Given** `KernelKeyring::probe()` reports success.
**When** `put("syauth-test-{nanos}-roundtrip", b"data")` →
`get(...)` → `remove(...)` against the Session keyring.
**Then** the put-bytes equal the get-bytes, and the post-remove
`get` returns `Ok(None)`. A RAII guard ensures cleanup on panic.

### TC-09: kernel_keyring_get_missing_returns_none (integration)

**Given** `KernelKeyring::probe()` reports success.
**When** `get("syauth-test-{nanos}-never-put")` is called.
**Then** the result is `Ok(None)`.

### TC-10: backend_error_messages_never_contain_secret_bytes
(audit-by-inspection, not a unit test)

We assert by inspection of `linux-keyutils::KeyError` and
`secret_service::Error` that neither type's `Display` impl prints
the secret payload bytes; only metadata (errno, item path, DBus
error name) is included. This is documented at the top of
`secrets.rs` so a future reviewer can re-audit if either crate
changes.

## Traceability
- Roadmap item: [specs/syauth/ROADMAP.md §S-006](../syauth/ROADMAP.md)
- Implementation files:
  - `crates/syauth-core/src/secrets.rs` (new module)
  - `crates/syauth-core/src/lib.rs` (add `pub mod secrets;` and
    re-exports)
  - `crates/syauth-core/Cargo.toml` (deps: `zeroize`,
    `linux-keyutils`, `secret-service`)
- Test files:
  - In-module `#[cfg(test)] mod tests` in
    `crates/syauth-core/src/secrets.rs`
  - `crates/syauth-core/tests/keyring_linux.rs`
- Spec links: SPEC §D6 (storage), SPEC §4.4 (durability matrix), SPEC
  §6 T-007 (root-key extraction residual).
