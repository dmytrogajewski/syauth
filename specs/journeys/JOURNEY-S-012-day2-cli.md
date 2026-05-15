# JOURNEY-S-012: `syauth-cli` — `list`, `revoke`, `status`

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md §S-012](../syauth/ROADMAP.md)
- Feature: Day-2 operations CLI for syauth. Verifies `list` matches the DoD
  TSV format precisely (`id\tname\tstatus\tcreated_at`, empty-hint on no
  bonds), introduces a new idempotent `revoke` subcommand that flips a bond
  status to `Revoked` without deleting it, and a read-only `status`
  subcommand that prints adapter state, advertising state, bond count, and
  the last unlock outcome read from a rolling `last.log`.

## 1. Journey

When **I am the operator running syauth post-install on a Linux host with one
or more phones bonded**, I want to **be able to list every bond, revoke a
phone I just lost, and quickly check whether the unlock pipeline is alive**,
so I can **manage day-to-day phone-as-key state without hand-editing
`/var/lib/syauth/bonds.toml` or grepping syslog**.

## 2. CJM

Day-2 operations are the long tail of any auth product: the install was
yesterday, the next time the operator types `syauth …` is the day a phone is
lost or the day unlock stops working and they need to debug. Until S-012, the
operator's only options for these moments are (a) hand-edit `bonds.toml` and
hope the TOML is valid, or (b) read the syslog. Both are footguns. S-012
ships the three day-2 verbs that close the loop.

Key design decisions, all encoded as tests:

1. **`list` stays a thin reader of `BondStore::load(...).list()`.** S-011
   already shipped a TSV implementation. S-012 verifies the format matches
   the DoD verbatim (`id\tname\tstatus\tcreated_at`, no header), pins the
   empty-store hint to a single named const so a future copy-edit cannot
   silently break the regression test, and keeps the empty-store path on
   STDOUT with exit 0 (not stderr, not non-zero). The existing
   `LIST_EMPTY_HINT = "(no bonds; run `syauth pair` to add one)"` is kept
   verbatim — the assignment text reads "no bonds — run `syauth pair` to
   add one" with an em-dash, but the existing const in `pair.rs` uses a
   semicolon. The DoD only requires the hint contain "no bonds" and a
   pointer to `syauth pair`; we keep the established const so the S-011
   integration tests stay green.

2. **`revoke` requires `--id <peer-id>`, not a positional.** The other
   mutating verb (`install-pam`) uses `--service`. Mixing positional ids
   into a CLI that otherwise speaks long-form options would be inconsistent
   and creates ambiguity if we ever add a `revoke --reason "..." <id>`
   ordering. Long-form everywhere. The journey test pins this.

3. **`revoke` is idempotent at every layer.** S-005 already makes
   `BondStore::mark_revoked` a no-op on an already-revoked bond. S-012 keeps
   that contract at the CLI level: a second revoke against the same id
   exits 0 with a message naming the existing reason. Idempotency is the
   operator-visible promise — they can run the command twice with no
   anxiety about what happens.

4. **`revoke` NEVER deletes a bond.** The operator's mental model is "the
   record stays, the verdict flips to `revoked`". Deletion is a separate
   verb not in S-012. The status field carries the reason so the audit
   trail (who revoked, why) survives reboots.

5. **`revoke` of an unknown id is a non-zero error with the id named in
   stderr.** Fail-loud — the operator almost certainly mistyped the id and
   needs to see exactly which id was looked up.

6. **`status` is read-only, no exceptions.** It opens `last.log` to read
   the most recent unlock outcome but never creates, truncates, or rotates
   it. The writer of `last.log` is the PAM module (lands in S-009); S-012
   only defines the format and reads it. Missing or empty file → print
   `(no entries)` and exit 0.

