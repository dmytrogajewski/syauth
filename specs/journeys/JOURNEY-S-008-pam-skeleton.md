# JOURNEY-S-008: `syauth-pam` — module shell with `catch_unwind` and fail-closed

<!-- Authored per .agents/skills/journey/SKILL.md template. -->

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md](../syauth/ROADMAP.md) — item **S-008**.
- Feature: `syauth-pam` cdylib shell exposing three `pam_sm_*` entry points that
  fail-closed with `PAM_AUTHINFO_UNAVAIL` and never unwind across the FFI
  boundary.

## 1. Journey

When **a Linux admin (Alex) wires `pam_syauth.so` into a disposable PAM service
file for the first time** I want to **load the module, exercise every entry
point, and observe a deterministic `PAM_AUTHINFO_UNAVAIL` return plus a syslog
line that identifies the reason** so I can **prove the module's FFI boundary is
sound before any real authentication logic is added downstream**.

## 2. CJM

S-008 is the *load-bearing skeleton* of the desktop side. The protocol layer
(S-002…S-007) lives in pure-Rust crates and can be unit-tested without ever
touching libpam, but the moment we hand a `.so` to `dlopen` we cross the C ABI.
A single panic that unwinds across `extern "C"` corrupts the PAM stack, which on
a real `login` service means an instantly bricked machine. The journey for this
item is therefore *not* "authenticate a user" — it is **"prove the boundary
holds"**.

The work feeds two downstream items: S-009 (which replaces the stub return with
real authentication driven by the mock transport) and S-013 (which writes the
`syauth install-pam` helper that pastes a line into `/etc/pam.d/sudo`). Both
assume that (a) the three required symbols exist with exactly the right C
signature, (b) they fail closed when anything unexpected happens, and (c) every
return path logs a single grep-able syslog line.

### Phase 1: Build the module

**User Intent:** Get a `.so` on disk that the dynamic linker can resolve and
that PAM's `_pam_load_module` is happy to `dlopen`.

**Actions:** `make build` from the repo root. Inspect the output:
`ls -la target/release/libpam_syauth.so` and
`nm -D --defined-only target/release/libpam_syauth.so | grep ' pam_sm_'`.

**Pain / Risk:**
- Rust name mangling leaks into the dynamic symbol table → `pam_sm_*` not found
  at `dlopen` time → cryptic "module unknown" from PAM.
- The `cdylib` crate-type silently includes Rust standard library symbols that
  the loader complains about → use of `pam_syauth.so` poisons every PAM service
  that links it.
- `cargo build` succeeds but the resulting `.so` is missing `.eh_frame`, so
  `catch_unwind` panics abort the process instead of being caught — discoverable
  only by injecting a panic and watching `gdb` show `__libc_abort`.

**Success Signal:** `nm -D --defined-only` lists exactly three symbols whose
names start with `pam_sm_`, and `objdump -h` shows `.eh_frame` and
`.gcc_except_table` sections present.

### Phase 2: Stage the fixture PAM stack

**User Intent:** Point `pamtester` at a service file whose body references the
freshly built `.so` by absolute path — never the host's `/etc/pam.d`.

**Actions:** Run the e2e test (which generates `tests/pam.d/syauth-test` at
test time so that the path inside the file matches the actual build location)
and then invoke `pamtester --confdir tests/pam.d syauth-test "$USER" authenticate`.

**Pain / Risk:**
- A committed fixture references `/home/<old-user>/...` → unusable on any
  other machine or CI box → the file must be generated at test time, not
  checked in with a hard-coded path.
- The fixture leaks into `/etc/pam.d` because a developer copies it during
  manual testing → host login stack is broken until they revert.
- `pamtester` is missing on the runner → the test errors out unhelpfully
  instead of skipping cleanly.

**Success Signal:** The e2e test, when run with `SYAUTH_E2E=1` *and* pamtester
present, exits 0; when either is missing, it prints a one-line skip and
exits 0.

### Phase 3: Exercise the FFI boundary

