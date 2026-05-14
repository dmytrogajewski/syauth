---
name: orchestrate
description: Drive a roadmap to completion by spawning /implement sub-agents per item, dependency-aware and resumable
---

# Agent Instructions: Roadmap Orchestrator

<constraints>
Do not run git commands. All version control is handled by the user.
Follow the persona and contracts defined in AGENTS.md.
This skill never edits code directly. Its only writes are to the roadmap (status updates, DoD evidence links) and to the task list. All code changes are produced by sub-agents.
Assume at most one orchestrator runs at a time per repository. Concurrent orchestrators are not supported in v1.
A sub-agent that fails MUST leave the roadmap item marked `in_progress` with a `blocked-reason:` annotation — never silently rollback to `pending`, because that would mask the failure on resume.
</constraints>

<role>
You are a build foreman. You read the roadmap, decide what is ready to work on next, dispatch independent sub-agents to do the work in isolated worktrees, and update the roadmap as items complete. You do not write code yourself. You do not redesign work. If an item is unclear, you surface the ambiguity to the user — you never guess.
</role>

You operate the roadmap as a directed acyclic graph of work items. You exploit parallelism where the graph permits and serialize where it does not. You treat the roadmap as the single source of truth: every status transition is reflected there before the orchestration loop continues.

---

## When To Use This Skill

Invoke `/orchestrate` when:
- A roadmap exists at `specs/{spec-name}/ROADMAP.md` and the user wants to drive it forward.
- Resuming work after a crash or after a fresh context window — re-running `/orchestrate` is the canonical "continue where we left off" entry point.
- Working through a backlog of well-defined items that don't need new design decisions.

Do **not** use `/orchestrate` when:
- The roadmap doesn't exist yet — use `/roadmap` first.
- The next item requires a design call — invoke `/researcher` or `/journey` first.
- A specific item is failing and needs root-cause diagnosis — use `/bug` directly.

---

## Inputs

Default roadmap path: `specs/*/ROADMAP.md` (the orchestrator picks the most recently modified if multiple exist; if ambiguous, asks the user).

Optional arguments parsed from `$ARGUMENTS`:

| Flag | Meaning | Default |
|------|---------|---------|
| `--roadmap <path>` | Override the roadmap location | autodetect |
| `--max-parallel <N>` | Max concurrent sub-agents | `2` |
| `--only <id>[,id...]` | Restrict to specific items (run them then stop) | unset |
| `--from <id>` | Start from this item (skip earlier completed) | unset |
| `--dry-run` | Print the plan, dispatch nothing | false |
| `--yes` | Skip the per-batch confirmation prompt | false |

---

## Phase 1: Parse The Roadmap

1. Read the roadmap file end-to-end.
2. For each item (section starting with `## Step S-NNN:`), extract:
   - `id` — `S-NNN`
   - `title` — text after the colon
   - `status` — one of `pending` | `in_progress` | `completed` | `blocked`. Determine by:
     - All DoD checkboxes ticked (`- [x]`) → `completed`
     - Any `<!-- status: in_progress -->` comment → `in_progress`
     - Any `<!-- status: blocked reason="..." -->` comment → `blocked`
     - Otherwise → `pending`
   - `dor` — the list of `S-NNN` IDs named in the DoR section (parse with a regex over the bullet list)
   - `dod_checkboxes` — count of `[ ]` vs `[x]` lines under the DoD section
   - `journey` — the journey slug declared in the `**Journey:**` field
   - `files_likely_affected` — the listed paths
3. Build the DAG: `id → set of dependency ids`.
4. Detect cycles. If a cycle exists, refuse to run and tell the user the cycle path.

<output_format>
```
Roadmap: specs/syauth/ROADMAP.md
Items: 21 total — 0 completed, 0 in_progress, 0 blocked, 21 pending
Ready now (DoR satisfied): S-001
Critical path: S-001 → S-002 → S-008 → S-009 → S-010 → S-018 → S-019 → S-020 → S-021
Parallelizable lanes once unblocked:
  - Desktop CLI lane: S-011, S-012, S-013
  - Android lane: S-014 → S-015 → {S-016, S-017} → S-018
```
</output_format>

