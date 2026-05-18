# JOURNEY-S-009: `syauth install-presenced` + retire short-burst advertise

> **Spec anchors:** `specs/unlock-proximity/SPEC.md` §3 Scope item #9
> ("systemd user unit: `crates/syauth-presenced/dist/syauth-presenced.service`,
> installed by `syauth install-presenced` (new subcommand) with
> `WantedBy=default.target`."); §4 Migration & Compatibility (the
> long-lived daemon replaces the per-PAM-call advertise burst); §3
> Decisions row "Daemon process model" (per-user, not system —
> install lives in `~/.config/systemd/user/`).
>
> **Roadmap row:** `specs/unlock-proximity/ROADMAP.md` Step S-009.
>
> **Closure condition (verbatim from ROADMAP.md):**
>
> ```
> cargo test -p syauth-cli --test install_presenced_flow
> git grep -n "fn connect" crates/syauth-transport/src/bluez_advertise.rs   # empty
> ```

## Roadmap Link
- Source roadmap: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-009.
- Feature: add the `install-presenced` subcommand to `syauth-cli`
  (mirrors `install-pam`): copies the daemon binary to
  `/usr/local/libexec/syauth-presenced`, installs the systemd user
  unit to `~/.config/systemd/user/syauth-presenced.service`, runs
  `systemctl --user daemon-reload`, and enables + starts the unit.
  `syauth install-pam` grows a `--with-presenced=true` default so the
  two installs are bundled. Delete the old short-burst
  `BluerAdvertiser::connect` path from
  `crates/syauth-transport/src/bluez_advertise.rs` (safe now that
  S-008 removed the only caller).

## 1. Journey

When **a Linux operator who has just paired their phone with the
`syauth` desktop (S-013 install-pam done) and wants the long-lived
presence daemon (S-001..S-008) to start up on every login without
hand-editing systemd units**, I want to **run a single
`syauth install-presenced` (or accept the default
`syauth install-pam --with-presenced=true` bundle) that drops the
daemon binary into a predictable `/usr/local/libexec/` slot, writes
the canonical systemd user unit under `~/.config/systemd/user/`,
reloads systemd, and enables + starts the unit** so I can **walk
away with a daemon that survives reboot, restarts on crash, and
honors the SPEC §3 Decisions row "Daemon process model" — per-user,
not system — without ever having touched `/etc/systemd/system/`
or run anything as root**.

## 2. CJM

The operator finished S-013 and has `pam_syauth.so` wired into a PAM
service file. They built the workspace once with `cargo build
--release`. Today, before S-009, they must manually copy
`target/release/syauth-presenced` into a writable directory, hand-
write a systemd user unit referencing it, run `systemctl --user
daemon-reload` and `enable --now syauth-presenced.service`, and
hope they kept the path consistent with the unit they wrote. Every
step is a chance to mis-spell the `ExecStart` path, drop the unit
into the system-wide tree (where `--user` cannot reach it), or
forget the `daemon-reload`. S-009 removes that entire ritual.

### Phase 1: Install — bundle the daemon binary + unit + systemd reload

**User Intent:** Make `syauth-presenced` a real, restart-safe,
per-user systemd service in one CLI call.

**Actions:**
1. Build the workspace (`cargo build --release`) so
   `target/release/syauth-presenced` exists.
2. Run `syauth install-presenced --from
   target/release/syauth-presenced` (or accept the default
   `syauth install-pam --service sudo --with-presenced=true` bundle
   that S-009 wires in).
3. Observe: the daemon binary is copied to
   `/usr/local/libexec/syauth-presenced`, the unit file appears at
   `~/.config/systemd/user/syauth-presenced.service`,
   `systemctl --user daemon-reload` and
   `systemctl --user enable --now syauth-presenced.service` run.

**Pain / Risk:**
1. Operator forgets the `--from` flag and the auto-detect picks the
   wrong binary (e.g. an in-tree `target/debug` artifact).
2. `XDG_CONFIG_HOME` is unset and the fallback to `~/.config/` mis-
   resolves (no `$HOME`, runs as root, etc.).
3. `systemctl` is missing (CI container without systemd) and the
   `enable --now` call fails opaquely.

**Success Signal:** `systemctl --user is-active syauth-presenced`
prints `active`; `journalctl --user -t syauth-presenced -f` shows
the daemon's startup banner.

### Phase 2: Dry-run / test — let CI exercise the installer without `systemctl`

**User Intent:** Run the installer hermetically — in a tempdir, with
no real systemd — so the CI gate at `make test` covers it.

