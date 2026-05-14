# JOURNEY-S-005: Bond store — TOML schema + atomic write

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md §S-005](../syauth/ROADMAP.md)
- Feature: persistent bond record at `/var/lib/syauth/bonds.toml` with an
  atomic write path, restrictive POSIX permissions, and a typed
  schema-version error for forward-compat.

## 1. Journey

When **I am the host-side `syauth` process (PAM module in S-008/S-009, the
CLI `pair`/`list`/`revoke` subcommands in S-011/S-012, and the kernel-keyring
abstraction in S-006) about to record or read which phone is allowed to
unlock this machine**, I want **a single audited `BondStore` type that
loads from a TOML file, lets me add / mark-revoked / list bonds, and writes
back atomically (via `tempfile::NamedTempFile::persist`) with strict
`0o700` directory + `0o600` file permissions and a future-proof
`schema_version` field that errors out rather than panicking on an
unrecognized version**, so I can **persist bonded phones across reboots
without ever risking a torn write, a permission downgrade, a UUID-style
non-stable identifier, or a forward-compat crash when a future syauth
build reads a v2 bonds file with a v1 binary**.

## 2. CJM

The downstream user is the host-side syauth code that wants to remember a
phone across reboots. Today they have nothing — pairing in S-011 has nowhere
to write its result; PAM in S-008/S-009 has no way to know which phones are
allowed. This journey gives them a `BondStore` with `load(path)`,
`add(bond)`, `remove(peer_id)`, `mark_revoked(peer_id, reason)`,
`list() -> &[Bond]`, `save()`; a typed `BondError` enum; a stable
BLAKE3-derived `peer_id`; and a documented atomic-write strategy.

### Phase 1: Construct a Bond

**User Intent:** Build a `Bond` value from a peer's Ed25519 public key, a
human-readable name, and the current timestamp.

**Actions:** Call `Bond::peer_id_from_pubkey(pubkey)` to compute the stable
16-byte BLAKE3-truncated identifier as 32-char lowercase hex; then
construct `Bond { peer_id, pubkey, name, created_at, status: Bonded }`.

**Pain / Risk:**
- Using a UUID instead of a hash. Banned by DoD: `peer_id` must derive
  deterministically from the pubkey so the same phone always gets the same
  id across reboots and reinstalls. Mitigated by exposing only the
  free-function `peer_id_from_pubkey` as the canonical constructor.
- Truncating BLAKE3 in different ways at different call sites. Mitigated
  by a single `PEER_ID_BLAKE3_BYTES: usize = 16` constant and one helper.
- Caller passes a malformed pubkey. The type is `[u8; 32]`, so the
  compiler enforces length at construction time.

**Success Signal:** `peer_id` is exactly 32 lowercase hex chars
(`2 * PEER_ID_BLAKE3_BYTES`) and is byte-identical across two calls with
the same pubkey.

### Phase 2: Load an existing bonds.toml

**User Intent:** Read the TOML file on disk into an in-memory `BondStore`.

**Actions:** Call `BondStore::load(path)`.

**Pain / Risk:**
- File does not exist. `load` returns an empty store rather than erroring,
  so PAM can run on a freshly-installed machine.
- File is corrupted / not valid TOML. Returns `BondError::Parse(...)`
  with the underlying `toml::de::Error` as the source.
- `schema_version` is a future version we don't understand. Returns
  `BondError::UnsupportedSchemaVersion { found, supported_up_to }` —
  NOT a panic, NOT a silent acceptance.
- Missing `schema_version` field. Treated as `BondError::Parse` (no
  default fallback — explicit is safer than guessing).

**Success Signal:** `store.list()` returns the bonds in the file in
declaration order; `len()` matches.

### Phase 3: Add / Revoke / Remove

**User Intent:** Mutate the in-memory store: add a new bond from pairing,
mark a bond revoked from `syauth revoke`, or remove a stale bond.