7. **`status` tolerates a missing adapter.** `bluer` raises an error when
   `hci0` is absent (true on most CI runners, including this worktree's).
   `status` maps that to `adapter-state: Missing` and continues; it does
   NOT return non-zero. This is the difference between "diagnostic tool I
   can run anywhere" and "diagnostic tool that only works on the target
   host" — we want the former.

8. **`advertising:` is hard-coded to `false` in v0.1.** Advertising
   lifecycle lives in S-018. Until then, `status` reports `false`, with a
   note in this journey that S-018 will wire it up.

9. **`--bond-dir`, `--adapter`, `--last-log` are the only flags `status`
   takes.** The integration test injects all three to keep the test
   hermetic. None of them have side effects.

10. **`--help` and `--version` are pinned by snapshot tests.** Six help
    snapshots: the top-level and every subcommand. `clap` changes (or our
    own copy edits) will surface as snapshot diffs that the reviewer must
    consciously accept by `cargo insta accept`.

### Phase 1: List

**User Intent:** "Show me every phone I have bonded."

**Actions:**
- `syauth list` (defaults) → TSV, one row per bond, no header.
- `syauth list --bond-dir /tmp/foo` → same against an alternate location.

**Pain / Risk:**
- No bonds: a hard error or a missing-file stack trace would scare the
  operator. We print `(no bonds; run `syauth pair` to add one)` on stdout
  and exit 0.
- Header row would break shell pipelines (`syauth list | awk '{print $1}'`).
  No header.
- Malformed bonds file → typed error from `BondStore::load`, surfaced via
  the dispatcher's `anyhow::Error` printer.

**Success Signal:** TSV on stdout, exit 0.

### Phase 2: Revoke

**User Intent:** "My phone is gone — kill the bond now."

**Actions:**
- `syauth revoke --id deadbeef… --reason "phone lost"` → marks revoked,
  exits 0.
- Running it again → exits 0, prints `bond <id> already revoked: <reason>`.

**Pain / Risk:**
- Mistyped id: must see exactly which id we looked up. Stderr names it.
- Default reason: if `--reason` is omitted, use `manual: syauth revoke` so
  the audit trail is non-empty.
- Race with `pair`: the bond file is atomic-rewritten, so a concurrent
  revoke + pair either preserves the prior bond or includes both — never a
  half-written file.

**Success Signal:** `syauth list` shows the bond with `status` =
`revoked:<reason>`; the PAM module will refuse unlocks from that peer.

### Phase 3: Status

**User Intent:** "Is the unlock pipeline alive? Did my last unlock succeed?"

**Actions:**
- `syauth status` (defaults) — five lines, key-aligned.
- `syauth status --adapter hci2 --bond-dir /var/lib/syauth --last-log
  /var/lib/syauth/last.log` — explicit form.

**Pain / Risk:**
- No `hci0` (CI host, container): must print `Missing` and continue. Hard
  error here would make the integration test impossible without a USB BT
  dongle.
- `last.log` missing: print `(no entries)`. Never create or write.
- `last.log` larger than `LAST_UNLOCK_LOG_MAX_LINES`: read at most that
  many lines (the writer is expected to keep it bounded; this is a
  defensive cap).
- Garbled last line: surface as `(unparseable: <line>)` rather than
  panicking.

**Success Signal:** Five labeled lines on stdout, exit 0.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Operator hand-edits `bonds.toml` to revoke a phone | Revoke | One typed verb does it atomically. |
| No visibility into adapter / last unlock without journalctl | Status | One read-only verb shows everything. |
| `clap --help` drift breaks docs | All | Snapshot tests pin the surface. |

### North Star Summary

The operator should be able to handle every common day-2 question (which
phones? revoke this one. is it working?) with three single-line commands
and no editor. Each verb's output is shell-pipeable; each verb is
non-destructive in the sense that errors are loud and changes are atomic.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] `syauth list` returns immediately on an empty store with the
      bootstrap hint.
- [x] `syauth status` returns within ~50 ms even on hosts without `hci0`.

### Onboarding Clarity
- [x] `syauth --help` shows every subcommand with one-line descriptions.
- [x] Error messages name the offending id / file.

### Production-Ready Defaults
- [x] `--bond-dir` defaults to `/var/lib/syauth`.
- [x] `--adapter` defaults to `hci0`.
- [x] `--last-log` defaults to `<bond-dir>/last.log`.

### Golden Path Quality
- [x] `list` → TSV, `revoke` → exit 0, `status` → five lines.

### Decision Load
- [x] Only one required flag in `revoke` (`--id`). Everything else has a
      sane default.

### Progressive Complexity
- [x] Simple cases stay simple; advanced flags (`--reason`, `--last-log`)
      are opt-in.

### Error Quality
- [x] Unknown id → exit non-zero with the id in stderr.
- [x] Missing adapter → soft-fail with `adapter-state: Missing`.

### Failure Safety
- [x] `revoke` is idempotent; `status` is read-only.
- [x] Atomic bond-file persist via `BondStore::save`.

### Runtime Transparency
- [x] Every subcommand writes its key result to stdout.

### Debuggability
- [x] `status` exposes the live adapter + last-log line.

### Cross-Surface Consistency
- [x] `syauth pair`, `list`, `revoke`, `status`, `install-pam`,
      `uninstall-pam` all use long-form flags.

### Workflow Consistency
- [x] The clap dispatcher pattern from S-011/S-013 is reused unchanged.