---

## Phase 2: Plan The Next Batch

1. Find all items where `status == pending` AND every id in `dor` has `status == completed`. Call this set **ready**.
2. From `ready`, pick up to `--max-parallel` items, preferring:
   - Items on the critical path (compute by walking the DAG backward from the last item).
   - Items in **different** lanes (i.e. they share no transitive dependent), to maximize true parallelism.
   - Smaller items first when ties remain (rough proxy: fewer files in `files_likely_affected`).
3. For each chosen item, decide an isolation strategy:
   - **`worktree`** when running in parallel with another item that touches overlapping files (per `files_likely_affected`).
   - **None (in-place)** when the item is the only one in the batch.
4. Print the proposed batch to the user with the items, expected file paths, and isolation choice. If `--yes` is not set, ask for confirmation. If `--dry-run` is set, stop here.

---

## Phase 3: Dispatch

For each item in the approved batch:

1. **Claim the item** in the roadmap: add a `<!-- status: in_progress claimed-at: {ISO-8601} -->` HTML comment immediately under the `## Step S-NNN:` heading. This is a *single Edit* operation; do not batch claims across items because a partial batch leaves stale claims.
2. **Build the sub-agent prompt** using the template in §6 below. It must be entirely self-contained — the agent has none of your context.
3. **Spawn the Agent**:
   - `subagent_type: "claude"` (the catch-all agent that has all tools).
   - `description`: short, e.g. `"Implement S-007 — BtPeer trait + mock"`.
   - `prompt`: per §6 template.
   - `isolation`: `"worktree"` if parallel with another item touching shared paths; otherwise omit.
   - `run_in_background`: `true` when the batch has more than one item (so the harness notifies you on each completion).
4. **Send multiple Agent tool calls in a single response** when the batch has more than one item — that is what makes them run concurrently.

<rule>
Never dispatch a sub-agent with a non-self-contained prompt. The agent does not see your conversation. If your prompt says "do what we discussed" or "continue from before," it will fail.
</rule>

---

## Phase 4: Reconcile

When a sub-agent returns:

1. Read its result message. Look for:
   - An explicit "completed" claim.
   - A list of modified files.
   - A summary of which DoD checkboxes were satisfied.
2. **Verify**, do not trust:
   - Read the item's DoD section from the roadmap.
   - For each `[ ]` line the agent claims to have completed, sanity-check at least one piece of evidence (file exists, test name matches). Use Read/Bash for this. Do NOT re-run the full test suite — the sub-agent already did, and the orchestrator must stay cheap.
   - If a claim cannot be substantiated, transition the item to `blocked` with `reason="dod-not-substantiated: <which checkbox>"` and surface to the user.
3. **Update the roadmap**:
   - Replace `- [ ]` with `- [x]` for each substantiated DoD checkbox.
   - Append an `Evidence` subsection under the item with bullets like `- DoD #3: tests in crates/syauth-core/src/replay.rs:42` and `- Modified files: …`.
   - If all DoD checkboxes are ticked, remove the `<!-- status: in_progress … -->` claim comment. The item is now `completed` by virtue of having all checkboxes ticked.
   - If some checkboxes remain unticked but the sub-agent finished cleanly, leave the item `in_progress` with a `<!-- status: in_progress partial: true -->` annotation, and queue it for a follow-up pass.
4. **Failure case** — sub-agent returned with an error, a panic, or an unfinished state:
   - Transition to `blocked` with `reason="<short reason from agent>"`.
   - Append a `Blocker` subsection with the agent's summary, the modified files (if any), and a suggested next step (`/bug`, `/researcher`, `/threat`).
   - Surface the failure to the user before continuing.

<rule>
The roadmap is updated **after** verification, not optimistically before. If a sub-agent claims completion and the orchestrator can't verify, that is a failure — do not propagate the claim.
</rule>

---

## Phase 5: Loop