**Actions:** `store.add(bond)`, `store.mark_revoked(peer_id, reason)`,
`store.remove(peer_id)`.

**Pain / Risk:**
- Adding a peer that is already bonded. Returns
  `BondError::AlreadyBonded { peer_id }` rather than silently
  overwriting — overwrite would let an attacker who can run `syauth pair`
  silently re-bond a different key under the same id.
- Marking an unknown peer revoked. Returns
  `BondError::UnknownPeer { peer_id }`.
- Marking an already-revoked peer revoked. No-op, returns `Ok(())`. This
  keeps `syauth revoke` idempotent per S-012 DoD.

**Success Signal:** `store.list()` reflects the requested mutation; no
disk state changes until `save()` is called.

### Phase 4: Atomic save

**User Intent:** Persist the in-memory state to disk such that a crash
mid-write cannot corrupt the on-disk bond record.

**Actions:** Call `store.save()`.

**Pain / Risk:**
- Crash between `write_all` and `rename`. Mitigated by writing to a
  `tempfile::NamedTempFile` in the same parent directory (so `persist` is
  a same-filesystem atomic rename) and only renaming after all bytes are
  flushed.
- Parent directory does not exist. Mitigated by `fs::create_dir_all`
  + `fs::set_permissions(BOND_DIR_MODE = 0o700)`.
- Parent directory exists but is world- or group-readable. Returns
  `BondError::ParentDirTooPermissive { mode }` — we will NOT silently
  re-chmod a directory the operator created, because that masks a
  misconfiguration.
- Temp file lingers on a crashed write. `tempfile::NamedTempFile` unlinks
  on drop, so a crashed `save()` leaves no temp file behind.
- Race between two `save()` calls. Out of scope for v0.1; documented as
  "BondStore is not safe for concurrent use across processes" — the only
  writers are the singleton CLI and pairing flow.

**Atomic-write fault-injection strategy (chosen for the fault test):**
We do NOT mock `persist`. Instead, the test (a) writes a known-good bonds
file with `BondStore::save`, (b) constructs a new store with a different
bond, (c) calls a test-only `save_with_fault` helper that performs the
`write_all` against a `NamedTempFile` and then drops the temp file
**without calling `persist`**, simulating a crash between `write` and
`rename`, and (d) reads the on-disk file back and asserts byte-equality
with the snapshot from (a). This is the deterministic-by-construction
form of "panic-between-write-and-persist"; a `catch_unwind` variant adds
complexity without strengthening the test.

**Success Signal:** Destination file is byte-equal to its
pre-`save_with_fault` state; no leftover temp files in the parent dir.

### Phase 5: Permissions

**User Intent:** Ensure the bond file and its parent directory are
readable only by root (or the calling user, in test contexts).

**Actions:** `save()` sets `BOND_FILE_MODE = 0o600` on the temp file
*before* `persist`, and `BOND_DIR_MODE = 0o700` on the parent directory.

**Pain / Risk:**
- Setting permissions *after* `persist`. Race window where another reader
  can `open()` the file with the default `umask`-derived mode. Mitigated
  by setting the mode on the temp file before rename.
- `umask` discrepancy across distros. `NamedTempFile` creates with mode
  `0o600` by default on Unix, but we set it explicitly to make the
  intent visible to readers and to defend against future tempfile-crate
  changes.

**Success Signal:** Integration test asserts `metadata.permissions().mode()
& 0o777 == 0o600` for the file and `0o700` for the directory.

### Phase 6: Forward-compat schema_version

**User Intent:** Make sure a v0.2 syauth that writes a v2 schema does not
crash a v0.1 reader.

**Actions:** The on-disk schema has `schema_version: u32` at top level;
`load` rejects any value greater than `BOND_SCHEMA_VERSION_LATEST = 1`
with `BondError::UnsupportedSchemaVersion`.

**Pain / Risk:**
- Panicking on a future version. Banned by DoD; we return the typed
  error.
