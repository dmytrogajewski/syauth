---
name: ffi
description: Rust↔C/JNI FFI safety audit and unsafe-boundary review for syauth
---

# Agent Instructions: FFI Safety Audit

<constraints>
Do not run git commands. All version control is handled by the user.
Follow the persona and contracts defined in AGENTS.md.
Unsafe code is denied by default in syauth. Every `unsafe` block requires a `// SAFETY:` comment that names the invariant it relies on. Audits fail when SAFETY comments are missing or stale.
Run `make lint` before considering any step complete. Run `cargo +nightly miri test` on pure-Rust portions where possible.
</constraints>

<role>
You are a Rust FFI auditor. You read every `unsafe` block as if you were the borrow checker: you name the invariants the compiler can no longer see and prove the code upholds them. You understand `#[repr(C)]`, panic-safety across FFI, drop order at the boundary, and the difference between `*mut T`, `&mut T`, and `NonNull<T>`.
</role>

You produce audits, not patches by default. A patch only follows a finding when the fix is one-line and obvious. Larger fixes turn into roadmap items.

---

## When To Use This Skill

Invoke `/ffi` when:
- Adding a new `extern "C"` function (export or import).
- Crossing a JNI boundary to/from the Android companion app.
- Reviewing changes that contain `unsafe` blocks.
- Bumping a `*-sys` crate version.
- A symptom screams "memory corruption": flaky crashes, valgrind/asan reports, non-deterministic data tearing.

For PAM module-specific concerns, also run `/pam`.

---

## Phase 1: Catalog The Boundary

List every FFI surface in scope. For each surface record:

| Direction | Symbol | Header / Crate | repr | Ownership at boundary |
|-----------|--------|----------------|------|----------------------|
| export    | `pam_sm_authenticate` | `pam-sys` | `extern "C"` | caller owns handle |
| import    | `pam_get_item`        | `pam-sys` | `extern "C"` | callee borrows, output is callee-owned |
| import    | `bluez_dbus_call`     | `zbus` (safe wrapper) | n/a | n/a |
| export (JNI) | `Java_com_sy_..._challenge` | `jni` | `extern "system"` | JNI rules |

Skip rows where a vetted safe wrapper exists. Only `unsafe` rows need a full audit.

---

## Phase 2: The Per-Block Audit Checklist

For every `unsafe` block in the audit scope, walk through this checklist and write findings inline:

### A. Pointer validity
- [ ] Source of each raw pointer is documented (where it was created, by whom).
- [ ] Null check or `NonNull::new`? If skipped, justify (e.g. PAM guarantees non-null).
- [ ] Alignment is guaranteed by `repr(C)` or by the C ABI of the producer.
- [ ] No use-after-free path: the lifetime of the pointee outlives every dereference.

### B. Aliasing
- [ ] No `&mut T` exists while another `&T` or `&mut T` to the same allocation is live.
- [ ] No two `&mut T` derived from the same raw pointer concurrently.
- [ ] If the pointer crosses a callback, the callback cannot reenter and alias it.

### C. Ownership / Drop
- [ ] Who frees this memory? Producer or consumer?
- [ ] If Rust allocated and C will free: the allocator matches (`Box::into_raw` requires `Box::from_raw` to free, not `libc::free`).
- [ ] If C allocated and Rust copies: copy is complete before the C-owned region can be invalidated.
- [ ] `Drop` does not run twice (no `Box::from_raw` on a pointer also freed by C).

### D. repr & layout
- [ ] Every struct passed across the boundary is `#[repr(C)]` or `#[repr(transparent)]`.
- [ ] No `bool` in `repr(C)` structs going to C (use `u8`; Rust `bool` is technically not ABI-stable for non-0/1 inputs from C).
- [ ] No niches: `Option<NonNull<T>>` is fine; `Option<&T>` across `extern "C"` is fine; raw `Option<T>` of arbitrary `T` is not.
- [ ] Enums sent to C are `#[repr(C)]` or `#[repr(i32)]` with a fixed discriminant.

### E. Panic safety
- [ ] Every exported `extern "C" fn` body is wrapped in `std::panic::catch_unwind`.
- [ ] On caught panic, the function returns a sentinel error value — never zero/success.
- [ ] No `?` propagates through the boundary unwrapped — every `Result` is mapped to a return code before the FFI return.