**Actions:**
1. Pass `--dry-run --unit-dir <tempdir> --from <fake-binary>`.
2. Read stdout: lines like `would-run: systemctl --user
   daemon-reload` and `would-run: systemctl --user enable --now
   syauth-presenced.service` appear instead of real shell-outs.
3. Inspect `<tempdir>/syauth-presenced.service` — the file is
   present, mode-readable, and contains `ExecStart=<--from path>`.

**Pain / Risk:**
1. The dry-run path drifts from the live path — i.e. the unit text
   the operator sees in dry-run is not byte-identical to the unit
   the live path writes.
2. The `--unit-dir` override is too narrow (only covers the unit
   file path, not the systemctl invocations) so the dry-run still
   shells out.
3. The test fixture binary is not mode-executable; the installer
   refuses to copy it, hiding a real-world success path.

**Success Signal:**
`cargo test -p syauth-cli --test install_presenced_flow` passes;
the assertion reads back the unit file's `ExecStart=` line and
matches the `--from` path; stdout contains both `would-run:` lines.

### Phase 3: Bundle — `install-pam` calls `install-presenced` by default

**User Intent:** Treat S-009 as the missing companion to S-013, so
`syauth install-pam` does the right thing without a second
invocation.

**Actions:**
1. Run `syauth install-pam --service sudo` (no flags) — both the
   PAM service file edit AND the presenced install fire.
2. Run `syauth install-pam --service sudo --with-presenced=false`
   when the operator wants only the PAM line (e.g. they manage the
   daemon via their distro's systemd unit).
3. Both invocations are idempotent — second run is byte-identical.

**Pain / Risk:**
1. The bundled `install-presenced` step fails (no systemctl, dry-
   run was meant) but the PAM edit already happened — partial
   state.
2. The operator passes `--with-presenced=false` once, then forgets
   the daemon was never installed; PAM falls through to
   `pam_unix.so` silently.
3. The dry-run flag propagates from `install-pam` to
   `install-presenced` so the test harness can drive both without
   shelling out.

**Success Signal:** `syauth install-pam` exits 0; `systemctl
--user is-active syauth-presenced` is `active`; a subsequent run of
`syauth install-pam` reports both the AlreadyInstalled PAM line
and the unit-already-present presenced state.

### Phase 4: Retire the per-PAM-call advertise burst

**User Intent:** Delete the dead code path so the long-lived
`PersistentPeripheral` is the only advertise surface.

**Actions:**
1. Confirm `git grep -n "BluerAdvertiser::connect\|BluerAdvertiseSession"
   crates/` is empty after S-008.
2. Delete `BluerAdvertiser::connect` (and `connect_inner`),
   `BluerAdvertiseSession`, its `Session` impl, the
   `ensure_subscribed_and_ready` helper, and the
   `ADVERTISE_READ_BUFFER_BYTES` / `ADVERTISE_DISCOVERABLE` /
   `ADVERTISE_CONNECTABLE` constants that only the burst path
   referenced.
3. Keep `BluerAdvertiser::new_sync`, `rotating_uuid_for`,
   `current_minute_from`, `build_unlock_services` — S-003's
   `PersistentPeripheral` still reuses these.
4. Delete the `connect_rejects_when_not_paired` unit test (it
   exercises the deleted path); keep `new_sync_records_inputs`,
   `current_minute_from_extracts_minute_floor`,
   `dev004_security_flags_set_on_application`,
   `rotating_uuid_for_matches_free_function`, the per-minute
   rotation test, and the bond-key-dependence test.

**Pain / Risk:**
1. A test in another crate still pulls the burst path through a
   `BtPeer` dyn cast — silent compile-fail surfaces only at
   `make test`.
2. The pruned constants (`ADVERTISE_LOCAL_NAME`,
   `ADVERTISE_READ_BUFFER_BYTES`) are re-exported from
   `syauth-transport::lib.rs`; dropping them is a re-export
   compile error.
3. The `PersistentPeripheral` from S-003 silently regressed and
   nobody noticed because the burst was masking it.

**Success Signal:** `git grep -n "fn connect"
crates/syauth-transport/src/bluez_advertise.rs` returns empty;
`cargo test -p syauth-transport` is green;
`cargo test -p syauth-presenced` and `cargo test -p syauth-pam`
unchanged.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Operator has to hand-edit a systemd user unit. | Phase 1 | One-shot `install-presenced` writes the canonical unit + reloads systemd. |
| CI cannot exercise the installer because no real systemctl in the container. | Phase 2 | `--dry-run --unit-dir <tempdir>` flag set; stdout `would-run:` lines pin the call sequence. |
| Two-step `install-pam` then `install-presenced` is easy to forget. | Phase 3 | `--with-presenced=true` default bundles the two. |
| Dead `BluerAdvertiser::connect` path lingers and confuses future readers. | Phase 4 | Delete the burst surface; `PersistentPeripheral` is the only advertise path. |

### North Star Summary

A fresh-paired operator runs `syauth install-pam --service sudo`
once. The PAM service file is atomically edited (S-013), the daemon
binary is copied to `/usr/local/libexec/syauth-presenced`, the
systemd user unit is written to
`~/.config/systemd/user/syauth-presenced.service`,
`systemctl --user daemon-reload && enable --now` fires, and the
operator's next reboot leaves the daemon running. The radio-side
advertise path is owned exclusively by S-003's
`PersistentPeripheral`; the legacy per-PAM-call burst is gone from
the repository.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] One CLI call (`syauth install-pam` with default
      `--with-presenced=true`) takes the operator from "binary
      built" to "daemon enabled on boot".
