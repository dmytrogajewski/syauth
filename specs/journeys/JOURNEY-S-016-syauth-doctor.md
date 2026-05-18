# JOURNEY-S-016: `sy syauth doctor`

> **Spec anchors:**
>
> - `specs/unlock-proximity/SPEC.md` §3 Scope item 24 (verbatim):
>
>   > `syauth status` (existing subcommand) is extended to report:
>   > daemon liveness, count of bonded peers being advertised, time
>   > since last challenge, time since last connect by each peer.
>
>   S-017 owns the per-peer extension of `status`; S-016 ships the
>   sibling `doctor` subcommand that surfaces filesystem / unit /
>   socket / adapter health in greppable `key=value` lines so an
>   operator dashboard can `sy syauth doctor | grep daemon=` and
>   light up an alert without parsing JSON.
>
> - `specs/unlock-proximity/SPEC.md` §7 Trust Boundaries (verbatim):
>
>   > `bond_key` (32 bytes): sensitive. Lives at
>   > `/var/lib/syauth/keys/<peer_id>.bin` (0600 root-owned). Daemon
>   > reads on startup, never writes.
>
>   The `keys_<peer_id>_mode` probe is the operator-visible canary
>   for that file-mode invariant; if the keys file is `0644` the
>   bond_key is leaking to every local UID and the doctor must
>   surface a `warn` summary so the operator catches it before the
>   next `sudo` lands a relay.
>
> - `specs/unlock-proximity/SPEC.md` §8 Risks — SSH-session caveat
>   (verbatim):
>
>   > Operator runs sudo from an SSH session (different
>   > `XDG_RUNTIME_DIR` than the daemon) | unlock fails because
>   > socket path mismatch | Medium | PAM module's `--socket`
>   > argument lets ops point at the right path; doctor explains the
>   > SSH-case to operators
>
>   The `xdg_runtime_dir = set|unset` probe is the audit trail for
>   that mitigation: it shows whether the doctor process inherited
>   `XDG_RUNTIME_DIR` from the operator's session or fell back to
>   `/run/user/$UID`, so an ops-channel paste of the doctor's
>   output is enough to diagnose the SSH-from-laptop case without
>   asking the operator to dump their env.
>
> **Roadmap row:** `specs/unlock-proximity/ROADMAP.md` Step S-016.
>
> **Closure condition (verbatim from ROADMAP.md):**
>
> ```
> cargo test -p syauth-cli --test doctor_flow
> ```

## Roadmap Link
- Source roadmap: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-016.
- Feature: New `syauth doctor` subcommand. Inspects daemon liveness
  (PID file + socket reachability), bonds file presence and
  parseability, keys file mode 0600 and contents readable, BlueZ
  adapter `Powered=true`, systemd user unit state, last 10 lines of
  `/var/lib/syauth/last.log`, and surfaces the `XDG_RUNTIME_DIR` /
  SSH-session caveat. Output is one greppable `key=value` line per
  probe (plus a final `doctor=ok|warn|fail` summary) so
  `sy syauth doctor | grep daemon=` lights up operator dashboards.
  `--json` emits the same data as a typed JSON object for tooling.

## 1. Journey

When **a syauth operator notices that `sudo` is suddenly falling
through to FIDO (or worse, prompting for a UNIX password) and
suspects a regression somewhere in the
`pam_syauth` → `syauth-presenced` → BlueZ → bond-store → phone
chain**, I want to **type `sy syauth doctor` and immediately see
nine `key=value` lines that name the broken component plus a final
`doctor=ok|warn|fail` summary**, so I can **paste the output into
an ops channel, fix the root cause (`systemctl --user start
syauth-presenced`, `chmod 0600`, `bluetoothctl power on`, or move
my `sudo` invocation off the SSH session), and re-run the doctor
to confirm green — without grepping syslog, parsing
`bonds.toml` by hand, or attaching `strace` to a stuck PAM
module**.

## 2. CJM