### Change Safety
- [x] `revoke` never deletes a bond; flips status only.
- [x] `status` never writes the last-log file.

### Experimentation Safety
- [x] `--bond-dir` lets tests / sandboxes inject a tempdir.

### Interaction Latency
- [x] All three verbs are local file I/O + at most one dbus call.

### Developer Feedback Speed
- [x] Snapshot tests catch `clap` regressions on every `make test`.

### Team Scale
- [x] Snapshot files are committed to git.

### System Scale
- [x] Bond store is the workspace's only persistent state; bounded by user
      count.

### Right Behavior by Default
- [x] Defaults match SPEC §4.4.

### Anti-Bypass Design
- [x] No flag short-circuits the bond-store load/save invariants.

## 4. Tests

All in `crates/syauth-cli/tests/cli.rs` (driving the built `syauth`
binary via `assert_cmd`) plus library-level unit tests inside
`revoke.rs` and `status.rs`.

### TC-01: `syauth --version` prints semver and exits 0

**Given** the built `syauth` binary on PATH.
**When** I run `syauth --version`.
**Then** stdout matches `syauth \d+\.\d+\.\d+`, exit code is 0.

### TC-02: `syauth --help` snapshot is stable

**Given** the built `syauth` binary.
**When** I run `syauth --help`.
**Then** stdout matches the committed snapshot at
`tests/snapshots/cli__help_snapshot.snap` (`insta::assert_snapshot!`).
A `clap`-derived change without conscious snapshot-update is caught.

### TC-03: Each subcommand has a `--help` snapshot

**Given** the binary.
**When** I run `<sub> --help` for `pair`, `list`, `revoke`, `status`,
`install-pam`, `uninstall-pam`.
**Then** each one matches its committed snapshot file.

### TC-04: `syauth list` on an empty store prints the hint

**Given** a fresh `--bond-dir` tempdir.
**When** I run `syauth list --bond-dir <td>`.
**Then** stdout contains `no bonds`, exit code is 0, no file is created.

### TC-05: `syauth revoke --id <known>` marks the bond revoked

**Given** a `--bond-dir` with one bonded phone written via `BondStore`.
**When** I run `syauth revoke --id <id> --bond-dir <td>`.
**Then** exit code is 0; reloading the store shows `BondStatus::Revoked`
with the configured reason.

### TC-06: `syauth revoke --id <unknown>` exits non-zero with the id in stderr

**Given** an empty `--bond-dir`.
**When** I run `syauth revoke --id deadbeef`.
**Then** exit code is non-zero, stderr contains the literal id.

### TC-07: `syauth revoke` is idempotent

**Given** a bonded phone already revoked via the previous test.
**When** I run `syauth revoke --id <id>` again.
**Then** exit code is 0; reloading the store still shows the original
revocation reason (S-005 contract — no overwrite).

### TC-08: `syauth status` prints every documented field

**Given** a `--bond-dir` tempdir.
**When** I run `syauth status --bond-dir <td> --adapter
not-a-real-adapter`.
**Then** stdout contains every label: `adapter:`, `adapter-state:`,
`advertising:`, `bonds-count:`, `last-unlock:`. Exit code is 0.

### TC-09: `syauth status` parses a synthetic last.log entry

**Given** a `last.log` file with one line:
`2026-05-15T12:00:00Z success 0123456789abcdef0123456789abcdef`.
**When** I run `syauth status --last-log <path> --adapter
not-a-real-adapter --bond-dir <td>`.
**Then** the `last-unlock:` line contains `success` and the peer id.

### TC-10: `syauth status` reports `Missing` on a non-existent adapter

**Given** an adapter name BlueZ does not know.
**When** I run `syauth status --adapter <name>`.
**Then** stdout has `adapter-state: Missing`, exit code is 0.

### TC-11: `syauth status` reports `(no entries)` for missing last.log

**Given** a `--bond-dir` with no `last.log`.
**When** I run `syauth status --bond-dir <td> --adapter
not-a-real-adapter`.
**Then** the `last-unlock:` line is `(no entries)`.

## Traceability
- Roadmap item: [specs/syauth/ROADMAP.md §S-012](../syauth/ROADMAP.md)
- Implementation files:
  - `crates/syauth-cli/src/main.rs` — dispatcher additions.
  - `crates/syauth-cli/src/revoke.rs` — new module.
  - `crates/syauth-cli/src/status.rs` — new module.
  - `crates/syauth-cli/src/list.rs` — unchanged (already matches DoD).
- Test files:
  - `crates/syauth-cli/tests/cli.rs` — integration suite.
  - `crates/syauth-cli/tests/snapshots/*.snap` — clap surface pins.