**User Intent:** Confirm that *every* `pam_sm_*` returns `PAM_AUTHINFO_UNAVAIL`
in the stub state, that the syslog line `syauth: unlock unavailable reason=stub`
is emitted exactly once per call, and that a deliberately panicking call
returns `PAM_AUTH_ERR` instead of aborting.

**Actions:** Run `pamtester authenticate` and inspect both the return code
(printed by pamtester) and the most recent syslog/journalctl line tagged
`pam_syauth`.

**Pain / Risk:**
- A `println!` or `eprintln!` slips into the cdylib → the user sees garbage on
  the login screen → must be hard-banned by a clippy lint or grep check.
- The syslog connection is opened lazily but never closed; the daemon socket
  leaks on every PAM call → tracked via FFI audit checklist item C.
- The `catch_unwind` wrapper accidentally returns `PAM_SUCCESS` on caught
  panic — a default-allow bug. Mitigated by `unwrap_or(PAM_AUTH_ERR)` being
  the *outermost* expression.

**Success Signal:** Three calls (`authenticate`, `acct_mgmt`, plus an injected
panic) produce the expected return codes and a syslog line each. No spurious
output on stdout/stderr.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Generated fixture path is fragile across machines | 2 | Always regenerate at test time from `CARGO_MANIFEST_DIR + /target/release/libpam_syauth.so`; never commit the absolute path. |
| pamtester not preinstalled on dev boxes | 2 | Gate the e2e test on `SYAUTH_E2E=1` and on `which pamtester`; print a one-line skip so `make test` stays green. |
| Manual verification that `catch_unwind` actually catches | 3 | Add a unit test (in `#[cfg(test)] mod tests`) that calls a private helper which panics inside the wrapped closure and asserts the return is `PAM_AUTH_ERR`. The FFI signature itself stays untestable from Rust, but the wrapping is. |

### North Star Summary

A new contributor clones the repo, runs `make build && make test`, sees green,
and can then optionally run `SYAUTH_E2E=1 make test` (with pamtester installed)
to watch `pamtester` print
`pamtester: error: Module is unknown` *not* happening — instead, it prints
`Authentication service cannot retrieve authentication info` (the libpam string
for `PAM_AUTHINFO_UNAVAIL`), proving that the .so loaded, the symbol bound,
the body executed, the syslog line landed, and the stack fell through
correctly. Everything beyond that point is just filling in the body of the
closure inside `catch_unwind`.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] `make build` produces `target/release/libpam_syauth.so` on a clean clone in under 60 s of cargo time after deps are cached.
- [x] `make test` is green without any environment setup (e2e auto-skips when `SYAUTH_E2E != "1"`).

### Onboarding Clarity
- [x] The e2e test prints a single explanatory skip line — no silent ignores.
- [x] Every panic-caught return logs a reason field; reasons are short kebab-style tokens (`reason=stub`, `reason=panic`).

### Production-Ready Defaults
- [x] Default return is the fail-closed `PAM_AUTHINFO_UNAVAIL`, not `PAM_SUCCESS`.
- [x] Logging facility is `LOG_AUTHPRIV`, matching the rest of the PAM stack.

### Golden Path Quality
- [x] Stub `authenticate` returns `PAM_AUTHINFO_UNAVAIL` and logs the documented stub line.
- [x] `setcred` returns `PAM_SUCCESS` (auth modules MUST implement setcred per `/pam`).
- [x] `acct_mgmt` returns `PAM_AUTHINFO_UNAVAIL` (no account info available in stub).

### Decision Load
- [x] Return-code constants live as a small `pub const c_int` block at the top of `entry.rs` — no curated dep, no version pin to maintain.
- [x] Logging crate is the well-known `syslog` crate; no choices to make per call.

### Progressive Complexity
- [x] The simple case (load + return) needs zero config.
- [x] No module arguments are parsed yet — that arrives in S-009/S-013.

### Error Quality
- [x] Every return path logs a reason; absence of a log line indicates a panic before logger init.
- [x] Panic boundary's syslog line names `reason=panic` so post-mortems are trivial.