The syauth operator is the human who runs `sudo` on this desktop.
They have already paired their phone via `sy syauth pair`; the
daemon, the PAM module, and the bonds file all exist on disk;
their phone is reachable in principle. What they're missing is a
single command that probes every link of the chain at once and
prints a result they can grep, diff, or paste. Today they would
have to run `systemctl --user status syauth-presenced`,
`ls -la /var/lib/syauth/keys/`, `bluetoothctl show`, `tail -n 10
/var/lib/syauth/last.log`, and `echo $XDG_RUNTIME_DIR` separately
— five commands, four different output formats, no summary. The
doctor collapses that into one.

### Phase 1: Happy path — daemon up, bonds healthy, no warnings

**User Intent:** Confirm the unlock chain is green before relying
on it (e.g., before a remote demo, or as the first step in a CI
smoke job that runs after `sy syauth install-pam`).

**Actions:**

1. Operator runs `sy syauth doctor` in their interactive shell.
2. Reads the nine `key=value` lines top-to-bottom.
3. Sees the final line `doctor=ok` and moves on.

**Pain / Risk:**

- The doctor could miss a degraded-but-not-dead component (e.g.,
  daemon up but `bluez_adapter=unpowered`) and report `ok` —
  every probe failure must downgrade the summary to at least
  `warn`.
- The doctor could spend tens of seconds on the BlueZ DBus probe
  and turn the operator's `Ctrl-C` reflex into a habit — the
  daemon-socket probe MUST use the same 50 ms `DAEMON_CONNECT_TIMEOUT`
  the PAM module uses, and the BlueZ probe must be best-effort
  (any failure folds into `unknown`, not into a hang).
- The doctor could leak a sensitive bytes-of-the-bond-key field
  into greppable output — the probe surface is mode + presence
  only; bytes never enter `stdout`.

**Success Signal:** Operator sees `doctor=ok` on the final line
and the eight prior probes all report a green token
(`daemon=up`, `bonds_count=N`, `keys_*_mode=0600`, `bluez_adapter`
in `{powered, unknown}`, etc.).

### Phase 2: Daemon-down case — flags systemctl + suggests start command

**User Intent:** Diagnose a stuck unlock when `sudo` falls through
to FIDO immediately. Suspected cause: the `syauth-presenced`
systemd user unit is not running (just booted from cold, or the
unit was stopped during a sysadmin shake-down).

**Actions:**

1. Operator runs `sy syauth doctor`.
2. Reads `daemon=down: <reason>` on the second probe line.
3. Reads `systemctl_user_unit=inactive` on a later line.
4. Runs `systemctl --user start syauth-presenced.service`.
5. Re-runs `sy syauth doctor`; sees `daemon=up` and `doctor=ok`.

**Pain / Risk:**

- The doctor's `daemon=down` line could be ambiguous between
  "socket file missing" and "socket present but daemon hung" —
  the `reason` token must distinguish (`socket-missing`,
  `connect-refused`, `frame-error`, `timeout`).
- The doctor could shell out to `systemctl --user` even on a CI
  host without `systemd-logind`, dump scary stderr, and confuse
  the operator — the `systemctl` probe must suppress stderr and
  fold any error into `unknown`.
- The doctor could mark the summary `fail` instead of `warn` on
  daemon-down, training the operator to ignore the difference —
  `daemon=down` is `fail`; missing systemd binary is `warn` (the
  daemon could still be alive under a different launcher).

**Success Signal:** Operator's third line of output names
`systemctl_user_unit=inactive`, they remember `systemctl --user
start syauth-presenced.service` from `docs/`, and the next doctor
run is green.

### Phase 3: Permission-broken case — keys file is 0644 instead of 0600

**User Intent:** Diagnose a regression after restoring
`/var/lib/syauth/` from a backup. The restore preserved owner and
group but reset modes to the umask default (`0644`) — the
bond_key is now world-readable. The operator does not realise
this until the doctor surfaces it; sudo still works because the
daemon doesn't care about file mode for its read.

**Actions:**

1. Operator runs `sy syauth doctor` as a post-restore sanity check.
2. Reads a `keys_<peer_id>_mode=0644 (expected 0600)` line for
   each affected file.
