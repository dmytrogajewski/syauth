# JOURNEY-S-013: `syauth-cli` — `install-pam` / `uninstall-pam` with atomic edit

<!-- Authored per .agents/skills/journey/SKILL.md template. -->

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md](../syauth/ROADMAP.md) — item **S-013**.
- Feature: `syauth install-pam` and `syauth uninstall-pam` subcommands that
  edit a PAM service file (default `/etc/pam.d/<service>`) atomically, taking a
  `.bak` backup before any write and restoring from it on uninstall.

## 1. Journey

When **a Linux admin (Alex) who has just paired their phone wants to wire
`pam_syauth.so` into `/etc/pam.d/sudo`** I want to **run one `syauth install-pam
--service sudo` command that takes a backup, inserts the syauth line at the top
of the auth stack, and refuses to clobber a pre-existing backup** so I can
**stop hand-editing PAM stack files — the single highest-risk operation in
syauth's deployment story — without becoming the next "locked out of sudo"
support thread**.

## 2. CJM

S-013 is the bridge between "I built `pam_syauth.so`" and "syauth actually
runs on my login stack." The spec calls editing `/etc/pam.d/*` "the worst
foot-gun" precisely because the failure mode — a typo in the auth stack that
denies every subsequent login — is unrecoverable without a rescue disk and is
the dominant support burden for every comparable PAM module (`pam-bluetooth`,
`pam-beacon`, `BLEUnlock`).

The friction map in `SPEC.md` §5.4 lists "Editing `/etc/pam.d/*` is scary" as
the **single biggest** phase-3 friction. Threat-model row **T-005** (PAM stack
misconfiguration leading to bypass) explicitly names "Ship `syauth install-pam`
helper" as the mitigation. S-013 satisfies both.

The helper has two non-obvious correctness requirements:

1. It must be **idempotent.** A second invocation must produce a byte-identical
   file to the first. This matters because admins routinely re-run config
   scripts, package post-install hooks fire on upgrades, and Ansible/Puppet
   converge to a desired state by repeated application.
2. It must **never destroy a backup it does not own.** The `.bak` next to a
   service file is the user's escape hatch. If the admin had a pre-existing
   `sudo.bak` from another tool, we must refuse to overwrite it. On uninstall,
   if we don't recognize a syauth line, we leave the backup alone — silence
   over destruction.

### Phase 1: Discover the helper exists

**User Intent:** Find out that `syauth` has a one-shot helper that handles the
PAM edit so they don't have to.

**Actions:** Read `docs/pam.md` or run `syauth --help`. Run `syauth install-pam
--help` to inspect flags.

**Pain / Risk:**
- The user doesn't know the helper exists, hand-edits `/etc/pam.d/sudo`, makes
  a typo, locks themselves out. Mitigation: prominent mention in
  `docs/getting-started.md` (out of scope for this step) and in `--help`.
- `--help` text is too terse to convey what the command actually does to their
  files. Mitigation: a one-line `about` plus per-flag `long_help` strings.
- The user expects this to also configure other services beyond `sudo`. The
  command takes `--service <name>`, but the user has to know to repeat it for
  `gdm-password`, `login`, etc. Mitigation: documented in `docs/pam.md` (out of
  scope for this step) and reflected in the journey north-star.

**Success Signal:** `syauth install-pam --help` and `syauth uninstall-pam
--help` both exit 0 and name every flag with a one-line description.

### Phase 2: Run install on a service file

**User Intent:** Edit `/etc/pam.d/sudo` to add the syauth line, atomically,
keeping a backup.

**Actions:** `sudo syauth install-pam --service sudo --yes`. The command
reads `/etc/pam.d/sudo`, copies it to `/etc/pam.d/sudo.bak` (refuses if a
`.bak` already exists), inserts the canonical line
`auth    required    pam_syauth.so timeout=1200` at the top of the auth block
in a `tempfile::NamedTempFile` created in the same directory, then
`persist`s the temp file over the original.

**Pain / Risk:**
- A previous tool already left `sudo.bak` in place — we'd silently overwrite
  the admin's known-good snapshot. Mitigation: refuse with a clear error that
  names the existing `.bak` file path and tells the admin how to proceed.
- The temp file is created in a different filesystem than `/etc/pam.d` and
  `persist` falls back to `copy + unlink`, breaking atomicity. Mitigation:
  always create the temp file in the same directory as the target via
  `NamedTempFile::new_in(pam_dir)`.
