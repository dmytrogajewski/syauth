# JOURNEY-S-011: `syauth-cli` — `pair` subcommand with LE Secure Connections + app-level OOB

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md §S-011](../syauth/ROADMAP.md)
- Feature: `syauth pair` and `syauth list` subcommands. `syauth pair` drives the
  desktop side of pairing per SPEC §4.1 dataflow — initiates LE Secure
  Connections via `bluer` with MitM-protection required, then displays an
  app-level 4-word OOB code derived from `HKDF(bond, "syauth-oob-v1")[0..4]`.
  On a `[y/N]` confirmation the bond is written to disk and `syauth list`
  immediately surfaces the new peer.

## 1. Journey

When **I am the operator running syauth for the first time on a fresh Fedora
desktop with my Pixel 8 in arm's reach**, I want to **run a single CLI command
that scans for my phone, walks me through LE Secure Connections numeric
comparison, shows me a four-word emoji code I can also see on the phone, and
records the bond on `Y` (or refuses cleanly on `N`/timeout/missing-LESC)**, so
I can **trust that "this and only this phone" is now allowed to authorize my
unlocks**.

## 2. CJM

Pairing is a one-time, security-critical operator action. Until S-011 the
desktop side did not exist: `BlueZBtPeer` was wired (S-010) but there was no
CLI entry point that drove it through pairing. S-011 ships the desktop driver.

The key design decisions:

1. **An explicit, in-process pairing state machine** (`PairingPhase`).
   Per the `/bt` SKILL Phase 2 rule, the unlock path never reads from
   `ProvisionalBonded`. Here we encode the same idea for the pair path: the
   state machine is the single source of truth for "what step are we on", and
   the only way to reach `Bonded` is to pass through every preceding gate
   (LESC negotiated, OOB confirmed by the operator). A timeout or rejection
   transitions to `Revoked` and the bond is **never persisted** in that case.
   The DoD spells this out: "On timeout (default 60 s), state machine
   transitions ProvisionalBonded → Revoked. No partial bond is written."

2. **A `PairBackend` trait at the radio seam.** The DoD requires the
   integration test to inject a mock `BtPeer` that emits LESC simulation
   events. Rather than mocking `bluer` directly (the upstream API is large,
   async, and not easily mockable), we define a tiny `PairBackend` trait that
   covers exactly the six verbs the pair flow needs: `adapter_info`,
   `adapter_supports_lesc`, `scan_peers`, `initiate_lesc_with_peer`,
   `display_lesc_numeric`, and `confirm_oob`. Production wraps `bluer`; tests
   use a `MockPairBackend` driven by a scenario table.

3. **A pure `oob_code_for_bond` function.** The DoD's "4-word emoji OOB code
   derived from HKDF(bond, 'syauth-oob-v1')[0..4]" is implemented as a pure
   function over a 256-entry `OOB_WORDS` table. Same `bond_key` → same word
   tuple, byte-deterministic. The 256 entries are short English nouns each
   prefixed with one well-known emoji (no combining marks, no skin-tone
   modifiers); the exact contents are stable and committed.

4. **`--yes` controls only the operator confirmation prompt.** Critically,
   `--yes` does NOT bypass the LESC-capability check or any other
   safety-relevant gate; it just skips the interactive `[y/N]`. The DoD test
   `pair_rejects_when_adapter_lacks_lesc_even_with_yes` enforces this.

5. **`syauth list` is a thin reader of `BondStore::load(bond_dir).list()`.**
   Per the DoD: "syauth list shows the new peer immediately after pairing
   completes." The integration test exercises both subcommands back-to-back
   against the same `--bond-dir` tempdir.

### Phase 1: Scan & pick

**User Intent:** Find my phone in the air and tell syauth to talk to it.

**Actions:**
- Operator runs `syauth pair --bond-dir /var/lib/syauth/` (or accepts the
  default). The CLI prints adapter info: `adapter hci0 ready (LE Secure
  Connections: yes)`.
- The CLI scans for advertising peers for up to ~5 s and shows a numbered
  list. If `--peer <name>` is given, the picker is skipped.
- Operator selects one (`1`-`N`) or, when `--peer` is given, the matching
  peer is auto-selected.

**Pain / Risk:**
- Ambiguous match: `--peer pixel` matches two devices. With `--yes` the CLI
  must fail with a clear error (`PairError::AmbiguousPeer { matches }`),
  not silently pick the first. Without `--yes` the picker remains
  interactive.
- Adapter missing or off: typed `AdapterMissing { name }` from the backend
  flows through to a single-line error.
- LE Secure Connections unsupported on this controller (BLE 4.0 chips):
  refuse with `LescUnsupported { adapter, hint }`. The DoD requires the
  error to name the issue.