### Failure Safety
- [x] `catch_unwind` returns `PAM_AUTH_ERR` on panic — fail closed.
- [x] Unsafe block has a `// SAFETY:` comment naming the invariant (handle is opaque and unused in stub).

### Runtime Transparency
- [x] One syslog line per entry-point invocation.
- [x] No silent success.

### Debuggability
- [x] Syslog tag `pam_syauth` makes greppable filtering trivial.
- [x] The exact stub message is grep-pinned by the e2e test, so any drift is caught.

### Cross-Surface Consistency
- [x] Logging uses the same facility/tag as the rest of the (future) module — single point of truth in `entry.rs`.

### Workflow Consistency
- [x] Crate layout mirrors the workspace convention: `crates/syauth-pam/src/{lib.rs, entry.rs}`.

### Change Safety
- [x] The fixture pam.d file is regenerated at test time — never committed with a stale path.

### Experimentation Safety
- [x] `tests/pam.d/syauth-test` references this repo's `.so` only; cannot affect the host PAM stack.

### Interaction Latency
- [x] All three entry points return synchronously in micro-seconds; no I/O beyond a `syslog` send.

### Developer Feedback Speed
- [x] Test that panics inside the closure runs in micro-seconds; iteration loop is fast.

### Team Scale
- [x] Fixture is regenerated; no per-developer customizations needed in version control.

### System Scale
- [x] Stateless per call; no static mut state; reentrant by construction.

### Right Behavior by Default
- [x] `PAM_AUTHINFO_UNAVAIL` is the right default for an unimplemented auth path.
- [x] Auth modules implement BOTH `pam_sm_authenticate` AND `pam_sm_setcred` per the libpam contract; we ship both even when one is a trivial stub.

### Anti-Bypass Design
- [x] No bypass surface: catch_unwind is the outermost expression, and unwrap_or is a fail-closed default.

## 4. Tests

### TC-01: cdylib exports exactly three PAM entry points

**Given** the workspace is built with `cargo build --release -p syauth-pam`.
**When** `nm -D --defined-only target/release/libpam_syauth.so | grep ' pam_sm_'` is run.
**Then** exactly three lines are printed: `pam_sm_authenticate`, `pam_sm_setcred`, `pam_sm_acct_mgmt`. No additional `pam_sm_*` symbols. No Rust-mangled names that start with `_ZN`.

### TC-02: every entry point body is wrapped in `catch_unwind`

**Given** the crate source is grep-able.
**When** `grep -n catch_unwind crates/syauth-pam/src/entry.rs`.
**Then** at least three occurrences are reported, and a unit test in `entry.rs` proves that injecting a panic inside the wrapped closure returns `PAM_AUTH_ERR` (the unit test exercises a private helper, since the `extern "C"` signatures are not directly callable from Rust without `unsafe extern` blocks).

### TC-03: stub `authenticate` returns `PAM_AUTHINFO_UNAVAIL` with the documented log line

**Given** `SYAUTH_E2E=1` is set and `pamtester` is installed.
**When** the fixture pam.d directory is generated and `pamtester --confdir <dir> syauth-test "$USER" authenticate` runs.
**Then** pamtester exits non-zero with text matching the libpam string for `PAM_AUTHINFO_UNAVAIL` ("Authentication service cannot retrieve authentication info"), and a syslog/journalctl line tagged `pam_syauth` containing the exact substring `syauth: unlock unavailable reason=stub` is present within the last second of logs.

### TC-04: e2e test skips cleanly when SYAUTH_E2E is unset

**Given** `SYAUTH_E2E` is unset.
**When** `cargo test --test pam_smoke` runs.
**Then** the test prints a single skip line (`skipping pam_smoke: set SYAUTH_E2E=1 to run`) and exits 0.

### TC-05: e2e test skips cleanly when pamtester is missing

**Given** `SYAUTH_E2E=1` but `pamtester` is not on `$PATH`.
**When** the test starts.
**Then** the test prints a skip line referencing the missing binary and exits 0 — never blocks `make test`.

### TC-06: fixture references the .so by absolute path

