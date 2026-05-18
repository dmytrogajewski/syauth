# JOURNEY-S-001: Scaffold `syauth-presenced` crate + binary + systemd unit

> **Spec anchors:** `specs/unlock-proximity/SPEC.md` §3 Approach (the
> "long-running desktop `syauth-presenced` user-service" framing), §4
> Architecture (the daemon shape in the ASCII block), §6 Rehydration
> (cold-start sequence — step 1 "open `${XDG_RUNTIME_DIR}/syauth/auth.sock`
> (file lock + bind)").
>
> **Roadmap row:** `specs/unlock-proximity/ROADMAP.md` Step S-001.
>
> **Closure condition (verbatim from ROADMAP.md):**
> `cargo test -p syauth-presenced --test lifecycle_smoke`
> — both tests pass; binary exit code 0; pid file removed on shutdown.

## Roadmap Link

- Source roadmap: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-001.
- Feature: new workspace member `syauth-presenced` — the daemon shell the
  rest of S-002..S-019 builds on. This step ships only the process
  skeleton (arg parsing, syslog tracing, single-instance pidfile lock,
  empty tokio loop, SIGINT/SIGTERM handling, systemd user unit). The
  Unix-socket RPC server, BLE peripheral, rotation, multi-peer, challenge
  flow, and PAM rewrite are explicitly later steps and out of scope here.

## 1. Journey

When **a desktop operator installs syauth and wants the daemon process
that all later unlock-path work depends on**, I want to **`cargo build -p
syauth-presenced --release`, drop the systemd user unit in place, and
`systemctl --user start syauth-presenced` so the binary stays up across
sessions, refuses a second instance under the same `${XDG_RUNTIME_DIR}`,
and shuts down cleanly on `SIGTERM`**, so I can **stack S-002 (Unix-socket
RPC), S-003 (BLE peripheral extract), and every later step on a daemon
whose lifecycle is already known-good — no later step has to debug
"why doesn't the daemon start" plumbing**.

## 2. CJM

The operator just finished the existing `syauth pair` flow (the
pre-S-001 codebase). Their bond record exists on disk, but `pam_syauth`
still drives BlueZ directly per-PAM-call — that's the architecture the
master SPEC is replacing. S-001 doesn't change any user-visible unlock
behaviour. What it does is create a process that can be started,
checked, and stopped — the foundation S-002 and beyond can hook into.
Friction today: there is no daemon, so there is no test bed for the
socket / BLE / rotation work that follows. This journey removes that
friction.

### Phase 1: Operator runs `cargo build -p syauth-presenced --release`

**User Intent:** produce the daemon binary on a fresh checkout, prove
the new crate is wired into the workspace, and prove the binary's
public surface is the four CLI arguments the SPEC demands.

**Actions:**
- Operator clones the repo, runs `cargo build -p syauth-presenced
  --release`.
- Operator runs `target/release/syauth-presenced --help` and confirms
  the four required flags appear: `--socket`, `--bonds-file`,
  `--keys-dir`, `--log-level`.

**Pain / Risk:**
- New workspace member not listed in the root `Cargo.toml` `members`
  array — `cargo build -p syauth-presenced` returns
  `package ID specification ... did not match any packages`.
- Default values for the three filesystem flags differ from the SPEC's
  stated paths — a downstream step (S-008 PAM client, S-009 install
  glue) wires to the wrong socket path and silently fails.
- `--log-level` has no validation — operator typos `info` as `infor`,
  process panics on startup with an unstructured message.

**Success Signal:** `cargo build` exits 0, `--help` lists the four
flags with the SPEC defaults documented inline.

### Phase 2: Operator runs `systemctl --user start syauth-presenced`

**User Intent:** keep the daemon up across login sessions via the
systemd user manager, with logs flowing to the journal under the
`syauth-presenced` tag.

**Actions:**
- Operator copies `crates/syauth-presenced/dist/syauth-presenced.service`
  to `~/.config/systemd/user/`, runs `systemctl --user daemon-reload`,
  then `systemctl --user start syauth-presenced`.
- Operator runs `journalctl --user -t syauth-presenced -f` and watches
  the startup line appear.
- Operator runs `ls -l ${XDG_RUNTIME_DIR}/syauth/presenced.pid` and
  confirms the pidfile exists with mode `0600`.

**Pain / Risk:**
- Pidfile parent directory `${XDG_RUNTIME_DIR}/syauth/` doesn't exist
  — daemon panics on the lock attempt instead of creating the dir.
- Second `systemctl --user start syauth-presenced` (manual race
  against systemd) races the first instance — both daemons hold the
  socket path simultaneously and corrupt later steps' expectations.
- `XDG_RUNTIME_DIR` not set (e.g. inside a serial console / cron) —
  daemon has nowhere safe to put the pidfile and must surface a typed
  error, not a stack trace.

**Success Signal:** `systemctl --user status syauth-presenced` reports
`active (running)`; pidfile exists; a second `start` invocation while
the first is up exits non-zero with a typed `another instance is
already running` message.

### Phase 3: Daemon receives SIGTERM during shutdown

**User Intent:** stop the daemon cleanly (operator-initiated, or
systemd-initiated on logout) and have the on-disk state (pidfile, the
not-yet-shipped socket from S-002 onwards) revert to "not running"
without manual cleanup.

**Actions:**
- Operator runs `systemctl --user stop syauth-presenced` (which sends
  SIGTERM, waits, then SIGKILL on the systemd default timeout).
- Operator runs `ls ${XDG_RUNTIME_DIR}/syauth/presenced.pid` and
  confirms the file is gone.
- Operator runs `systemctl --user start syauth-presenced` again and
  the next start succeeds with no "stale pidfile" error.

**Pain / Risk:**
- SIGTERM handler installed too late — first ~ms of startup is unkillable,
  smoke test flakes.
- Pidfile cleanup uses `unlink` but the lockfile is still open on
  another descriptor — file actually persists, next start refuses.
- SIGINT (operator hits Ctrl+C in a `journalctl -f` tail buffer)
  treated differently from SIGTERM — operator confusion.

**Success Signal:** SIGTERM → process exits with status 0 within
≤ 1 s; pidfile removed; subsequent `start` succeeds cleanly.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| New crate must be wired into workspace `members` or `cargo build -p` fails silently | 1 | The DoD bullet `Workspace Cargo.toml lists the new crate` is a one-line edit; we add it in the same patch as the crate itself |
| Default paths for `--socket`, `--bonds-file`, `--keys-dir` must match the SPEC literally so S-002 / S-008 can rely on them | 1 | Encode the SPEC paths as named module-level constants; `--help` surfaces them |
| Pidfile single-instance lock can race if implemented with `exists()` then `create()` instead of an atomic lock | 2 | Use `fcntl(F_SETLK)` advisory exclusive lock on the pidfile fd — kernel-level atomic; second instance gets `EAGAIN`/`EACCES` and exits non-zero |
| Signal handler racing startup | 3 | Install the SIGINT + SIGTERM stream BEFORE the empty loop starts; `tokio::signal::unix` is the canonical primitive |

### North Star Summary

After S-001 closes, `cargo build -p syauth-presenced --release` produces
a binary that boots, logs `syauth-presenced started` to syslog under the
`syauth-presenced` tag, opens a single-instance pidfile lock at
`${XDG_RUNTIME_DIR}/syauth/presenced.pid`, refuses a second start,
sleeps in an empty tokio loop, and on SIGTERM / SIGINT cleanly removes
the pidfile and exits with status 0. The systemd user unit is shipped
next to the crate so operators (and later install steps) can wire it
without ad-hoc Service file authoring. No other behaviour is added —
S-002 wires the socket, S-003 wires BLE, S-004 wires rotation, etc.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] `cargo build -p syauth-presenced --release` produces a runnable
      binary in one command.