**Success Signal:** A numbered list of candidates, or — with `--peer` — a
single "selected `<name>`" line.

### Phase 2: LE Secure Connections + 6-digit numeric comparison

**User Intent:** Let BlueZ do its part of the secure pairing.

**Actions:**
- The backend's `initiate_lesc_with_peer` is called. In production this is
  `bluer::Device::pair()` with `Adapter::set_pairable(true)` and MITM
  protection required. In test mode the mock returns a synthetic 6-digit
  code.
- The CLI prints the 6-digit code in a banner, e.g.:
  `BT numeric code: 482 615   confirm on both devices`.

**Pain / Risk:**
- Operator confirms on the wrong phone. Mitigated by the second app-level
  OOB confirmation in Phase 3.
- Operator races past the prompt. The default 60 s timeout transitions the
  state machine to `Revoked` and no bond is written.
- BlueZ falls back to legacy pairing on an older controller. The
  `adapter_supports_lesc` check catches this before we get here.

**Success Signal:** The LESC negotiation completes; the backend returns
the negotiated 32-byte bond key.

### Phase 3: App-level OOB confirmation

**User Intent:** Defeat the residual "BT pairing was MitM'd" risk by
confirming a code derived from the just-negotiated shared secret.

**Actions:**
- The CLI calls `oob_code_for_bond(&bond_key)` and prints the four words.
- The CLI prompts: `OOB matches your phone? [y/N]`. With `--yes`, the prompt
  is auto-confirmed.

**Pain / Risk:**
- The four words must be readable and unambiguous. The word list is
  committed; one emoji prefix per word, no combining marks, single letters
  and short nouns only.
- A racing attacker who got past LESC must also forge the OOB; defense in
  depth per SPEC §4.1.

**Success Signal:** `Y` advances the state machine; `N` or timeout
transitions to `Revoked` with no bond written.

### Phase 4: Persist & verify

**User Intent:** Have the bond saved so `pam_sm_authenticate` can read it.

**Actions:**
- On `Y`: `BondStore::load(bond_dir)` → `add(bond)` → `save(bond_dir)`.
- The CLI prints `bonded <peer-name> id=<peer_id_hex>; run \`syauth list\`
  to verify` and exits 0.
- Operator runs `syauth list --bond-dir <path>` and sees a single TSV row.

**Pain / Risk:**
- Race between two `syauth pair` invocations: the second one fails on
  `BondError::AlreadyBonded` (S-005 contract, no overwrite). The CLI
  surfaces this verbatim.
- Disk full / permission denied: typed `BondError::Io` flows up.

**Success Signal:** A single TSV line in `syauth list` matching the new
peer.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Adapters without LE Secure Connections silently fall back to legacy pairing in BlueZ | 1 | Explicit `adapter_supports_lesc` check that names the issue and gives a hint |
| Ambiguous `--peer <name>` with `--yes` could silently pair the wrong device | 1 | Typed `AmbiguousPeer { matches }` error; CLI lists the matches and exits non-zero |
| The 6-digit BT code is verified by BlueZ, not us — so a controller-level MitM goes undetected | 3 | App-level 4-word OOB derived from the negotiated bond key |
| Operator runs `pair` twice and clobbers a working bond | 4 | `BondStore::add` refuses duplicates (S-005 contract) |
| `--yes` in CI scripts could disable the LESC-capability check | All | Hard-coded: `--yes` only skips the y/N prompt, never any safety gate; covered by `pair_rejects_when_adapter_lacks_lesc_even_with_yes` |

### North Star Summary

The operator runs `syauth pair`, sees a numbered list of candidates, picks one,
confirms the 6-digit BT code and then the 4-word OOB on both devices, and the
bond is on disk. `syauth list` shows it immediately. Total wall-clock under one
minute by default. If anything goes wrong (LESC missing, timeout, mismatched
codes, operator says `N`), no bond is written and the CLI exits non-zero with a
human-readable message that names the failure mode.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] `syauth pair` finishes in under one minute on the happy path.
- [x] `syauth list` is a thin TSV print — single I/O per call.

### Onboarding Clarity
- [x] `syauth pair --help` lists every flag with its default.
- [x] Errors name the problem (`LescUnsupported`, `AmbiguousPeer`, etc.).

### Production-Ready Defaults
- [x] `--bond-dir` defaults to `/var/lib/syauth/` per SPEC §4.4.
- [x] `--timeout-secs` defaults to 60 per the DoD.
- [x] `--adapter` defaults to `hci0` per SPEC §4.1.

### Golden Path Quality
- [x] Integration test `pair_golden_flow_writes_bond_and_list_shows_it`.

