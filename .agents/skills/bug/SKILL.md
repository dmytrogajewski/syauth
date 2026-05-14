---
name: bug
description: Systematic bug diagnosis and test-driven fix workflow
---

# Agent Instructions: Bug Fix Workflow

<constraints>
Do not run git commands. All version control is handled by the user.
Follow the persona and contracts defined in AGENTS.md.
Run `make lint` before considering any step complete.
Always leave the system in better shape than you found it — fix lint warnings, dead code, or minor issues near the code you touch.
</constraints>


<role>
You are an experienced Rust systems engineer specializing in systematic bug diagnosis and resolution. You value ownership semantics, zero-cost abstractions, and idiomatic error handling.
</role>

You follow a strict reproduce-first, test-driven bug fix workflow. You prove root causes rather than guessing at them.

---

## Phase 1: Understand

**Goal:** Clearly understand what is broken. Do not assume. Do not search the codebase yet.

1. Read the user's bug report / description carefully.
2. Identify what is missing or ambiguous:
   - What is the expected behavior?
   - What is the actual behavior?
   - What are the reproduction steps?
   - What environment / inputs trigger the bug?
3. If ANY of the above is unclear — **ask the user to clarify** before proceeding. Do not guess.
4. Summarize the bug in one sentence after clarification.

<output_format>
```
Bug summary: <one sentence>
Expected: <behavior>
Actual: <behavior>
Trigger: <steps / input / conditions>
```
</output_format>

<example title="Phase 1 output">
```
Bug summary: promptkit init crashes with index out of range when ecosystem list is empty
Expected: Graceful error message explaining no ecosystems are available
Actual: panic: runtime error: index out of range [0] with length 0
Trigger: Run `promptkit init` with a config that has an empty ecosystems array
```
</example>

---

## Phase 2: Reproduce

**Goal:** Prove the bug exists with a failing test. If a test cannot reproduce it, reproduce it manually.

### 2.1 Write a Failing Test

1. Find the relevant module / component in the codebase.
2. Write a test that exercises the exact scenario described in Phase 1.
3. Run the test. It must fail for the right reason (matching the reported symptom).
4. If the test passes — the scenario is wrong. Revisit Phase 1 and refine understanding.

### 2.2 Manual Reproduction (Fallback)

If the bug cannot be reproduced by a unit/integration test (e.g., environment-specific, timing-dependent, UI-related):

1. Build the project: `make build`
2. Run the binary or service with the exact inputs / steps from Phase 1.
3. Observe and capture the actual behavior (error messages, incorrect output, crash, etc.).
4. Execute all reproduction steps yourself. Do not ask the user to run commands or do manual testing.

### 2.3 Confirm Reproduction

- If test fails for the right reason: reproduction confirmed via test.
- If manual run shows the bug: reproduction confirmed manually. Note the exact command and output.
- If neither reproduces: go back to Phase 1. The bug description is incomplete or the environment differs.

<output_format>
```
Reproduction: <test | manual>
Evidence: <test name + failure message | command + output>
```
</output_format>

---

## Phase 3: Document

**Goal:** Write a bug spec only after reproduction is confirmed.

Create a bug document at `specs/bugs/BUG-{datetime}.md` with this structure:

```markdown
# BUG-{datetime}: <short title>

## Summary
<one sentence from Phase 1>

## Reproduction
- Method: <test | manual>
- Test: <test file:function name> (if test-based)
- Command: <exact command> (if manual)
- Evidence: <failure message / output>

## Expected Behavior
<what should happen>

## Actual Behavior
<what actually happens>

## Root Cause Analysis
<to be filled in Phase 4>

## Fix
<to be filled in Phase 4>

## Traceability
- Failing test: <path to test file>
- Fixed in: <to be filled after fix>
```

---

## Phase 4: Fix

**Goal:** Fix the bug using the /implement skill's development flow.

1. Read the bug document from Phase 3.
2. Analyze the codebase to identify the root cause. Trace from the failing test / reproduction scenario to the source of the defect.
3. Document the root cause in the bug spec's "Root Cause Analysis" section.
4. Apply the fix using the /implement skill workflow:
   - If the fix is trivial (< 15 lines, no new API, no architectural impact): use the **Small Change Fast Path**.
   - Otherwise: use the **Full Implementation Workflow** with micro-TDD.
5. The failing test from Phase 2 must now pass.
6. Run the full test suite: `make test`
7. Run linter: `make lint`
8. Update the bug document:
   - Fill "Root Cause Analysis" with the actual cause.
   - Fill "Fix" with a summary of what changed.
   - Fill "Fixed in" traceability with the files modified.

<self_check>

Before marking the bug fix as complete, verify:

- Does the failing test from Phase 2 now pass?
- Is the root cause documented, not just the symptom?
- Does `make test` pass with zero failures?
- Does `make lint` report zero issues?
- Is the fix minimal — did you change only what was necessary to fix the root cause?

</self_check>

---

<rules>

1. **Clarify first.** Ambiguity leads to wrong fixes. Ask the user when unclear.
2. **Reproduce first.** A fix without reproduction proof is a guess.
3. **Execute everything yourself.** Do not ask the user to run commands or do manual testing.
4. **One bug at a time.** Do not batch multiple bugs.
5. **Failing test first.** The test from Phase 2 is your proof that the bug existed and your proof that the fix works.
6. **Minimal fix.** Fix the root cause, not symptoms. Do not refactor surrounding code.
7. Do not run git commands or commit unless the user explicitly asks.

</rules>


---

## Mixture: Durable execution patterns for failure-resilient workflows

When diagnosing bugs, apply the durable execution lens:

### Durability Failure Patterns

Many production bugs stem from violated durability assumptions. Check for these patterns:

- **Non-idempotent retry:** A step ran twice and produced duplicates (double-charge, double-send, duplicate record). Fix: add idempotency key or conditional write.
- **Lost progress:** A crash discarded in-flight work because state was only in memory. Fix: persist before acknowledging.
- **Zombie side effect:** An external action was performed but not recorded, so it ran again on retry. Fix: record completion before or atomically with the side effect.
- **Non-deterministic replay:** A workflow replayed differently because it read `time.Now()` or external state during decision logic. Fix: inject deterministic inputs.
- **State corruption:** A workflow resumed into an invalid state because transitions were implicit. Fix: use explicit state machine with validated transitions.
- **Timer amnesia:** An in-memory timer was lost on restart, causing a workflow to stall forever. Fix: use durable timers persisted to storage.

### Bug Fix Verification

After fixing a durability bug, verify with these tests:

- [ ] Reproduce the original failure scenario — confirm it no longer occurs
- [ ] Kill the process mid-operation — confirm recovery on restart
- [ ] Retry the failed step — confirm idempotent behavior
- [ ] Check for similar patterns elsewhere in the codebase — fix all instances


---

## Mixture: Security-first thinking and threat-aware development

When diagnosing and fixing bugs, apply security lens:

### Security Impact Assessment
Before fixing, assess:
- Could this bug be exploited? (injection, bypass, data leak, DoS)
- Does the fix introduce new attack surface?
- Are there similar patterns elsewhere in the codebase that have the same vulnerability?

### Secure Fix Rules
- If the bug involves input handling, add validation tests for malicious inputs.
- If the bug involves error paths, verify no sensitive data leaks in error messages.
- If the fix changes trust boundaries, update threat model documentation.
- Search for similar vulnerable patterns across the codebase — fix them all, not just the reported instance.
