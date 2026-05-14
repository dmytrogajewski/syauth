---
name: generalize
description: Find reusable code and document generalization opportunities
---

# Agent Instructions: Generalize — Find Reusable Code

<role>
You are a code analysis agent. Your goal is to find all potentially reusable code and document it for generalization.
</role>

---

## Workflow (micro-iterations)

### 1. Iterate Over Every Source File (Except Tests)

Scan all `*.rs` files excluding test modules (`#[cfg(test)]` blocks and `tests/` directory).

### 2. For Each File, Analyze Every Function

For each function ask: **is it reusable?**

**Criteria:**


- **(a) Cross-crate potential** — it does something that could be needed in other crates if we generalize the function signature.
- **(b) Already generic** — it is already mostly generic; to make it fully generic we just have to add trait bounds or generic parameters.
- **(c) Decomposable** — it could be decomposed into smaller generic functions.
- **(d) Replaceable** — it could be removed entirely if another known function from `specs/ref/LIST.md` is used instead.

### 3. Take Action

| Finding | Action |
|---------|--------|
| **(a), (b), (c)** | Document into `specs/ref/LIST.md` |
| **(d)** | Add info into `specs/ref/LIST.md` near the corresponding function that could replace it |

**Update `specs/ref/LIST.md` after every file — do not batch**, because batching risks losing findings if the process is interrupted.

### 4. Build the Spec

After completing all files:

1. Read `specs/ref/LIST.md`.
2. Write `specs/ref/SPEC.md` — organize findings into **clusters by problem domain**.

---

## LIST.md Format

```markdown
## Dedup opportunities

1. {path/to/module}

   Function: {function name}
   Position: {file}:{line}:{col}
   Findings: {What did you find during analysis}
   Could replace:
     - path/to/module:999:99:FunctionName
     ...

...
```

---

## Progress Tracking Graph

Track your progress as a graph. Complete one movement at a time.

```
[ {pkg1} [x] ] -> [ {pkg2} [done] ] -> [ {pkg2.1} [skip] ]
                                     -> [ {pkg2.2} [skip] ]
               -> [ {pkg3} [next] ] -> [ {pkg3.1} [ ] ]
```

**Legend:**

| Symbol | Meaning |
|--------|---------|
| done | Completed or checked hypothesis |
| skip | Canceled |
| next | Current work |
| [ ] | Not started |

---

<self_check>

Before writing SPEC.md, verify:

- Are all findings backed by exact file paths and line numbers?
- Have you checked for false positives — is each finding genuinely reusable, not just a helper?
- Does LIST.md cover every source file (excluding tests)?

</self_check>

<rules>

1. **Micro-loops** — update `LIST.md` after every file, not at the end.
2. **One graph movement at a time** — complete the current file before moving on.
3. **Be specific** — include exact file paths, line numbers, and function signatures.
4. **Generics focus** — when criterion (b) applies, note which trait bounds and generic parameters would be needed.
5. **Only genuine reuse** — document genuinely reusable code, not every helper, because false positives waste implementation effort.

</rules>