3. Reads `doctor=warn` on the final line (not `ok`).
4. Runs `chmod 0600 /var/lib/syauth/keys/*.bin`.
5. Re-runs the doctor; sees `keys_<peer_id>_mode=0600` and
   `doctor=ok`.

**Pain / Risk:**

- The doctor could read the keys file bytes to validate them and
  accidentally write a re-encoded copy elsewhere — the keys probe
  MUST be read-only (`stat`-only), never `read_to_end`.
- The doctor could flag a `0600`-but-symlink path as fine when
  the symlink target is `0644` — we follow the symlink for the
  mode check so the surfaced mode is the *effective* mode the
  daemon sees.
- The doctor could iterate the keys dir in non-deterministic
  filesystem order, breaking the `--json` consumer's diff —
  entries are sorted by `peer_id` before emit.

**Success Signal:** Operator's per-key mode lines all read
`0600`, `doctor=ok`, and the bond_key is no longer
world-readable.

### Phase 4: SSH-session caveat — XDG_RUNTIME_DIR mismatch

**User Intent:** Diagnose why `sudo` works from the laptop's
console but fails from an SSH session. Suspected cause:
`XDG_RUNTIME_DIR` is unset in the SSH environment so the PAM
module looks at `/run/user/$UID/syauth/auth.sock` while the
daemon — started by the console session — bound to
`/run/user/$UID/syauth/auth.sock` *too*, but the PAM module is
running as a different `$UID` (the operator's, not the target
user's).

**Actions:**

1. Operator runs `sy syauth doctor` over SSH.
2. Reads `xdg_runtime_dir=unset (fallback /run/user/1000)`.
3. Reads `daemon=down: connect-refused` two lines above.
4. Recognises this is the SPEC §8 SSH-session caveat from
   `docs/known-gaps.md`.
5. Either re-runs `sudo` from a `screen` session attached to the
   console, or passes `--socket` to the PAM module.

**Pain / Risk:**

- The doctor could pretend `XDG_RUNTIME_DIR` is set when the env
  is missing and the fallback path happens to exist — the probe
  must distinguish "env-set" from "env-unset, fallback used"
  explicitly.
- The doctor could itself crash if `XDG_RUNTIME_DIR` is set to a
  non-existent path — the probe records the path verbatim, never
  `stat`s it as a precondition for emit.
- The doctor could fold the SSH-caveat into the `daemon=down`
  reason and hide the SPEC §8 attribution — the
  `xdg_runtime_dir` line is its own probe so dashboards can
  alert on the SSH case separately.

**Success Signal:** Operator's output names both
`xdg_runtime_dir=unset (fallback ...)` and `daemon=down`, they
match it to `docs/known-gaps.md` SSH row, and their next
`sudo` succeeds (either from the console or with `--socket`
passed in).

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Operator runs five separate commands to diagnose one unlock failure | All | Doctor collapses to one command with nine `key=value` probes plus a summary |
| `0644` on a keys file is invisible to `sudo` (still works) until the next `chmod` audit | Phase 3 | Doctor flags `keys_*_mode=0644 (expected 0600)` and downgrades summary to `warn` |
| SSH-session unlock failures are diagnosed by reading SPEC §8 from memory | Phase 4 | `xdg_runtime_dir=unset (fallback ...)` is a one-line breadcrumb back to the SPEC row |
| BlueZ DBus probe could hang the doctor on a broken DBus stack | Phase 1 | Best-effort with a hard timeout: any DBus error folds into `bluez_adapter=unknown`, never blocks |
| Operator dashboards can't parse the human prose status output | All | `--json` mode emits the same data as a typed JSON object via `serde_json::to_string_pretty` |

### North Star Summary

A single `sy syauth doctor` run prints nine greppable lines that
diagnose every link of the unlock chain in under one second, and
a final `doctor=ok|warn|fail` summary that an ops dashboard can
alert on without parsing prose. `--json` mode emits the same
data shape so tooling can consume the doctor without scraping
its key=value lines.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] One `sy syauth doctor` run prints the full diagnostic; no
      sub-commands, no flags needed for the happy path.