- [x] `target/release/syauth-presenced --help` documents every flag in
      ≤ 10 lines.

### Onboarding Clarity
- [x] `--help` text references the SPEC default paths inline so
      operators don't need to read the SPEC to know what to pass.
- [x] Startup syslog line is greppable: `syauth-presenced started`.

### Production-Ready Defaults
- [x] `--socket` defaults to `${XDG_RUNTIME_DIR}/syauth/auth.sock`.
- [x] `--bonds-file` defaults to `/var/lib/syauth/bonds.toml`.
- [x] `--keys-dir` defaults to `/var/lib/syauth/keys/`.
- [x] `--log-level` defaults to `info`.

### Golden Path Quality
- [x] Build, start, stop sequence is the SPEC §6 cold-start sequence
      step 1 (open + lock the pidfile), nothing more.

### Decision Load
- [x] Four flags total; no environment-variable shadow surface for any
      of them in S-001.

### Progressive Complexity
- [x] Empty tokio loop is the simplest possible main body; S-002
      replaces the loop with the RPC accept loop without touching the
      lock/signal scaffold.

### Error Quality
- [x] `XDG_RUNTIME_DIR` unset → typed error `runtime-dir-missing`.
- [x] Second instance → typed error `another instance is already
      running` with the offending pidfile path.

### Failure Safety
- [x] Pidfile cleanup runs on every exit path (signal, top-level
      error, normal shutdown).
- [x] Advisory lock means even a hard kill leaves no "stale pidfile
      blocks restart" condition (kernel releases the F_SETLK on exit).