- [x] No editor / no `chmod` / no `systemctl edit` — the installer
      handles the lot.

### Onboarding Clarity
- [x] `syauth install-presenced --help` snapshot-pinned; flags self-
      document.
- [x] `--dry-run` exposes exactly what the live path would do, so
      operators can preview before approving.

### Production-Ready Defaults
- [x] `--with-presenced=true` is the default — the bundle is the
      golden path.
- [x] `/usr/local/libexec/syauth-presenced` is the SPEC §3 anchor
      and is named via a constant (`DEFAULT_DAEMON_BIN_PATH`).

### Golden Path Quality
- [x] Live path: copy binary → write unit → `daemon-reload` →
      `enable --now`.
- [x] Dry-run path writes the unit and prints `would-run:` lines
      for the two `systemctl` invocations.

### Decision Load
- [x] Only one decision the operator must make: opt out of the
      bundle (`--with-presenced=false`).
- [x] Source binary location auto-detects via `current_exe()`
      sibling search when `--from` is omitted.

### Progressive Complexity
- [x] Simple case (just-paired operator) stays simple — one CLI
      call.
- [x] Advanced overrides (`--from`, `--unit-dir`, `--dry-run`) are
      opt-in.

### Error Quality
- [x] Missing source binary surfaces as a typed
      `InstallPresencedError::SourceMissing`.
- [x] Failed `systemctl` call surfaces the exit code + stderr.

### Failure Safety
- [x] `--dry-run` is the recovery escape hatch: preview before live
      run.
- [x] Re-running the installer is idempotent (the unit file is
      overwritten with byte-identical content; `enable --now` is
      a no-op on an already-enabled unit).

### Runtime Transparency
- [x] Stdout names every artifact written.
- [x] `journalctl --user -t syauth-presenced -f` is the operator's
      log view.

### Debuggability
- [x] `--dry-run` stdout is the audit trail — every `would-run:`
      line shows the exact command that would have fired.
- [x] Unit file is plain text on disk; operators can diff against
      the bundled `dist/syauth-presenced.service`.

### Cross-Surface Consistency
- [x] The unit file is sourced via `include_str!` from
      `crates/syauth-presenced/dist/syauth-presenced.service`; the
      same bytes the package ships are the bytes the installer
      writes.
- [x] Help-text terminology mirrors `install-pam`.

### Workflow Consistency
- [x] `install-presenced` mirrors `install-pam`'s flag style
      (`--service`, `--pam-dir`, `--yes`) → (`--from`, `--unit-dir`,
      `--dry-run`).
- [x] Both subcommands return typed `*Outcome` enums; the CLI
      dispatch prints a one-liner per variant.

### Change Safety
- [x] `--dry-run` is the preview before live.
- [x] The installer overwrites the unit file in place — operator
      drop-ins under
      `~/.config/systemd/user/syauth-presenced.service.d/` are
      preserved (systemd loads them on next `daemon-reload`).

### Experimentation Safety
- [x] `--unit-dir <tempdir>` makes the installer hermetic;
      `--dry-run` skips all shell-outs.
- [x] CI exercises both via `tests/install_presenced_flow.rs`.

### Interaction Latency
- [x] Live path: one `fs::copy`, one `fs::write`, two `systemctl`
      calls. Sub-second.
- [x] Dry-run path: one `fs::write` + two `println!`. Instant.