- [x] Each probe has a hard timeout (50 ms socket connect, BlueZ
      best-effort) so wall-clock stays under one second on a
      healthy host.

### Onboarding Clarity
- [x] The `--help` text names every probe so the operator knows
      what `daemon=`, `bonds_count=`, `keys_*_mode=` mean before
      running the command.
- [x] The `key=value` format mirrors the existing `syauth status`
      labelled-line convention.

### Production-Ready Defaults
- [x] Defaults match the SPEC: socket at `${XDG_RUNTIME_DIR}/syauth/auth.sock`,
      bonds at `/var/lib/syauth/bonds.toml`, keys at
      `/var/lib/syauth/keys/`, audit log at `/var/lib/syauth/last.log`.
- [x] No `--bond-dir` / `--socket` flags are required for the
      operator-typical case.

### Golden Path Quality
- [x] Phase 1 (happy path) prints `doctor=ok`; integration test
      `reports_daemon_up_when_socket_responds` pins it.
- [x] The summary token is one of `ok | warn | fail`; no other
      values.

### Decision Load
- [x] One subcommand, one optional `--json` flag. No mode-toggles,
      no per-probe enable/disable.
- [x] The `--socket` override is present for the SSH-session edge
      case but not required.

### Progressive Complexity
- [x] Default mode is greppable lines; `--json` is opt-in for
      tooling.
- [x] No verbose / quiet modes — every probe always emits its line.

### Error Quality
- [x] `daemon=down: <reason>` names the reason explicitly
      (`socket-missing`, `connect-refused`, `frame-error`,
      `timeout`) so the operator can act without re-running with
      a `--debug` flag.
- [x] `keys_*_mode=0644 (expected 0600)` names the expected value
      so the operator doesn't have to look it up.

### Failure Safety
- [x] Doctor is read-only by contract: no `mkdir`, no `chmod`, no
      writes anywhere. Recoverable from a borked install because
      it never modifies state.
- [x] `--json` mode parses cleanly even if every probe fails; the
      schema is total over its variants.

### Runtime Transparency
- [x] Nine `key=value` lines plus a summary — every probe's
      outcome is visible.
- [x] No hidden state: the doctor's only side effect is writes to
      `stdout`.

### Debuggability
- [x] Output is `grep`-friendly: `sy syauth doctor | grep daemon=`
      isolates a single line for dashboards.
- [x] `--json` mode emits the same data for structured consumers.

### Cross-Surface Consistency
- [x] Uses the same `DEFAULT_BONDS_FILE`, `DEFAULT_KEYS_DIR`, and
      `DEFAULT_AUDIT_LOG_FILE` paths as the daemon, so the doctor
      and the daemon agree on what to inspect.

### Workflow Consistency
- [x] Subcommand placement mirrors `syauth status` (sibling),
      `syauth list`, `syauth pair`.
- [x] `--help` snapshot is committed alongside the other
      subcommand `--help` snapshots in `tests/snapshots/`.

### Change Safety
- [x] Doctor never writes to the host; the operator can re-run
      it freely between probes.
- [x] `--json` output is a stable schema (typed via `serde_json`).

### Experimentation Safety
- [x] Doctor is safe to run in CI, on a stranger's machine, on a
      production host: no writes, no DBus permissions required
      (DBus failure folds into `unknown`).

### Interaction Latency
- [x] Wall-clock target: < 1 s on a healthy host. The 50 ms
      socket connect timeout dominates the daemon-down case;
      everything else is local filesystem.

### Developer Feedback Speed
- [x] Tests pin the three load-bearing probes:
      `reports_daemon_up_when_socket_responds`,
      `reports_daemon_down_when_socket_missing`,
      `flags_keys_file_not_0600`.
- [x] Snapshot test pins the `--help` surface; a clap regression
      surfaces as a snapshot diff.

### Team Scale
- [x] Snapshot files are committed alongside source; team-wide
      help surface is version-controlled.
- [x] `--json` output is a stable contract for ops tooling.

### System Scale
- [x] The probe set is fixed; adding a probe is one new
      `key=value` line, no architectural change.
- [x] Per-key probes scale linearly with the bond count (and
      sort, so output stays deterministic).

