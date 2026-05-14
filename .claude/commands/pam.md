---
name: pam
description: PAM module scaffolding, integration, and safe end-to-end testing for syauth
---

# Agent Instructions: PAM Module Workflow

<constraints>
Do not run git commands. All version control is handled by the user.
Follow the persona and contracts defined in AGENTS.md.
Run `make lint` before considering any step complete.
Never test PAM changes against the user's real login stack on the host machine. Always use a disposable service name (e.g. `syauth-test`) or a container/VM.
Unsafe FFI code requires a documented `// SAFETY:` justification — see /ffi for the audit checklist.
</constraints>

<role>
You are a Linux PAM specialist building a Rust-based authentication module. You understand the libpam ABI, the four PAM module types (auth, account, session, password), conversation callbacks, the `pam_handle_t` lifecycle, and the difference between module return codes (`PAM_SUCCESS`, `PAM_AUTH_ERR`, `PAM_IGNORE`, `PAM_AUTHINFO_UNAVAIL`).
</role>

You produce PAM modules that are correct under the libpam C ABI, fail closed by default, and are end-to-end testable without compromising the host's auth stack.

---

## When To Use This Skill

Invoke `/pam` when:
- Adding or modifying a `pam_sm_*` entry point in the syauth PAM module.
- Changing the conversation flow, prompts, or return-code semantics.
- Wiring syauth into a PAM service file (`/etc/pam.d/*`).
- Debugging a "permission denied" / "auth fail" with no clear cause in syslog.
- Reviewing changes that touch `unsafe extern "C"` exported symbols.

For pure FFI-safety review of an existing module, use `/ffi` instead.

---

## Phase 1: Understand The Module Contract

Before writing or changing code, write down which entry points you are touching and what each one must return on success and on every failure mode.

PAM module types and their entry points (declare ONLY what you implement):

| Module type | Required entry points |
|-------------|-----------------------|
| auth        | `pam_sm_authenticate`, `pam_sm_setcred` |
| account     | `pam_sm_acct_mgmt` |
| session     | `pam_sm_open_session`, `pam_sm_close_session` |
| password    | `pam_sm_chauthtok` |

Document for each entry point:
- Inputs read from `pam_handle_t` (user, rhost, tty, service, items).
- Side effects (set creds, open BT channel, write log).
- The full return-code matrix (success, denied, unavailable, ignore).
- Default-deny path — what happens when the Android peer is offline.

<output_format>
```
Entry point: pam_sm_authenticate
Reads:       PAM_USER, PAM_RHOST, syauth.conf (peer key)
Writes:      PAM data item "syauth.session_token"
Returns:
  PAM_SUCCESS              - peer challenge verified
  PAM_AUTH_ERR             - peer responded with bad signature
  PAM_AUTHINFO_UNAVAIL     - peer not reachable within timeout
  PAM_IGNORE               - module disabled in config (passes to next stack entry)
Default on panic in Rust:   PAM_AUTH_ERR (catch_unwind boundary)
```
</output_format>

---

## Phase 2: Scaffold The Crate

For a new PAM module crate (or new entry point), enforce these invariants:

1. `Cargo.toml`:
   ```toml
   [lib]
   crate-type = ["cdylib"]
   name = "pam_syauth"   # produces libpam_syauth.so
   ```
2. Symbol export — each `pam_sm_*` is `#[unsafe(no_mangle)] pub unsafe extern "C" fn`.
3. Every entry point body is wrapped in `std::panic::catch_unwind` and converts a caught panic to `PAM_AUTH_ERR` (never `PAM_SUCCESS`, never unwind across the FFI boundary).
4. Use `pam-sys` or `pam-bindings` for the C types; do not redeclare them.
5. Logging goes through `syslog` with facility `LOG_AUTHPRIV`. Never use `println!` / `eprintln!` from a PAM entry point — stdout/stderr inside a login session goes nowhere useful and can leak to the user.

<rule>
The default branch of every match on PAM input MUST be a failure return code. Fail closed. A missing case is a bypass.
</rule>

---

## Phase 3: Conversation Flow (When Needed)

If your module prompts the user (rare for syauth, since the Android peer drives the flow):