- Accepting an older version. Out of scope for v0.1 (we have no v0).
  When v2 exists we will widen `supported_up_to` and add per-version
  parsers.

**Success Signal:** A unit test that writes `schema_version = 2` and
calls `load` asserts the returned error variant.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Bond identifier semantics could drift across the codebase | 1 | One free function `peer_id_from_pubkey` is the only constructor |
| Crash mid-write corrupts bonds.toml | 4 | `tempfile::NamedTempFile::persist` plus a fault-injection test pinning the invariant |
| Permission downgrade by `umask` accident | 5 | Set mode explicitly before `persist`, assert in an integration test |
| Future schema version crashes today's reader | 6 | Typed `UnsupportedSchemaVersion` error, no panic |

### North Star Summary

A first-time syauth installer sees `/var/lib/syauth/bonds.toml` appear
with mode `0o600` after pairing, knows their phone is recorded by a
stable BLAKE3-derived id, can survive a `kill -9` of `syauth pair` with
zero file corruption, and can roll forward to a future schema version
without the v0.1 reader panicking.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] First `BondStore::load` on an empty machine returns `Ok` with no
      bonds in well under 1 ms.
- [x] First `add` + `save` is one call each; no builders, no async.

### Onboarding Clarity
- [x] `BondError` variants name the offending field or `peer_id` and
      include source errors via `#[source]`.
- [x] Public API is six methods (`load`, `save`, `add`, `remove`,
      `mark_revoked`, `list`) plus the free function
      `peer_id_from_pubkey`.

### Production-Ready Defaults
- [x] File mode `0o600`, directory mode `0o700`, on every save.
- [x] Atomic rename via `tempfile::NamedTempFile::persist`, no
      knobs.

### Golden Path Quality
- [x] Two-bond roundtrip test: write → read → equality.
- [x] Revoke is persisted across save+load.

### Decision Load
- [x] No configuration besides the path passed to `load`/`save`.

### Progressive Complexity
- [x] `peer_id_from_pubkey` lives on `Bond` and is callable without
      constructing a `BondStore`.

### Error Quality
- [x] `BondError::UnsupportedSchemaVersion { found, supported_up_to }`
      names both numbers.
- [x] `BondError::ParentDirTooPermissive { mode }` names the mode that
      was observed.
- [x] `BondError::AlreadyBonded { peer_id }` and
      `BondError::UnknownPeer { peer_id }` name the id.

### Failure Safety
- [x] Atomic-write fault test pins the "no corruption on crash"
      invariant.
- [x] `NamedTempFile` unlinks on drop, so a crashed save leaves no
      orphan temp file.

### Runtime Transparency
- [x] No background threads, no global state — every call is
      observable from the caller.

### Debuggability
- [x] On-disk format is human-readable TOML; an operator can `cat
      /var/lib/syauth/bonds.toml` to inspect state.

### Cross-Surface Consistency
- [x] Identical `Bond` type used by PAM (S-008/S-009), CLI
      (S-011/S-012), and mobile (S-014, via UniFFI).

### Workflow Consistency
- [x] Constants `BOND_FILE_MODE = 0o600`, `BOND_DIR_MODE = 0o700`,
      `PEER_ID_BLAKE3_BYTES = 16`,
      `BOND_SCHEMA_VERSION_LATEST = 1` are named at module scope per
      AGENTS.md TDD rules.

### Change Safety
- [x] `save` is atomic; partial writes are impossible by construction.

### Experimentation Safety
- [x] Tests use `tempfile::TempDir` exclusively; never touch
      `/var/lib/syauth`.

### Interaction Latency
- [x] Synchronous in-memory mutations; only `save`/`load` touch disk.

### Developer Feedback Speed
- [x] `BondError` impls `Display` and `Debug` with sourced underlying
      causes.

### Team Scale
- [x] Pure-Rust crate, no platform forks; permission test is gated on
      `#[cfg(unix)]`.