### Right Behavior by Default
- [x] Every probe failure downgrades the summary; the operator
      cannot accidentally read `doctor=ok` when something is
      broken.
- [x] No `--ignore-bluez-errors` flag; doctor is conservative by
      construction.

### Anti-Bypass Design
- [x] Per-key mode check cannot be silenced; a `0644` keys file
      always shows up in the output.
- [x] The summary token is computed from the probes, not from a
      caller-supplied flag.

## 4. Tests

### TC-01: `reports_daemon_up_when_socket_responds`

**Given** a fake daemon listening on a tempdir Unix socket that
echoes back `Response::Status { peers: [], started_at: now }` to
any `Request::Status`.
**When** `syauth doctor --socket <tempdir>/auth.sock` runs.
**Then** stdout contains the literal line `daemon=up` and the
summary line is `doctor=ok`.

### TC-02: `reports_daemon_down_when_socket_missing`

**Given** a `--socket` path under a tempdir that does not exist.
**When** `syauth doctor --socket <tempdir>/no-such.sock` runs.
**Then** stdout contains `daemon=down: socket-missing` (or
similar reason token starting with `socket-missing`) and the
summary line is `doctor=fail`.

### TC-03: `flags_keys_file_not_0600`

**Given** a tempdir keys directory containing one
`<peer_id>.bin` file with mode `0o644`.
**When** `syauth doctor --keys-dir <tempdir>` runs.
**Then** stdout contains a line of the form
`keys_<peer_id>_mode=0644 (expected 0600)` and the summary line
is `doctor=warn`.

### TC-04: `doctor_help_snapshot`

**Given** the `syauth doctor --help` invocation.
**When** captured via `assert_cmd`.
**Then** `insta::assert_snapshot!` against
`tests/snapshots/cli__doctor_help_snapshot.snap` matches the
committed surface (any clap-derived shape change requires a
conscious `cargo insta accept`).

### TC-05: `json_mode_emits_typed_object`

**Given** a tempdir socket / bonds / keys (all valid) and the
`--json` flag.
**When** `syauth doctor --json` runs.
**Then** stdout parses as a JSON object with keys `daemon_socket`,
`daemon`, `bonds_file`, `keys`, `bluez_adapter`, `systemctl`,
`last_log_tail`, `xdg_runtime_dir`, `summary`; `summary` is one
of `"ok"`, `"warn"`, `"fail"`.

### TC-06: `xdg_runtime_dir_unset_uses_fallback`

**Given** the doctor is invoked with `XDG_RUNTIME_DIR` unset (and
`--socket` not passed).
**When** the `xdg_runtime_dir` probe emits.
**Then** stdout contains `xdg_runtime_dir=unset (fallback
/run/user/<uid>)` and the summary line is at least `warn`.

### TC-07: `last_log_tail_caps_at_ten_lines`

**Given** a `--audit-log` file with 25 lines.
**When** doctor runs.
**Then** `last_log_tail_1=...` through `last_log_tail_10=...`
appear (the most recent 10 lines), and no
`last_log_tail_11` line is emitted.

## Acceptance Criteria (verbatim from ROADMAP DoD)

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

## Implementation

**New production module:**
- `crates/syauth-cli/src/doctor.rs` — the entire `doctor` subcommand
  surface: `DoctorOpts` (clap), `DoctorReport`, `DaemonState`,
  `BondsReport`, `KeysReport`, `KeyFileReport`, `XdgRuntimeDirReport`,
  `DoctorError`, `run_doctor`, `build_report`, `write_keyvalue`,
  `write_json`, and the named constants `DEFAULT_BONDS_FILE`,
  `DEFAULT_KEYS_DIR`, `DEFAULT_AUDIT_LOG_FILE`,
  `EXPECTED_KEYS_FILE_MODE = 0o600`, `DOCTOR_LAST_LOG_TAIL = 10`,
  `DAEMON_CONNECT_TIMEOUT = 50 ms`,
  `DAEMON_STATUS_READ_TIMEOUT = 200 ms`. Probe sequence:
  `probe_xdg_runtime_dir` → `probe_daemon` → `probe_bonds` →
  `probe_keys` → `probe_bluez_adapter` (best-effort `"unknown"`
  pending S-017 wiring) → `probe_systemctl` (shell-out, stderr
  suppressed, any failure folds to `"unknown"`) →
  `probe_audit_log_tail` (capped at 10 lines, defensive ceiling
  4096) → `compute_summary`.