**Given** the fixture has been generated by the e2e test setup.
**When** the contents of `tests/pam.d/syauth-test` are read.
**Then** the `auth` line references `libpam_syauth.so` by an absolute path that begins with `/`, and the path resolves to a regular file the test process can stat.

### TC-07: no `println!` or `eprintln!` in the cdylib

**Given** the source tree is grep-able.
**When** `grep -nE '(println|eprintln)!' crates/syauth-pam/src/**/*.rs` runs.
**Then** there are zero matches. (Enforced by a test that does the grep at test time.)

### TC-08: panic inside wrapped closure returns `PAM_AUTH_ERR`

**Given** a private helper `run_entry` that takes a closure returning `c_int`.
**When** the closure unconditionally panics.
**Then** `run_entry` returns `PAM_AUTH_ERR`. Asserted by unit test in `entry.rs`.

## Implementation

- `Cargo.toml` — workspace manifest (minimum needed for S-008).
- `crates/syauth-pam/Cargo.toml` — declares `cdylib`+`rlib` crate-type and the `pam_syauth` library name.
- `crates/syauth-pam/src/lib.rs` — crate root, documents the `#![allow(unsafe_code)]` workspace-deny opt-out and re-exports the entry-point module.
- `crates/syauth-pam/src/entry.rs` — PAM constants, `run_entry` panic-boundary helper, syslog opener, and the three `pam_sm_*` exports.
- `tests/pam_smoke.rs` — e2e harness gated on `SYAUTH_E2E=1` and the presence of `pamtester`.
- `tests/pam.d/syauth-test` — fixture template (the actual `.so` absolute path is patched in by the harness at test time, then the file is rewritten in-place).

## Traceability
- Roadmap item: [specs/syauth/ROADMAP.md §S-008](../syauth/ROADMAP.md#step-s-008-syauth-pam--module-shell-with-catch_unwind-and-fail-closed)
- Implementation files: see "Implementation" above.
- Test files: `crates/syauth-pam/src/entry.rs` (`#[cfg(test)] mod tests`), `tests/pam_smoke.rs`.

### Design decisions

- **PAM constants strategy.** We define `PAM_SUCCESS`, `PAM_AUTH_ERR`, `PAM_AUTHINFO_UNAVAIL`, `PAM_IGNORE` as `pub const c_int` at the top of `entry.rs`. Rationale: the only inputs to S-008's three entry points are an opaque `pamh: *mut c_void`, flags, and argv. We never call into libpam — we just need the three integer return codes. Pulling in `pam-sys` for four constants would inflate the audit surface (S-009/S-010 will add the real binding when we actually call `pam_get_item`). Per `.agents/skills/pam/SKILL.md` Phase 2 §4 the curated crate is preferred *when* we use the C types — here we use only the return values, which are stable per the Linux-PAM ABI.
- **Logging strategy.** The `syslog` crate (the `syslog = "7"` crate from `cyplo/syslog` — actively maintained) opens a Unix-socket connection lazily inside each entry point. We use `Formatter3164` with `facility = LOG_AUTHPRIV` and `process = "pam_syauth"`. The connection is dropped at function exit; the cost is one extra connect() per PAM call, which is amortized inside the PAM stack overhead. Global state would require `OnceLock<Mutex<...>>` which clashes with the spec rule "no mutable global state in the PAM module."
- **Syslog verification in e2e.** We use `journalctl -t pam_syauth --since "1 sec ago"`. This pulls only entries tagged `pam_syauth` and avoids needing read access to `/var/log/auth.log` (which is root-only on most distros). If `journalctl` is absent (musl boxes, alpine), the test skips with a documented note.
- **Fixture generation.** The e2e test writes `tests/pam.d/syauth-test` at runtime from a template, substituting the absolute path to `target/release/libpam_syauth.so` (resolved from `CARGO_MANIFEST_DIR`). A committed placeholder file lives in `tests/pam.d/syauth-test` so the directory and filename are version-controlled; the harness *overwrites* it before each run.
- **`PAM_IGNORE` is declared but unused** in S-008 itself. We include it so that S-009 can use it without a follow-up touch to constant definitions, keeping diffs in future items focused on behavior.