### Developer Feedback Speed
- [x] Failing test names the missing assertion in one line.
- [x] Snapshot drift surfaces as a `cargo insta` diff.

### Team Scale
- [x] The unit file is version-controlled at
      `crates/syauth-presenced/dist/syauth-presenced.service`;
      every install on every machine writes the same bytes.

### System Scale
- [x] Per-user installation means a multi-user host gets N
      independent daemons (one per logged-in user), each with its
      own `XDG_RUNTIME_DIR` socket — no privilege coupling.

### Right Behavior by Default
- [x] `--with-presenced=true` is the default.
- [x] Auto-detect picks the sibling `syauth-presenced` next to the
      currently-running `syauth` binary.

### Anti-Bypass Design
- [x] No `--skip-systemctl` knob in live mode; if the operator does
      not want the systemctl side effects they ask for `--dry-run`
      (which then writes a tempdir file and prints `would-run:`
      lines).
- [x] The unit-file bytes are `include_str!`'d at compile time —
      the installer cannot ship a unit that diverges from the
      bundled `dist/` copy.

## 4. Tests

### TC-01: `install_writes_unit_and_starts_service`

**Given** a tempdir for `--unit-dir`, a touched-but-empty file at
`<tempdir>/fake-daemon-binary` to stand in for the daemon source.

**When** the operator runs `syauth install-presenced --dry-run
--unit-dir <tempdir> --from <tempdir>/fake-daemon-binary`.

**Then** the unit file appears at
`<tempdir>/syauth-presenced.service`, is mode-readable, contains
`ExecStart=<--from path>`, and stdout includes both `would-run:
systemctl --user daemon-reload` and `would-run: systemctl --user
enable --now syauth-presenced.service`.

### TC-02: `install-presenced --help` snapshot

**Given** the built `syauth` binary.

**When** the test runs `syauth install-presenced --help` via
`assert_cmd::Command`.

**Then** stdout matches the committed snapshot
`tests/snapshots/cli__install_presenced_help_snapshot.snap`.

### TC-03: `BluerAdvertiser::connect` deletion

**Given** S-008 has retired the only caller of the burst path.

**When** `git grep -n "fn connect"
crates/syauth-transport/src/bluez_advertise.rs` is run.

**Then** the result is empty;
`cargo test -p syauth-transport` is green.

## Traceability
- Roadmap item: [specs/unlock-proximity/ROADMAP.md Step S-009](../unlock-proximity/ROADMAP.md)
- Implementation files:
  - `crates/syauth-cli/src/install_presenced.rs` (new)
  - `crates/syauth-cli/src/lib.rs` (module wired)
  - `crates/syauth-cli/src/main.rs` (dispatch)
  - `crates/syauth-cli/src/install_pam.rs` (bundles install-presenced)
  - `crates/syauth-transport/src/bluez_advertise.rs` (burst path deleted)
  - `crates/syauth-transport/src/lib.rs` (re-export pruned)
- Test files:
  - `crates/syauth-cli/tests/install_presenced_flow.rs` (new)
  - `crates/syauth-cli/tests/install_pam.rs` (TC11 added; existing TCs pinned with `--with-presenced=false`)
  - `crates/syauth-cli/tests/cli.rs` (new `install_presenced_help_snapshot`)
  - `crates/syauth-cli/tests/snapshots/cli__install_presenced_help_snapshot.snap` (new)
  - `crates/syauth-cli/tests/snapshots/cli__help_snapshot.snap` (refreshed)
  - `crates/syauth-cli/tests/snapshots/cli__install_pam_help_snapshot.snap` (refreshed)

## Implementation

### Files created
- `crates/syauth-cli/src/install_presenced.rs` — the new module that
  owns `InstallPresencedOpts`, `InstallPresencedOutcome`,
  `InstallPresencedError`, and the `install_presenced` driver, plus
  the named constants (`DEFAULT_DAEMON_BIN_PATH`,
  `SYSTEMD_USER_UNIT_NAME`, `SYSTEMD_USER_UNIT_BUNDLED`,
  `DAEMON_BIN_NAME`, `SYSTEMD_USER_UNIT_SUBDIR`,
  `XDG_CONFIG_HOME_ENV`, `XDG_CONFIG_HOME_FALLBACK_SUBDIR`,
  `WOULD_RUN_PREFIX`) and the unit-tested helpers
  `resolve_unit_dir`, `resolve_source_binary`, `atomic_write_text`,
  `rewrite_exec_start`, `run_systemctl`.