- `crates/syauth-cli/src/lib.rs` — `pub mod doctor`.
- `crates/syauth-cli/src/main.rs` — new clap variant `Cmd::Doctor`
  and `run_doctor_cli` dispatcher.
- `crates/syauth-cli/Cargo.toml` — added `serde`, `serde_json`,
  `syauth-presenced`, `nix` (production deps) and `serde_json` (dev
  dep). All four crates already live in `Cargo.lock` so no new
  transitive surface.

**New tests:**
- `crates/syauth-cli/tests/doctor_flow.rs` — four integration
  tests:
  - `reports_daemon_up_when_socket_responds` (TC-01) — fake daemon
    on a tempdir Unix socket responds to `Request::Status`; asserts
    `daemon=up`.
  - `reports_daemon_down_when_socket_missing` (TC-02) — non-existent
    socket; asserts `daemon=down`, `socket-missing`, `doctor=fail`.
  - `flags_keys_file_not_0600` (TC-03) — tempdir keys file with
    mode 0644; asserts the per-peer mode line includes
    `(expected 0600)` and the summary downgrades to `doctor=warn`.
  - `json_mode_emits_typed_object` (TC-04) — `--json` output parses
    as a JSON object with the documented top-level keys and a
    `summary` token in `{ok, warn, fail}`.
- `crates/syauth-cli/tests/cli.rs` — new `doctor_help_snapshot`
  test pinning the `--help` surface.

**New snapshots:**
- `crates/syauth-cli/tests/snapshots/cli__doctor_help_snapshot.snap`
  — pins the `syauth doctor --help` surface.
- `crates/syauth-cli/tests/snapshots/cli__help_snapshot.snap`
  (UPDATED) — adds the `doctor` line to the top-level command list.

**In-module unit tests (10) cover:**
- `key_file_peer_id_strips_bin_suffix`,
  `key_file_peer_id_rejects_non_bin` — file-name parser.
- `probe_keys_flags_0644_file_as_not_ok`,
  `probe_keys_sorts_files_by_peer_id` — keys-dir probe.
- `default_socket_path_appends_syauth_auth_sock` — SPEC §3
  socket-path default.
- `compute_summary_is_{fail,warn,ok}_*` — summary state machine.
- `probe_audit_log_tail_caps_at_ten` — log-tail cap.
- `write_keyvalue_emits_summary_token` — renderer ends with the
  `doctor=<summary>\n` line.

**Closure evidence:**

- `cargo test -p syauth-cli --test doctor_flow` — 4 passed, 0 failed
  (the verbatim closure-condition probe from ROADMAP).
- `cargo test -p syauth-cli` totals: 142 passed, 0 failed, 3 ignored.
- `make scope-discipline` — exit 0 ("Scope-discipline grep clean.").
- `make lint` — green; clippy, fmt, audit, deny all pass.
- `make test` workspace totals: 402 passed, 0 failed, 8 ignored
  (the ignored set is the pre-existing radio-gated DEV-004 + smoke
  rows; no new ignored tests added by S-016).

## Traceability
- Roadmap item: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-016.
- Implementation files: `crates/syauth-cli/src/doctor.rs`,
  `crates/syauth-cli/src/lib.rs`, `crates/syauth-cli/src/main.rs`,
  `crates/syauth-cli/Cargo.toml`.
- Test files:
  `crates/syauth-cli/tests/doctor_flow.rs` (new),
  `crates/syauth-cli/tests/cli.rs` (added `doctor_help_snapshot`),
  `crates/syauth-cli/tests/snapshots/cli__doctor_help_snapshot.snap`
  (new),
  `crates/syauth-cli/tests/snapshots/cli__help_snapshot.snap`
  (updated).