### Runtime Transparency
- [x] syslog line on start; syslog line on signal-received; syslog
      line on shutdown-complete.

### Debuggability
- [x] `--log-level=debug` enables debug-level tracing-subscriber
      filters so operators can trace S-002 onwards.

### Cross-Surface Consistency
- [x] Default paths in `--help` match the strings in
      `specs/unlock-proximity/SPEC.md` §3 / §4 / §6.

### Workflow Consistency
- [x] Same `tracing_subscriber` initialization pattern reused from
      syauth-cli convention (clap derive, anyhow-shaped errors,
      `ExitCode` return).

### Change Safety
- [x] No public API surface to break — S-002 layers on top.

### Experimentation Safety
- [x] Smoke test overrides every default path with tempdir paths so
      tests do NOT touch `/var/lib/syauth/` and do NOT require root.

### Interaction Latency
- [x] Startup completes < 100 ms on the smoke-test hardware (empty
      tokio loop, no I/O beyond pidfile bind).
- [x] SIGTERM → exit ≤ 1 s (no slow drains).

### Developer Feedback Speed
- [x] `cargo test -p syauth-presenced --test lifecycle_smoke` runs in
      a few seconds and asserts the lifecycle gates directly.

### Team Scale
- [x] `crates/syauth-presenced/dist/syauth-presenced.service` is
      version-controlled so every operator gets the identical unit.

### System Scale
- [x] Single tokio runtime, no spawned tasks in S-001 — future steps
      add bounded `tokio::spawn`s under the same runtime.

### Right Behavior by Default
- [x] Default paths land at the SPEC's documented locations.
- [x] `--log-level=info` is the right default — `debug` would spam in
      production, `warn` would hide normal start/stop lines.

### Anti-Bypass Design
- [x] Single-instance lock is `fcntl(F_SETLK)`, a kernel primitive —
      operator cannot "trick" the lock with `rm` because the lock is
      held on the open file descriptor, not the directory entry.
- [x] `make scope-discipline` gate runs in CI; no `// v0.1 demo` /
      `// for now` markers in the production path.

## 4. Tests

### TC-01: `starts_and_terminates_cleanly`

**Given** a tempdir at `$T`, with the four flags pointed at
`$T/auth.sock`, `$T/bonds.toml`, `$T/keys/`, and `--log-level=info`.
**When** the smoke test `spawn`s the release binary, waits for the
pidfile `$T/runtime/syauth/presenced.pid` to appear, sends `SIGTERM`,
and waits for the process to exit.
**Then** the process exit status is `0`, the pidfile is removed, and
the wait completes within the 5-second test budget.

### TC-02: `refuses_second_instance`

**Given** an instance already running with the lock held on
`$T/runtime/syauth/presenced.pid`.
**When** the test `spawn`s a second instance with the same
`--socket` / `--bonds-file` / `--keys-dir` flags and waits for it to
exit.
**Then** the second instance exits non-zero within the 5-second test
budget; the first instance's pidfile is still present (its lock was
not stolen); after the test sends `SIGTERM` to the first instance, the
pidfile is removed.

## Implementation

Files created:
- `crates/syauth-presenced/Cargo.toml` — new workspace member.
- `crates/syauth-presenced/src/lib.rs` — library shell (re-exports the
  daemon-lifecycle primitives so future steps' integration tests can
  drive them in-process without spawning the binary).
- `crates/syauth-presenced/src/main.rs` — clap-derived CLI entrypoint
  that wires `tracing_subscriber` to syslog and delegates to the
  library `run()`.
- `crates/syauth-presenced/src/runtime.rs` — `Config`, `RunResult`,
  `run(Config)` async surface. Owns the pidfile lifecycle, the signal
  handler, the empty tokio loop, and the cleanup ordering.
- `crates/syauth-presenced/src/lock.rs` — `PidFileLock` RAII guard
  using `fcntl(F_SETLK)` advisory exclusive locks. Drop deletes the
  pidfile from the filesystem.
- `crates/syauth-presenced/dist/syauth-presenced.service` — systemd
  user unit.
- `crates/syauth-presenced/tests/lifecycle_smoke.rs` — TC-01 + TC-02.

Files modified:
- `Cargo.toml` (workspace root) — added `crates/syauth-presenced` to
  the `members` array.
- `specs/unlock-proximity/ROADMAP.md` — ticked S-001 DoD bullets and
  appended the `Traceability` line per the orchestrator's contract.

## Traceability

- Roadmap item: `specs/unlock-proximity/ROADMAP.md` Step S-001.
- Implementation files: see "Implementation" section above.
- Test files: `crates/syauth-presenced/tests/lifecycle_smoke.rs`.