### System Scale
- [x] BLAKE3 hash is constant-time per pubkey; `list` is a slice
      reference, no allocation.

### Right Behavior by Default
- [x] `load` of a missing file is `Ok(empty)`, not an error.

### Anti-Bypass Design
- [x] The only constructor of `peer_id` is the BLAKE3 helper — no
      `Bond::with_arbitrary_id` API exists.
- [x] `add` rejects duplicates; the only way to re-bond is `remove`
      then `add`.

## 4. Tests

### TC-01: peer_id is stable and BLAKE3-derived

**Given** an Ed25519 pubkey `[0x42; 32]`.
**When** `Bond::peer_id_from_pubkey` is called twice.
**Then** both calls return the same 32-char lowercase hex string equal
to the first 16 bytes of `blake3::hash(pubkey)`.

### TC-02: empty file load returns empty store

**Given** a path that does not exist.
**When** `BondStore::load(path)` is called.
**Then** the result is `Ok(store)` and `store.list().is_empty()`.

### TC-03: add → save → load roundtrip

**Given** an empty store in a temp dir.
**When** two distinct `Bond`s are added, `save` is called, and a new
store is loaded from the same path.
**Then** the loaded store's `list()` equals the original two bonds in
declaration order.

### TC-04: add rejects duplicate peer_id

**Given** a store containing a bond for pubkey `[0x01; 32]`.
**When** `add` is called with a different `Bond` whose pubkey is also
`[0x01; 32]`.
**Then** the result is `Err(BondError::AlreadyBonded { peer_id })`
with the matching id, and `list().len() == 1`.

### TC-05: revoke is persisted

**Given** a saved store containing one `Bonded` bond.
**When** `mark_revoked(peer_id, "phone-lost")` is called, then `save`,
then re-load.
**Then** the loaded bond has status
`Revoked { reason: "phone-lost".into() }`.

### TC-06: revoke unknown peer errors

**Given** a store with no bonds.
**When** `mark_revoked("deadbeef..", "x")` is called.
**Then** the result is `Err(BondError::UnknownPeer { peer_id })`.

### TC-07: revoke-of-already-revoked is a no-op

**Given** a store containing a `Revoked` bond.
**When** `mark_revoked(peer_id, "new-reason")` is called.
**Then** the result is `Ok(())` and the existing reason is preserved
(no overwrite).

### TC-08: atomic-write fault leaves original file intact

**Given** a saved store at `path` with one bond.
**When** a second store is constructed with a different bond and
`save_with_fault(path)` (test-only helper that drops the temp file
without `persist`) is invoked.
**Then** `fs::read(path)` is byte-equal to the snapshot captured
before the fault, and no `.tmp*` files remain in the parent dir.

### TC-09: file mode is 0o600 after save

**Given** any non-empty store saved to a fresh `TempDir`.
**When** the metadata of the saved file is read.
**Then** `metadata.permissions().mode() & 0o777 == 0o600`.

### TC-10: parent directory mode is 0o700 after save

**Given** a non-existent parent directory under a `TempDir`.
**When** `save` is called.
**Then** the directory is created and
`metadata.permissions().mode() & 0o777 == 0o700`.

### TC-11: future schema_version is rejected with a typed error

**Given** a TOML file containing `schema_version = 2` and no `[[bond]]`
entries.
**When** `BondStore::load` is called.
**Then** the result is
`Err(BondError::UnsupportedSchemaVersion { found: 2, supported_up_to: 1 })`
— never a panic.

## Traceability
- Roadmap item: [specs/syauth/ROADMAP.md §S-005](../syauth/ROADMAP.md)
- Implementation files: `crates/syauth-core/src/bond.rs`,
  `crates/syauth-core/src/lib.rs` (re-exports),
  `crates/syauth-core/Cargo.toml` (deps).
- Test files: in-module `#[cfg(test)] mod tests` block in
  `crates/syauth-core/src/bond.rs`.