After all items in the current batch are reconciled:

1. Print a short status summary: items completed in this batch, items still in_progress, items now blocked.
2. Update the TaskList: mark completed tasks done, add new tasks for any newly-revealed work.
3. Return to **Phase 2** to plan the next batch.
4. Stop when one of:
   - No items are `ready` and none are `in_progress` → roadmap done or fully blocked.
   - The user-supplied `--only` set is exhausted.
   - A blocking failure requires user input.

---

## 6. Sub-Agent Prompt Template

Each sub-agent is given a self-contained brief. Use this template verbatim, filling in the `{{...}}` placeholders. **Do not abbreviate.** The agent has no context other than this prompt.

```text
You are implementing one roadmap item from the syauth project. You have full repo access and all tools.

# Project context

- Repository root: {{absolute path to repo root}}
- Project: syauth (Linux PAM module + Android companion app for phone-as-key unlock).
- Personality and non-negotiables: read AGENTS.md at the repo root FIRST. Honor every rule.
- Specification: specs/syauth/SPEC.md — read sections relevant to this item.
- Roadmap: specs/syauth/ROADMAP.md — read your own item plus its prerequisites' "Files likely affected" so you know the existing code shape.

# Your assignment

You are implementing exactly ONE item: **{{item_id}} — {{item_title}}**.

The full item text follows. Treat the DoD checklist as the contract — every box must be true before you stop.

---
{{verbatim copy of the roadmap item, including Description, DoR, DoD, Tests, Files likely affected, Journey}}
---

# Workflow you must follow

Follow the canonical working loop from AGENTS.md:

1. **Read** AGENTS.md and specs/syauth/SPEC.md (relevant sections only).
2. **Write the journey doc** at `specs/journeys/{{journey_slug}}.md` using `.agents/skills/journey/SKILL.md` as the template. Do this before any code.
3. **Re-read** the journey doc to align scope.
4. **Invoke the /implement skill** to drive TDD: write failing tests first, then minimal code, then refactor. Honor the micro-TDD rules (one behavior per loop, < 15 lines per step, named constants).
5. For PAM-related items, also follow `.agents/skills/pam/SKILL.md`. For FFI-related items, follow `.agents/skills/ffi/SKILL.md`. For BLE items, follow `.agents/skills/bt/SKILL.md`. Pick the relevant ones based on what the item touches.
6. Run `make lint` and `make test` after every meaningful change. Both must be green at completion.
7. Tick the DoD checkboxes in the roadmap as you go (replace `- [ ]` with `- [x]`). Add an `## Evidence` subsection under the item listing: (a) modified files with one-line purpose, (b) added test files with what they verify, (c) any deviation from the original DoD and why.

# Constraints

- Do not run git commands. Do not commit.
- No `unsafe` Rust outside the documented FFI boundary; if you must add one, write a `// SAFETY:` comment naming the invariant.
- No `unwrap()` / `expect()` in production code paths (tests are fine).
- No `TODO` comments. Implement or stop and ask.
- If you discover the item is ambiguous or its DoR is not actually met, STOP and report back. Do not invent the missing context.

# Termination criteria

Return when, and only when:
- Every DoD checkbox for {{item_id}} is `[x]` in the roadmap.
- `make lint` exits 0.
- `make test` exits 0 (excluding tests gated on env vars like `SYAUTH_E2E=1`).
- The journey doc exists at the named path.
- The Evidence subsection is filled in under the roadmap item.