1. Fetch the conv struct: `pam_get_item(handle, PAM_CONV, ...)`.
2. Allocate `pam_message`/`pam_response` arrays using PAM's allocator contract — the application frees responses, you free messages.
3. NUL-terminate every string going through the conv pointer. A missing NUL is a crash, not a logic bug.
4. Treat any response read from the user as untrusted bytes; validate length before any further processing.

---

## Phase 4: Hermetic Test Rig

PAM is impossible to unit-test in isolation because it requires libpam to load the `.so`. Build a self-contained rig:

1. Build the module: `cargo build --release` → `target/release/libpam_syauth.so`.
2. Stage a private PAM tree under `tests/pam.d/syauth-test`:
   ```
   auth    required    $(pwd)/target/release/libpam_syauth.so debug
   account required    pam_permit.so
   ```
3. Use `pamtester` (or a thin Rust harness over `pam-sys`) to drive the stack:
   ```bash
   PAM_CONFDIR=$(pwd)/tests/pam.d pamtester syauth-test "$USER" authenticate
   ```
4. Capture syslog into a per-test file:
   ```bash
   journalctl --user -t pam_syauth --since "1 second ago" > tests/out/$NAME.log
   ```
5. Run the rig from `make test` via a `tests/pam_e2e.rs` integration test that shells out to the harness. Mark with `#[ignore]` and gate on `SYAUTH_E2E=1` if it requires installed `pamtester`.

<rule>
Never edit `/etc/pam.d/*` from test code. Always point pamtester at a fixture directory under the repo. A test that mutates the host pam stack is a bug.
</rule>

---

## Phase 5: Inspect & Verify

Required checks before closing the task:

- [ ] `nm -D --defined-only target/release/libpam_syauth.so` lists only `pam_sm_*` symbols (no Rust mangled names leaked).
- [ ] `ldd target/release/libpam_syauth.so` resolves; no missing symbols.
- [ ] Every `pam_sm_*` has a panic boundary (grep for `catch_unwind`).
- [ ] Every PAM return path is logged at debug level with the originating reason.
- [ ] The default-deny path was exercised by at least one e2e test (peer offline → `PAM_AUTHINFO_UNAVAIL`).
- [ ] `make lint` is green.

---

## Phase 6: Document

Update `docs/pam.md` (create if missing) with:
- Service file snippet for an admin to wire syauth into `/etc/pam.d/login` or `/etc/pam.d/sudo`.
- Full module argument list (`debug`, `timeout=N`, `peer=...`).
- Return-code matrix from Phase 1.
- syslog facility and tag used.
- Rollback instructions (remove the line from the service file; the module is inert).

---

## Common Failure Modes

| Symptom | Likely cause |
|---------|--------------|
| `Authentication failure` with no syslog entry | Module crashed before syslog init; check that `openlog` is called before the first log call, and that the panic boundary returns a code. |
| Stack hangs for ~30s then denies | Blocking I/O without timeout — the BT call must have a deadline shorter than the PAM service's own timeout. |
| Works for one user, fails for another | `PAM_USER` not refreshed; you cached a username across calls into a `static`. PAM modules must be reentrant. |
| Login loops after success | `pam_sm_setcred` not implemented or returns `PAM_AUTH_ERR`; auth modules MUST implement both. |
| `dlopen` fails: `undefined symbol: pam_sm_authenticate` | Symbol was stripped or not `#[unsafe(no_mangle)]`; check with `nm -D`. |

---

<self_check>

Before closing the task:

- Does every `pam_sm_*` entry point have a `catch_unwind` boundary?
- Is the default branch of every input match a failure code (fail closed)?
- Is there at least one e2e test that runs against `pamtester` with a fixture PAM directory?
- Are tests prevented from mutating `/etc/pam.d`?
- Does the module log every return path at debug level?
- Is the peer-offline path covered by a test?

</self_check>

<rules>

1. Fail closed. Every unknown input maps to denial, not success.
2. Never panic across the FFI boundary. Wrap every entry point in `catch_unwind`.
3. Never test against the host's real PAM stack. Use a fixture directory.
4. Never use stdout/stderr inside a PAM module — use syslog.
5. PAM modules must be reentrant. No `static mut` global state.
6. `pam_sm_setcred` is not optional for auth modules; implement it even if it returns `PAM_SUCCESS` immediately.

</rules>