- The new file ends up with different permissions (e.g., `0644` from
  `umask 022`) than the original (`0644`). PAM files must be world-readable
  but not world-writable. Mitigation: explicitly copy the source file's
  permissions onto the persisted file via `PermissionsExt::set_mode` and
  verify in tests.
- The original file has trailing whitespace, mixed line endings, or non-UTF-8
  bytes (rare but legal in pam.d). Mitigation: treat the file as bytes; do not
  re-encode; only prepend the line at the documented position.
- A second invocation duplicates the line. Mitigation: regex-match
  `^\s*auth\s+\S+\s+pam_syauth\.so\b` before writing; if matched, exit 0 with
  a "no changes" message.

**Success Signal:** Exit 0. The first non-comment, non-blank `auth` line in
the file is now the canonical syauth line. `<pam-dir>/<service>.bak` exists
and is byte-equal to the file before the edit.

### Phase 3: Re-run install (idempotency)

**User Intent:** Confirm running the command twice is safe (e.g., a config
manager converging on desired state).

**Actions:** `sudo syauth install-pam --service sudo --yes` a second time.

**Pain / Risk:**
- The second run blows up because `sudo.bak` already exists (the bak it itself
  wrote on run #1). Mitigation: detect the idempotent case BEFORE checking
  bak existence — if the syauth line is already present, exit 0 with a "no
  changes" message and do not touch the bak.
- The second run silently appends another `pam_syauth.so` line. Mitigation:
  same — detect first.
- The second run reorders lines (e.g., a parser-based rewrite). Mitigation:
  bail out at the regex check; never rewrite the file when the line is
  present.

**Success Signal:** Exit 0 with "syauth line already present in
<path>; no changes". File is byte-identical to the post-first-install state.
`.bak` is byte-identical to the pre-first-install state.

### Phase 4: Run uninstall

**User Intent:** Remove syauth from the PAM stack and restore the file
exactly to the pre-install state.

**Actions:** `sudo syauth uninstall-pam --service sudo --yes`. The command
reads `/etc/pam.d/sudo`, finds the syauth line, verifies `sudo.bak` exists,
atomically replaces `sudo` with the bytes from `sudo.bak`, then deletes
`sudo.bak`.

**Pain / Risk:**
- The bak is missing (admin deleted it). Then we can't safely revert. We
  must NOT just strip the syauth line in-place — that could corrupt the stack
  if the line was edited. Mitigation: refuse with a clear, actionable error
  message.
- The file has no syauth line. We could be looking at a service file that was
  never modified by us. Mitigation: no-op exit 0 with a `WARN`; do NOT touch
  the `.bak` (it might belong to someone else).
- The restore is partial (process killed between rename and bak delete).
  Mitigation: bak deletion runs *after* the atomic rename; if we crash there,
  the file is correct and a stale bak remains — harmless and re-runnable.
- The bak was edited by hand between install and uninstall. We still trust it
  — that's the admin's prerogative — but we document the assumption.

**Success Signal:** Exit 0. The service file is byte-equal to the file before
install. `.bak` is gone.

### Phase 5: Run uninstall when nothing is installed

**User Intent:** Run uninstall safely on a service that does not have syauth
wired in (mistake, config manager idempotency).

**Actions:** `syauth uninstall-pam --service sudo --yes` when `sudo` has no
syauth line.

**Pain / Risk:**
- The command deletes the `.bak` (which belongs to another tool). Mitigation:
  never delete a bak we don't own — gate deletion on the presence of a
  recognizable syauth line in the *current* service file.
- The command rewrites the file with bak contents anyway. Mitigation: short-
  circuit on "no syauth line found" before any I/O.

**Success Signal:** Exit 0 with a `WARN` line to stderr ("no syauth line
found in <path>; nothing to uninstall"). No file changes.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Admin doesn't know the helper exists | 1 | Document in `docs/getting-started.md` and reference from `syauth --help` (follow-up: S-012 ships the `syauth` top-level help). |
| `.bak` from another tool would be clobbered | 2 | Refuse install when a `.bak` already exists; name the file in the error and instruct the admin to move it out of the way. |
| Atomic rename across filesystems silently degrades | 2 | Always create the temp file in `--pam-dir` (the destination dir), guaranteeing `persist` is a real rename. |
| Idempotency breaks if we check `.bak` before checking for the line | 3 | Detect the syauth-line-already-present case FIRST; only touch the bak on the actual first install. |
| Restoring without a bak is destructive guesswork | 4 | Refuse uninstall when bak is missing AND the line is present; instruct the admin. |
| Uninstall on an unrelated file destroys someone else's bak | 5 | Gate bak deletion on a recognizable syauth line in the current service file. |

### North Star Summary

A first-time syauth user pairs their phone, reads three lines of
`docs/pam.md`, runs `sudo syauth install-pam --service sudo`, sees a
one-line "wrote backup to /etc/pam.d/sudo.bak; inserted syauth at top of auth
block" message, then `sudo -k && sudo whoami` triggers the phone prompt.
Five minutes later they realize they want to roll back: `sudo syauth
uninstall-pam --service sudo` produces a byte-identical
`/etc/pam.d/sudo` to before. No editor, no diff, no sweat, no lockout.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] `syauth install-pam --service sudo --yes` completes in well under a second on a typical service file (single small read + write).
- [x] The user does not have to know `tempfile` semantics — atomicity is the default.

### Onboarding Clarity
- [x] Each subcommand exposes `--help` with a one-line `about` plus per-flag descriptions.
- [x] Error messages name the offending path and the recovery action ("move /etc/pam.d/sudo.bak aside and retry").

### Production-Ready Defaults
- [x] `--module-args` defaults to `timeout=1200` (the documented PAM module argument).
- [x] `--so-path` defaults to `pam_syauth.so` (PAM resolves via its module search path; we never hard-code an absolute path).
- [x] `--pam-dir` defaults to `/etc/pam.d`; tests inject a tempdir.

### Golden Path Quality
- [x] Install on a stock `/etc/pam.d/sudo` writes the canonical line at the documented position.
- [x] Uninstall restores byte-equality.

### Decision Load
- [x] Three flags total (`--service`, `--module-args`, `--so-path`) plus the test/admin escape hatches (`--pam-dir`, `--yes`).
- [x] No subcommand-of-subcommand structure; install/uninstall are siblings at the top level.

### Progressive Complexity
- [x] Simple case: `syauth install-pam --service sudo`. Power users override `--module-args` and `--so-path`.
- [x] Absolute-path overrides exist for testers and packagers but are not in the user-facing docs.

### Error Quality
- [x] Existing `.bak` → named in the error, recovery action stated.
- [x] Missing `.bak` on uninstall → named in the error, recovery action stated.
- [x] Missing service file → clear "no such file" message naming the path.

### Failure Safety
- [x] Atomic write via `tempfile::NamedTempFile::persist` — no half-written file even on kill -9.
- [x] Backup is always taken before any write.
- [x] Uninstall is a no-op when nothing is recognizable — never destroys an unrelated bak.

### Runtime Transparency
- [x] Every state transition prints one line to stdout: "wrote backup", "inserted syauth line", "restored from backup", or "no changes".
- [x] Warnings go to stderr.

### Debuggability
- [x] All paths (service file, bak, temp file) are logged on success and error.

### Cross-Surface Consistency
- [x] Behavior is identical for any service name — no per-service special-casing.
- [x] The same regex `^\s*auth\s+\S+\s+pam_syauth\.so\b` is used by both install (idempotency check) and uninstall (recognition check).

### Workflow Consistency
- [x] Crate layout follows the workspace convention: `crates/syauth-cli/src/{main.rs,install_pam.rs,uninstall_pam.rs}`.
- [x] Integration test lives at `crates/syauth-cli/tests/install_pam.rs`.

### Change Safety
- [x] `--yes` is required to skip the confirmation prompt; in normal CLI use, the admin sees a one-line preview and types `y`.
- [x] Tests always pass `--yes`; never depend on stdin.

### Experimentation Safety
- [x] Tests always use `--pam-dir <tempdir>`; the CLI never touches `/etc/pam.d` during the test suite.

### Interaction Latency
- [x] Single file read + single file write — bounded by disk syscalls, microseconds in practice.

### Developer Feedback Speed
- [x] The integration test is hermetic and runs in well under a second.

### Team Scale
- [x] No per-developer config; the helper is deterministic on any machine.

### System Scale
- [x] No global state; reentrant; safe to run from cron / Ansible.

### Right Behavior by Default
- [x] `auth    required    pam_syauth.so timeout=1200` matches the DoD verbatim.
- [x] Backup-first is the default and not optional.

### Anti-Bypass Design
- [x] There is no `--force` flag in v0.1 — clobbering a `.bak` is intentionally impossible.
- [x] There is no flag to skip the backup.

## 4. Tests

All tests live in `crates/syauth-cli/tests/install_pam.rs`. Each test drives
the built `syauth` binary via `assert_cmd::Command::cargo_bin("syauth")`,
isolates I/O to a `tempfile::TempDir`, and never touches `/etc/pam.d`.

### TC-01: install inserts the canonical line at the top of the auth block

**Given** a tempdir contains a copy of a realistic `sudo` PAM file (embedded
in the test as `FIXTURE_SUDO`).
**When** `syauth install-pam --service sudo --pam-dir <tempdir> --yes` runs.
**Then** exit code is 0; the resulting `<tempdir>/sudo` file contains the
canonical line `auth    required    pam_syauth.so timeout=1200` as the FIRST
non-comment, non-blank `auth` line; every other byte of the original is
preserved verbatim below it; `<tempdir>/sudo.bak` exists and is byte-equal
to `FIXTURE_SUDO`.

### TC-02: install is idempotent

**Given** TC-01 has just run.
**When** `syauth install-pam --service sudo --pam-dir <tempdir> --yes` runs
again.
**Then** exit code is 0; the file is byte-identical to the post-first-install
state; `<tempdir>/sudo.bak` is byte-identical to its post-first-install
contents (i.e., still equal to `FIXTURE_SUDO`).

### TC-03: install refuses to overwrite a pre-existing `.bak`

**Given** a tempdir contains `sudo` and a pre-existing `sudo.bak` with
contents `b"unrelated backup\n"`.
**When** `syauth install-pam --service sudo --pam-dir <tempdir> --yes` runs.
**Then** exit code is non-zero; the error names `<tempdir>/sudo.bak`;
neither `sudo` nor `sudo.bak` is modified.

### TC-04: uninstall restores byte-equality from `.bak`

**Given** the state at the end of TC-01.
**When** `syauth uninstall-pam --service sudo --pam-dir <tempdir> --yes` runs.
**Then** exit code is 0; `<tempdir>/sudo` is byte-equal to `FIXTURE_SUDO`;
`<tempdir>/sudo.bak` does not exist.

### TC-05: uninstall is a no-op when no syauth line is present

**Given** the state at the end of TC-04.
**When** `syauth uninstall-pam --service sudo --pam-dir <tempdir> --yes` runs.
**Then** exit code is 0; stderr contains a warning naming `<tempdir>/sudo`
and the phrase `nothing to uninstall`; no file is modified or removed.

### TC-06: uninstall refuses when bak is missing but syauth line is present

**Given** a tempdir contains an installed `sudo` file (with the syauth line)
but no `.bak`.
**When** `syauth uninstall-pam --service sudo --pam-dir <tempdir> --yes` runs.
**Then** exit code is non-zero; the error names the missing `.bak` and
instructs the admin to restore manually; no file is modified or removed.

### TC-07: install preserves file mode

**Given** a tempdir contains `sudo` with explicit mode `0o644`.
**When** install runs.
**Then** the post-install file has mode `0o644` (verified via
`std::os::unix::fs::PermissionsExt`).

### TC-08: install honors `--module-args`

**Given** a stock fixture.
**When** install runs with `--module-args 'timeout=60 debug'`.
**Then** the inserted line is `auth    required    pam_syauth.so timeout=60 debug`.

### TC-09: install honors `--so-path`

**Given** a stock fixture.
**When** install runs with `--so-path pam_syauth_test.so`.
**Then** the inserted line references `pam_syauth_test.so`; idempotency
regex still matches (because it only anchors on `pam_syauth*.so`... actually
no — the regex must be exact on `pam_syauth.so` to avoid surprises; this
test confirms that, and `--so-path` overrides are documented as the
admin's responsibility for matching uninstall).

### TC-10: `syauth install-pam --help` and `syauth uninstall-pam --help` exit 0

**Given** the built binary.
**When** each `--help` invocation runs.
**Then** exit code is 0; stdout contains the canonical line literal in the
help (for `install-pam`).

## Implementation

- `crates/syauth-cli/Cargo.toml` — adds `clap`, `anyhow`, `thiserror`,
  `tempfile`, `regex`, plus dev-deps `assert_cmd` and `predicates`.
- `crates/syauth-cli/src/main.rs` — replaces the placeholder with a clap
  Derive-based dispatcher routing `install-pam` and `uninstall-pam` to the
  library modules.
- `crates/syauth-cli/src/lib.rs` — new file; re-exports the two modules.
- `crates/syauth-cli/src/install_pam.rs` — read service file, idempotency
  check, backup-or-refuse, build new contents, atomic persist with mode
  preservation.
- `crates/syauth-cli/src/uninstall_pam.rs` — recognition check, bak-presence
  check, atomic restore from bak, bak deletion.
- `crates/syauth-cli/tests/install_pam.rs` — the ten test cases above, driven
  by `assert_cmd`.

## Traceability
- Roadmap item: [specs/syauth/ROADMAP.md §S-013](../syauth/ROADMAP.md#step-s-013-syauth-cli--install-pam--uninstall-pam-with-atomic-edit)
- Implementation files: see "Implementation" above.
- Test files: `crates/syauth-cli/tests/install_pam.rs`.

### Design decisions

- **Insertion position.** The DoD says `auth required pam_syauth.so
  timeout=1200`. We insert at the **top of the auth block** — above all
  existing `auth` directives. Rationale: per the `/pam` skill, this puts the
  phone-tap factor first, so the user is asked to tap their phone before any
  password prompt fires. The use of `required` (not `sufficient`) is taken
  verbatim from the DoD, with the documented consequence that the password
  stack still runs on every login; this is intentional for v0.1 — see
  SPEC.md §3.2 row D7 (PAM stack behavior) on why "fall through to
  `pam_unix.so`" is the chosen failure mode. A future step (out of scope
  here) may move to a `[success=ok default=ignore]` advanced syntax once we
  understand the operational behavior in the wild.
- **Why insert at the top of the auth block, not the very top of the file.**
  PAM service files commonly begin with `@include common-auth` directives
  or commented headers. Putting our line at the *very top of the file*
  would either run before `@include` blocks (correct) or before comment
  blocks that describe the stack (cosmetically ugly, semantically
  irrelevant). We choose "before the first `auth` directive" — i.e., the
  smallest semantic change — which means comments above the first `auth`
  line are preserved and our line slots in just before that first directive.
- **Why regex on `pam_syauth\.so` for recognition, not on the full line.**
  The admin may have edited the timeout, added `debug`, or changed
  whitespace. The recognition rule must be loose enough to match these
  variants but tight enough to never false-positive on (e.g.)
  `pam_syauth_legacy.so`. The regex `^\s*auth\s+\S+\s+pam_syauth\.so\b`
  with `\b` (word boundary) is the right precision.
- **Why refuse to overwrite an existing `.bak`.** A user's other tooling
  (Ansible playbook, distro package, manual edit) may have written
  `sudo.bak` already. Overwriting it would destroy the only known-good
  snapshot. The cost of refusing is one error message and a `mv`; the cost
  of overwriting is unrecoverable. Asymmetric.
- **Why no `--force` flag.** Anti-bypass design from the journey checklist:
  if we ship `--force`, every "I overwrote my backup" support ticket has a
  one-line repro. We instead document the manual recovery: `mv sudo.bak
  sudo.bak.before-syauth && syauth install-pam --service sudo`.
- **Why `regex` crate over hand-rolled scanning.** Three uses: idempotency
  check on install, recognition check on uninstall, and finding the first
  `auth` directive to anchor insertion. Hand-rolling three line scanners is
  more code and harder to read than a single `Regex::new(...)`. The
  `regex` crate is already a transitive dep in most Rust trees and the
  audit cost is low. Compile-time regex via `OnceLock` keeps the per-call
  cost flat.
- **Why CLI logic lives in library modules.** Per the assignment, the
  library/binary split makes the modules independently testable. The
  `assert_cmd`-based integration test drives the binary (the canonical
  user path), but a future fuzzing harness or in-process test can call
  `install_pam::install(&Opts { ... })` directly. The cost is one extra
  `lib.rs` file. The benefit is testability by construction.
- **Why mode preservation matters.** PAM files are world-readable
  (`0o644`) by convention; if our `tempfile`-persisted replacement landed at
  `0o600` (the temp-file default), `sudo` would still work (it runs setuid),
  but other modules in the stack and `pam_get_user` from non-root contexts
  could behave oddly. We `set_permissions` on the temp file to match the
  source before `persist` to keep mode invariant.
- **Why we do not chown.** Changing ownership requires privileges we may
  not have (running tests as non-root in CI). The original owner is
  inherited from the directory's umask on rename, which is what we want.
- **Why uninstall is a no-op when no syauth line is present.** The
  uninstall command must be safe to run multiple times (idempotency
  symmetry with install). It must also be safe to run on an unrelated
  service — the admin might be cleaning up. The minimal-surprise behavior
  is "exit 0, warn, do not touch the bak". This is the explicit DoD
  contract.