If you cannot reach termination, return with a clear blocker description: what you tried, what failed, what you think the next step is. Do NOT mark the item completed in that case.
```

---

## 7. Roadmap Markup Used By This Skill

The orchestrator reads and writes specific markup. Sub-agents must produce it correctly.

**Status comment** (claim, partial, blocker):
```markdown
## Step S-007: BtPeer trait + in-process mock
<!-- status: in_progress claimed-at: 2026-05-15T10:23:00Z claimed-by: orchestrate -->
```
or
```markdown
<!-- status: blocked reason="bluer 0.17 missing peripheral role on kernel 5.10" -->
```

**Evidence subsection** (appended by the sub-agent, read by the orchestrator):
```markdown
### Evidence
- DoD #1: `crates/syauth-transport/src/mock.rs:1-180` (MockBtPeer)
- DoD #2: `crates/syauth-transport/src/error.rs:5` (TransportError)
- Tests: `crates/syauth-transport/src/mock.rs` test module — 7 cases
- make lint: green (commit-local)
- make test: 14 passed, 0 failed (timestamp)
```

**DoD checkboxes** — toggle `- [ ]` ↔ `- [x]` in place. Do not delete the original text.

---

## 8. Examples Of Common Decisions

**Q: Two items are both ready. Should I run them in parallel?**

If `files_likely_affected` is disjoint: yes, in a worktree each. If they overlap, run sequentially.

**Q: An item is "ready" but its sibling under a parallel lane is `in_progress`. Run it?**

Yes, if the sibling does not write to overlapping files. The roadmap's "lane" labels are advisory; the file-overlap check is what matters.

**Q: A sub-agent took 45 minutes and returned with 3 of 6 DoD boxes done.**

Reconcile what's done (mark those `[x]`, write Evidence for them). Leave the item `in_progress partial: true`. On the next loop, dispatch a fresh sub-agent with the same item — the prompt template will lead it to pick up the unchecked boxes.

**Q: Two sub-agents in parallel both claim to have created `Cargo.toml`.**

This is why the orchestrator uses `isolation: "worktree"` for any parallel pair that share root-level files. If you ever skip the worktree and a collision happens, the second sub-agent's edit overwrites the first; mark both items `blocked` and ask the user how to merge.

**Q: An item is `blocked`. Should I keep planning past it?**

Yes — work in other lanes continues. Only items whose `dor` set includes the blocked item are also stuck. Surface the blocked item to the user once per planning pass, then move on.

---

## 9. Safety Rules

1. **Never edit code from the orchestrator.** Your only writes are to the roadmap and TaskList.
2. **Never run `git`**. Sub-agents inherit the same rule from AGENTS.md.
3. **Never auto-`--yes` past a `blocked` item without telling the user.** Blockers exist for a reason.
4. **Never spawn a sub-agent without verifying its DoR is satisfied.** Even when `--from` or `--only` is used, refuse to start an item with unmet prerequisites.
5. **Never silently mutate a DoD checkbox.** Every toggle is paired with an Evidence bullet.
6. **Never delete a `<!-- status: ... -->` annotation without writing the next state.** State transitions must be explicit.
7. **Treat worktree paths returned by the Agent tool as the source of changes for that item.** When `isolation: "worktree"` is used, the agent works on a copy; the orchestrator user will see the path in the agent result and merge it manually (the orchestrator does not auto-merge worktrees, because that requires git).

---

<self_check>

Before dispatching any batch:

- Have you parsed every roadmap item, including the dependency edges from DoR sections?
- Is the chosen batch free of cyclic or unmet prerequisites?
- Are parallel items in separate worktrees if their `files_likely_affected` overlap at all?
- Is the sub-agent prompt fully self-contained — does it reference AGENTS.md and SPEC.md by path, embed the item text verbatim, and name the journey slug?
- Is `max-parallel` respected even after retries?

Before reconciling a completion:

- Did you read at least one piece of file evidence per DoD checkbox you're about to tick?
- Did you write an Evidence subsection before transitioning the item to completed?
- Did you update the TaskList?

</self_check>

<rules>

1. The roadmap is the database. Every status transition is a write to ROADMAP.md.
2. Sub-agent prompts are self-contained. No shared context.
3. Parallel items live in worktrees when their file sets overlap.
4. Verify before you trust. A sub-agent's "completed" claim is a hypothesis until evidence is checked.
5. Blockers are not failures of the orchestrator; they are signals to the user. Surface, do not retry indefinitely.
6. Stop on ambiguity. The orchestrator does not redesign work.
7. No git, no commits, no destructive operations. Ever.

</rules>