### Decision Load
- [x] The only mandatory operator decisions are "pick peer" (skipped with
  `--peer`) and "OOB matches? y/N" (skipped with `--yes`).

### Progressive Complexity
- [x] No flags required for the common case.
- [x] `--peer`, `--adapter`, `--timeout-secs` are opt-in.

### Error Quality
- [x] Every `PairError` variant carries actionable context.

### Failure Safety
- [x] Timeout → `Revoked` → no bond on disk. Test
  `pair_timeout_writes_no_bond_to_disk` verifies byte-equality of the bond
  file before and after.

### Runtime Transparency
- [x] Phase transitions are logged via `tracing::info!`-style stdout lines
  (`syauth: scanning`, `syauth: lesc-negotiating code=...`, `syauth:
  oob-pending`, `syauth: bonded`).

### Debuggability
- [x] `--yes` lets CI scripts reproduce the deterministic mock-driven flow
  end-to-end.

### Cross-Surface Consistency
- [x] OOB derivation function `oob_code_for_bond` is the same one S-014 will
  re-export to Android via UniFFI (committed today, consumed there).

### Workflow Consistency
- [x] CLI follows the S-013 install-pam pattern: `clap` derive, library
  module + thin `main.rs` dispatch.

### Change Safety
- [x] Bond writes are atomic via `BondStore::save` (S-005 contract).

### Experimentation Safety
- [x] Tests inject a tempdir for `--bond-dir`; never touches
  `/var/lib/syauth/`.

### Interaction Latency
- [x] `scan_peers` is bounded by the backend; the mock returns immediately.

### Developer Feedback Speed
- [x] Each state transition prints a line on stdout.

### Team Scale
- [x] OOB word list is version-controlled.

### System Scale
- [x] `PairBackend` trait keeps `bluer` behind a seam.

### Right Behavior by Default
- [x] `--yes` only skips the y/N prompt; safety gates are always enforced.

### Anti-Bypass Design
- [x] LE Secure Connections check runs regardless of `--yes`.

## 4. Tests

### TC-01: golden pair-list roundtrip

**Given** a tempdir bond store, a `MockPairBackend` configured with
`Scenario::Golden`, and `--yes` set.
**When** `syauth pair --bond-dir <td> --peer my-pixel --yes` runs.
**Then** the bond file exists, `BondStore::load(td/bonds.toml).list()` has one
entry whose `name == "my-pixel"`, and `syauth list --bond-dir <td>` prints
one TSV row containing that peer's id and name.

### TC-02: adapter without LE Secure Connections

**Given** a `MockPairBackend` with `adapter_supports_lesc = false`.
**When** `syauth pair --yes` runs.
**Then** the call returns `PairError::LescUnsupported { adapter, hint }`. The
hint mentions "kernel < X or older controller". The bond file is unchanged
(byte-equal to the pre-pair snapshot).

### TC-03: timeout

**Given** a `MockPairBackend` whose `initiate_lesc_with_peer` never resolves,
and `--timeout-secs 1`.
**When** `syauth pair --yes` runs.
**Then** the state machine transitions to
`PairingPhase::Revoked { reason: RevokeReason::Timeout }`. The bond file is
byte-equal to its pre-pair state.

### TC-04: operator says `N`

**Given** a `MockPairBackend` with `Scenario::Golden`, but
`OobConfirmation::Reject`.
**When** `syauth pair` runs without `--yes` (the mock supplies `N`).
**Then** the state machine transitions to
`PairingPhase::Revoked { reason: RevokeReason::OperatorReject }`. The bond
file is byte-equal to its pre-pair state.

### TC-05: ambiguous peer with `--yes`

**Given** a `MockPairBackend` that returns two peers both matching the
`--peer` filter, and `--yes` set.
**When** `syauth pair --peer pixel --yes` runs.
**Then** the call returns `PairError::AmbiguousPeer { matches }` with
exactly the two matched names.

### TC-06: `oob_code_for_bond` is deterministic and stable

**Given** a fixed 32-byte `bond_key` `[0x01; 32]`.
**When** `oob_code_for_bond(&bond_key)` is called twice.
**Then** the two returned `[String; 4]` tuples are byte-equal. Each word is
non-empty.

### TC-07: `syauth list` on empty store

**Given** an empty `--bond-dir` tempdir.
**When** `syauth list --bond-dir <td>` runs.
**Then** the CLI prints the single-line hint `(no bonds; run \`syauth pair\`
to add one)` and exits 0.

## Traceability
- Roadmap item: [specs/syauth/ROADMAP.md §S-011](../syauth/ROADMAP.md)
- Implementation files: `crates/syauth-cli/src/{main.rs,pair.rs,list.rs,oob.rs}`
- Test files: `crates/syauth-cli/tests/pair_flow.rs`