### F. Encoding
- [ ] Every C string is decoded via `CStr::from_ptr(...).to_str()` with the `Utf8Error` mapped to a failure.
- [ ] Every Rust string passed to C is built via `CString::new(...)` (no interior NUL) and kept alive for the duration of the C call.
- [ ] Byte buffers cross the boundary with an explicit length, never NUL-terminated for non-text data.

### G. Threading
- [ ] If C can call back into Rust on a different thread, all captured state is `Send + Sync`.
- [ ] No `&'static` references to thread-local or per-call data leak into C.

### H. JNI-specific (when applicable)
- [ ] Local refs are released or wrapped in `AutoLocal` — no leaks across N JNI calls.
- [ ] Exceptions are checked (`env.exception_check()`) after every JNI call that can throw.
- [ ] `JNIEnv` is never retained beyond the call that received it (use `JavaVM::attach_current_thread` instead).

<output_format>
```
Finding F-001  severity=high  file=src/pam/auth.rs:42
unsafe { *response = make_response(prompt) };

A1 ✓  pointer comes from PAM, guaranteed non-null by spec
B1 ✗  response also written by conv() callback on same thread before this line
C1 ✗  Box::into_raw was used; PAM frees with libc::free → mismatched allocator
E1 ✗  no catch_unwind; a Rust panic in make_response unwinds into libpam

Recommendation: allocate response with libc::malloc + ptr::write; wrap caller in catch_unwind.
```
</output_format>

---

## Phase 3: Mechanical Checks

Run these even when you think the code is fine — they catch things review misses:

1. `cargo +nightly miri test --lib` on pure-Rust modules that touch raw pointers.
2. `cargo +nightly build -Z sanitizer=address` and run the e2e suite; check for ASan reports.
3. `cargo +nightly build -Z sanitizer=thread` if threading is involved.
4. `cbindgen --crate syauth --output target/syauth.h` and diff against the committed header; any divergence is a finding.
5. `nm -D --defined-only target/release/libpam_syauth.so | grep -v ' pam_sm_'` — exported symbols other than the PAM entry points are leaks; mark them `#[unsafe(no_mangle)] pub(crate)` or rename.
6. `objdump -h target/release/libpam_syauth.so | grep -E '\.eh_frame|\.gcc_except_table'` should be present — confirms unwind tables exist for `catch_unwind` to work.

---

## Phase 4: Document Findings

Write the audit to `specs/audits/FFI-{datetime}.md` with:

```markdown
# FFI Audit {datetime}

## Scope
- Crates: <list>
- Boundaries: <table from Phase 1>

## Findings
<one entry per finding, severity high|medium|low|info>

## Mechanical Checks
- miri: pass | fail (link)
- asan: pass | fail (link)
- cbindgen drift: yes | no

## Recommendations
- Inline fixes applied: <list of one-line fixes>
- Roadmap items proposed: <list>
```

Apply trivial one-line fixes inline; for anything larger, open a roadmap item via `/roadmap` and link it from the audit.

---

<self_check>

Before closing the audit:

- Did you list every FFI surface, not just the ones that changed?
- For every `unsafe` block in scope, did you walk all eight checklist sections?
- Did at least one mechanical check (miri or asan) run, with output captured?
- Is every finding actionable — does it name a file, a line, and a fix?
- Did you verify the `catch_unwind` boundary on every exported `extern "C" fn`?

</self_check>

<rules>

1. Every `unsafe` block has a `// SAFETY:` comment naming the invariant. Missing comments are findings.
2. Never unwind across `extern "C"`. `catch_unwind` at every export, full stop.
3. Allocator must match. Rust-allocated memory is Rust-freed; C-allocated is C-freed.
4. Ownership at the boundary is documented in prose, not inferred.
5. Prefer safe wrappers (`pam-bindings`, `zbus`, `jni::objects::JString`) over raw pointers — replace raw FFI with a wrapper when one exists.
6. A flaky FFI test is not flaky — it is undefined behavior surfacing. Treat it as P0.

</rules>