- `crates/syauth-cli/tests/install_presenced_flow.rs` — the
  hermetic integration test (TC-01: `install_writes_unit_and_starts_service`)
  that drives `syauth install-presenced --dry-run --unit-dir
  <tempdir> --from <fake>` and asserts the unit file, `ExecStart=`
  rewriting, and both `would-run:` stdout lines.
- `crates/syauth-cli/tests/snapshots/cli__install_presenced_help_snapshot.snap`
  — the help-text snapshot for `syauth install-presenced --help`.

### Files modified
- `crates/syauth-cli/src/lib.rs` — wires the new `install_presenced`
  module into the library surface.
- `crates/syauth-cli/src/main.rs` — adds `Cmd::InstallPresenced`,
  the `run_install_presenced` / `report_install_presenced`
  dispatchers, and the `run_install` chain that fires
  `install_presenced` when `--with-presenced=true`.
- `crates/syauth-cli/src/install_pam.rs` — `InstallOpts` grows
  `with_presenced` (default `true`), `presenced_dry_run`,
  `presenced_unit_dir`, `presenced_from`; the in-file unit-test
  fixture `opts_for` is updated to pass `with_presenced: false`.
- `crates/syauth-cli/tests/install_pam.rs` — existing TC01-TC09 pin
  `--with-presenced=false` to stay hermetic; new
  `tc11_install_pam_bundles_presenced_by_default` exercises the
  bundled `--with-presenced=true` flow with `--presenced-dry-run`.
- `crates/syauth-cli/tests/cli.rs` — adds the
  `install_presenced_help_snapshot` test.
- `crates/syauth-cli/tests/snapshots/cli__help_snapshot.snap` and
  `crates/syauth-cli/tests/snapshots/cli__install_pam_help_snapshot.snap`
  — refreshed for the new subcommand and the new install-pam flags.
- `crates/syauth-transport/src/bluez_advertise.rs` — deletes the
  burst path: `BluerAdvertiser::connect`,
  `BluerAdvertiser::connect_inner`, `BluerAdvertiseSession`, its
  `Session` impl, `ensure_subscribed_and_ready`, the
  `ADVERTISE_READ_BUFFER_BYTES` / `ADVERTISE_CONNECTABLE` constants,
  the `BtPeer` impl, and the `connect_rejects_when_not_paired`
  unit test. `build_unlock_services` is moved inside the test
  module so the DEV-004 link-encryption-flag assertion still
  compiles and runs. `BluerAdvertiser::new_sync`,
  `BluerAdvertiser::rotating_uuid_for`,
  `BluerAdvertiser::current_minute_from`, `ADVERTISE_LOCAL_NAME`,
  and `ADVERTISE_DISCOVERABLE` are preserved because
  `crate::PersistentPeripheral` (S-003) and other callers still
  reference them.
- `crates/syauth-transport/src/lib.rs` — drops the
  `ADVERTISE_READ_BUFFER_BYTES` re-export (only the burst path used
  it).
- `specs/unlock-proximity/ROADMAP.md` — ticks the S-009 DoD bullets
  and adds the Traceability paragraph.

### Closure-condition probes

```text
$ cargo test -p syauth-cli --test install_presenced_flow
running 1 test
test install_writes_unit_and_starts_service ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ git grep -n "fn connect" crates/syauth-transport/src/bluez_advertise.rs
(empty)
```

### Regression probes

- `cargo test -p syauth-pam` — 14 / 14 pass; the daemon Unix-socket
  client path is unaffected by the burst-path deletion.
- `cargo test -p syauth-presenced` — 3 / 3 pass; the daemon already
  used `PersistentPeripheral`, not the burst path.
- `cargo test -p syauth-transport` — 37 in-lib + 4 peripheral
  contract tests pass; the DEV-004 link-encryption flag assertion
  still pins the LESC contract via the relocated
  `build_unlock_services` helper.

### Deviations

None. The S-009 scope was implemented exactly as ROADMAP-named;
SPEC §3.2 D1–D8 and §3.3 ML "IN — v0.1.0" are unchanged.

The `BluerAdvertiser` struct (sans `connect`) survives as a pure
audit-helper carrier because the CLI status path and the
`PersistentPeripheral` builder still reach for the same
`new_sync` / `rotating_uuid_for` surface; the SPEC §3 Scope item #9
constraint ("`PersistentPeripheral` from S-003 is the only path
that opens an advertisement") is honored — no `connect` /
`advertise` / `serve_gatt_application` call exists in
`bluez_advertise.rs` after this change.
