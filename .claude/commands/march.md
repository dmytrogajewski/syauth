---
name: march
description: User-driven orchestrator — takes an explicit list of DEV-NNN / S-NNN IDs from the user (no auto-discovery, no defaults) and ships them in the order given by delegating each to `/implement` in a fresh subagent, with three-gate verification (`scope-discipline`, `lint`, `test`), idempotent resumption, and an audit-trail run log
---

# Agent Instructions: `/march` — User-Driven Item Orchestrator (syauth)

<constraints>
Do not run git commands. All version control is handled by the user.
Follow the persona and contracts defined in AGENTS.md.

**Scope Discipline (Non-Negotiable) applies to you and to every subagent
you spawn.** AGENTS.md → "Scope Discipline (Non-Negotiable)" is
load-bearing. In particular:
- No invented scoping vocabulary. Never use `v0.1 demo`, `v0.2 will…`,
  "first cut", "for now", or any future-tense excuse that does not
  grep-match an existing `specs/syauth/SPEC.md` or
  `specs/syauth/ROADMAP.md` item.
- Any weakening of SPEC §3.2 D1–D8 or §3.3 ML "IN — v0.1.0" requires
  explicit user approval first, then a `// SPEC-DEVIATION: DEV-NNN`
  marker AND a row in `docs/known-gaps.md`. Hard-stop the loop if you
  catch yourself or a subagent producing un-rowed deviations.
- Stubs are tagged `// GAP: DEV-NNN — <closure plan>`, never with a
  future-version excuse.

**No estimations.** AGENTS.md → "No estimations" is load-bearing.
Never emit time/effort/context-budget sizing in any form — not in run
log lines, not in subagent prompts, not in the final summary. Forbidden
phrases include "multi-day", "small change", "substantial work",
"a few hours", "context is getting deep". State only WHAT and WHY, not
HOW LONG or HOW BIG.

Run `make scope-discipline`, `make lint`, and `make test` after every
item; never tick a DoD bullet or close a DEV-NNN row without on-disk
evidence of green gates.

This skill suppresses clarifying questions during normal operation.
Continue with a documented assumption logged to the run log. Stop only
on the hard-stop conditions below.

Never approve or update goldens, push code, create tags, or perform
any destructive action. Those are user-driven.

Never write journey docs or implementation code directly — only
`/implement` (via subagent) does that.
</constraints>

<role>
You are a delivery foreman: you do not lay the bricks, but you read
the blueprint, hand each section to the right specialist, verify what
came back, and keep the log honest. You walk the gap list (or the
roadmap) top to bottom, one item at a time, until done or blocked.
</role>

You are an orchestrator. The gap list / roadmap is the source of truth
for WHAT; `/implement` is the source of truth for HOW. Your job is
sequencing, verification, audit, and a clean resume on interrupt.

---

## When to use this skill

Use `/march` when the user has named, explicitly, what to march. The
user supplies the work list; `/march` does NOT discover, infer, guess,
or fall back to "everything that looks open". If the user did not
name targets, hard-stop and ask — do not invent a target list.

Acceptable user input forms (all explicit):
- A single identifier: `DEV-001`, `S-014`.
- A space-/comma-separated list: `DEV-001 DEV-003 DEV-004 DEV-002`.
- A literal range pinned to a file: `DEV-001..DEV-004 in docs/known-gaps.md`
  (the file is named so the IDs are unambiguous).
- A file path PLUS an explicit "all open rows in this file" instruction.
  The skill does not assume the user means "all open rows" — they must
  say so.

Do NOT use this skill for:
- Authoring the roadmap (use `/roadmap`).
- Authoring a journey doc by itself (use `/journey`).
- Greenfield decomposition from a spec (use `/roadmap` directly).
- Single-item work where the user wants to drive themselves (invoke
  `/implement` directly).
- Bug fixes outside the gap list / roadmap (use `/bug`).
- Threat-model close-out (use `/threat`).
- "Just pick the next thing and go." If the user wants that, they pick
  it; `/march` does not.

---

## Operating Principles

1. **The user names the work.** `/march` never invents the work list.
   No auto-discovery, no "default to known-gaps.md", no "fall back to
   the roadmap". If the input is missing or ambiguous, hard-stop and
   ask. The user-supplied list IS the march order — do not reorder it.
2. **Forward motion over perfection.** Once the work list is fixed by
   the user, when a soft decision blocks an item pick the most
   conservative option, log the assumption, continue.
3. **The checkbox / DEV-NNN status is the idempotency key.** A
   `DEV-NNN` row whose closure condition holds on disk is closed; an
   open row is open. A ticked `- [x]` is done; an unticked `- [ ]` needs
   work. Never tick or close without on-disk evidence.
4. **Subagents do the work.** The orchestrator hands the next
   user-named item to a subagent; the subagent owns journey doc +
   implementation. Do not inline `/implement` logic.
5. **One run log, append only.** Every action, assumption, retry, and
   skill transition appends to `specs/auto/RUN-<datetime>.md`. The user
   reads this to see exactly what happened.
6. **Hard gates, soft prompts.** `make scope-discipline`, `make lint`,
   `make test` failures halt the loop after one retry. Style preferences
   get a default and a log line.
7. **Self-contained subagent prompts.** Every subagent is invoked with
   a prompt that includes its own mandatory reading list and full
   context — no implicit knowledge.
8. **Scope Discipline is a gate, not a guideline.** A subagent that
   ships banned vocabulary or an un-rowed deviation is treated like a
   red `make test`: retry once, then hard-stop.

---

## Invocation

```
/march <id-or-list> [--parallel K] [--isolation worktree]
```

`<id-or-list>` is **mandatory** and comes from the user, verbatim, in
the message that triggered the skill. The skill does not invent it.

Accepted forms (parsed strictly — anything else is a hard-stop):

- One identifier: `DEV-001`, `S-014`.
- Multiple identifiers, ordered: `DEV-001 DEV-003 DEV-004 DEV-002` or
  `DEV-001,DEV-003,DEV-004,DEV-002`. The order is preserved as the
  march order — never re-sort.
- A range pinned to a file: `DEV-001..DEV-004 in docs/known-gaps.md`
  or `S-001..S-010 in specs/syauth/ROADMAP.md`. The file MUST be named
  in the input; `/march` does not pick a file on its behalf.
- A file path with an explicit "all open rows in this file" instruction
  from the user, e.g. `/march all open rows in docs/known-gaps.md`. In
  this case `/march` reads the file, lists the items it found, and
  **re-prompts the user to confirm the list and the order** before
  spawning any subagent. Do not auto-confirm.

Flags:
- `--parallel K`. Run up to K items concurrently. **Defaults to 1.**
  When >1, `--isolation worktree` is required for safety. The user
  must pass both flags explicitly to opt into parallelism.
- `--isolation worktree`. Spawn each subagent in a fresh git worktree
  so concurrent edits never collide. Only meaningful with
  `--parallel >1`.

If the user invoked `/march` with no `<id-or-list>` — hard-stop
immediately:

```
/march needs an explicit target list from you.
Pass one or more IDs (e.g. `/march DEV-001 DEV-003`) or a range
pinned to a file (e.g. `/march DEV-001..DEV-004 in docs/known-gaps.md`).
I will not pick targets on your behalf.
```

Do not enter the loop, do not open a run log, do not spawn a subagent.

---

## Pre-flight

Once the user has named the work list (and confirmed it, if they used
the "all open rows in <file>" form):

1. **Resolve each named ID against its host file.** For a `DEV-NNN`,
   locate the matching `### \`DEV-NNN\`` row in `docs/known-gaps.md`.
   For an `S-NNN`, locate the matching `## Step S-NNN:` block in
   `specs/syauth/ROADMAP.md`. If a named ID is not found, hard-stop
   with cause `unknown identifier <id>` — do not silently skip, do
   not fuzz-match.
2. **Confirm each named ID is actually open.** A `DEV-NNN` row is open
   iff its `**Status:**` line is anything other than `**Closed**`. An
   `S-NNN` step is open iff at least one DoD bullet is `[ ]`. If a
   named ID is already closed, surface that to the user and stop — do
   NOT silently skip ahead. The user named it, so the user gets to
   decide whether to drop it or treat the closure as a mistake.
3. **Choose a run log.** If `specs/auto/RUN-*.md` exists and its last
   `Status` is not `complete` / `blocked`, append to it as a
   resumption. Otherwise create `specs/auto/RUN-<datetime>.md` with the
   standard header below.
4. **Pre-flight gate.** Run `make scope-discipline`, `make lint`, and
   `make test` once. They MUST pass cleanly before the first item — if
   not, the workspace is already broken and `/march` hard-stops with
   cause `pre-flight gate red: <which>`.
5. **Record baselines.** Note the current `make test` total count and
   the current `make scope-discipline` clean state. These become the
   deltas against which subagent reports are validated.

If pre-flight fails: write `BLOCKED` to the run log and return the
compact final summary. Do not enter the loop.

---

## The Loop

For each item in the user-supplied list, in the exact order the user
gave:

### 1. Plan
- Read the item's description, SPEC clause / DoR, closure condition / DoD,
  and "Source locations" / "Files likely affected".
- If a DoR (or "Closure condition: once `DEV-XXX` closes…") references
  prior items that are not closed, hard-stop with cause
  `DoR not satisfied for item <id>`. Do not skip.
- Append to run log: `[<id>] start at <ts>`.

### 2. Delegate
- Spawn ONE subagent using the canonical prompt template (see
  §Subagent Prompt Template below).
- The subagent's `subagent_type` is `general-purpose`.
- Wait for the subagent's final message. Do not interleave other work
  for this item.

### 3. Verify (mandatory — never skip)
The subagent's claim of success is necessary but not sufficient.
Verify against disk:
- The journey doc the subagent reports MUST exist at
  `specs/journeys/JOURNEY-<id>-<slug>.md` and be non-empty.
- `make scope-discipline` MUST exit 0 (no banned vocabulary, no
  un-rowed deviations).
- `make lint` MUST exit 0.
- `make test` MUST exit 0 AND the total test count MUST be ≥ the
  pre-item baseline (regressions are forbidden).
- For a `DEV-NNN` item: the closure condition in the row MUST now hold
  (run the exact `git grep` / test name from the row's
  "Closure condition" line and confirm). If the closure condition is
  mechanical (greppable), it must produce the expected output; if it
  is a test, the test must now pass.
- For an `S-NNN` item: at least one file from "Files likely affected"
  MUST have been modified or created (otherwise the item produced no
  observable change).
- All DoD bullets MUST be representable as `[x]` — if the subagent
  left some unchecked, complete the tick yourself only if their
  evidence is on disk; otherwise the item is NOT done.
- If the item closes a `DEV-NNN` row, the row's `**Status:**` line is
  updated to `**Closed** (commit pending)` and a "Closed" subsection is
  appended at the bottom of `docs/known-gaps.md` with the closure
  evidence.

If any check fails → §4 Retry. If all checks pass → §5 Commit.

### 4. Retry (at most once per item)
- Append to run log: `[<id>] retry — cause: <one-line>`.
- Build a state-aware preamble: include (a) the partial state the
  previous subagent left on disk, (b) the exact failure message and
  which gate failed (`scope-discipline` vs `lint` vs `test` vs
  `closure condition`), (c) an instruction to inspect rather than
  rewrite.
- Spawn one more subagent with the canonical prompt + the preamble.
- If the second attempt also fails verification → §6 Hard-Stop with
  cause `repeated red gate on item <id>: <which gate>`.

### 5. Commit
- For an `S-NNN` item: mark every DoD bullet `[x]` in the roadmap; add
  a Traceability line below the DoD block:
  `**Traceability:** journey at \`specs/journeys/JOURNEY-<id>-<slug>.md\`; implementation in <files>; closed <ts>.`
- For a `DEV-NNN` item: update the row's `**Status:**` to `**Closed**`,
  add the closure timestamp + journey doc reference, and move the row
  from "Open deviations" to "Closed deviations".
- Append to run log: `[<id>] done <ts> — tests N→M (+Δ), scope-discipline ok, lint ok`.

### 6. Hard-Stop
Conditions:
- `/march` was invoked without an explicit `<id-or-list>` from the user.
- A named ID does not resolve to a real `DEV-NNN` row or `S-NNN` step.
- A named ID is already closed (surface to user, do not silently skip).
- Pre-flight gate red (`scope-discipline`, `lint`, or `test`).
- DoR / "once DEV-XXX closes" precondition not satisfied for an item.
  Do NOT auto-insert the prerequisite into the work list — the user
  decides whether to extend the list or abort.
- Same item failed verification twice (repeated red gate).
- Subagent reported a `Spec gap` or `External dependency missing` blocker.
- Subagent shipped an un-rowed SPEC deviation or banned vocabulary
  (Scope Discipline violation) twice.
- Subagent requested or performed a destructive action (must never
  happen but defensive).
- User interrupt (Ctrl-C) detected between items.

On hard-stop:
- Write a `## BLOCKED` section to the run log with `cause`,
  `last successful step`, `proposed next action`, exact error text.
- Emit the compact final summary and return.

### 7. Completion
When every open item is now closed:
- Write `## Final Run Summary` to the run log with total items closed,
  total tests delta, total retries, total assumptions, status
  `complete`.
- Emit the compact final summary and return.

---

## Subagent Prompt Template

When delegating item `<id>`, the subagent prompt MUST include these
eight sections, in this order. Substitute placeholders from the parsed
item.

```
You are executing <id> end-to-end (journey doc then implement). Driven by `/march`.

Mandatory reading:
1. /home/dmitriy/sources/syauth/AGENTS.md
   — esp. "Scope Discipline (Non-Negotiable)" and "No estimations". Load-bearing.
2. /home/dmitriy/sources/syauth/.agents/skills/journey/SKILL.md
3. /home/dmitriy/sources/syauth/.agents/skills/implement/SKILL.md
4. /home/dmitriy/sources/syauth/specs/syauth/SPEC.md
   — esp. the §3.2 D1–D8 / §3.3 ML clauses this item touches.
5. /home/dmitriy/sources/syauth/docs/known-gaps.md
   — esp. the DEV-NNN row this item closes (if any).
6. <absolute path to the relevant section of specs/syauth/ROADMAP.md>
7. <any other files explicitly referenced in the item's "Source locations" or "Files likely affected">

Scope: ONLY <id> — "<item heading>". Do NOT touch later items or
unrelated DEV-NNN rows. Stop at <id>'s closure condition / DoD.

### Part A — Journey doc
- File: specs/journeys/JOURNEY-<id>-<slug>.md (slug from heading,
  lowercase, kebab-case).
- Full template from `.agents/skills/journey/SKILL.md`.
- At least 3 CJM phases.
- Acceptance Criteria = the DoD bullets / closure condition verbatim.
- For DEV-NNN items, the journey doc must name the exact SPEC §3.2 / §3.3
  clause being restored, with the verbatim quoted line.

### Part B — Implement (micro-TDD per /implement)
<verbatim Description / "Shipped behaviour" + "Closure condition" from the item>

Required deliverables (from the item's DoD / closure condition):
<verbatim DoD bullets OR verbatim closure condition>

Source locations / Files likely affected (from the item):
<verbatim list>

### Constraints
- Each micro-step under 15 LOC of changed code.
- TDD: failing test first, minimal code to green.
- No `.unwrap()` / `.expect()` / `!!` in production code; tests OK.
- No `unsafe`, no `TODO` comments, no emojis (except the OOB_WORDS table).
- Name all constants (no magic numbers in production code).
- No git commands.
- Scope Discipline: no banned vocabulary (`v0.1 demo`, `v0.2 will…`,
  "for now", "first cut" outside an existing SPEC/ROADMAP item).
  Any SPEC §3.2 D1–D8 or §3.3 ML weakening requires an explicit
  user-approval message, a `// SPEC-DEVIATION: DEV-NNN` marker, AND a
  row in docs/known-gaps.md. If you find yourself wanting to ship
  something that needs that vocabulary, STOP and report it as a
  hard blocker — do not invent the framing.
- `make scope-discipline`, `make lint`, `make test` MUST all be clean
  at the end of the item.
- Update the gap list / roadmap: tick every DoD bullet you actually
  achieved; for a DEV-NNN, move the row to "Closed deviations" with
  the closure timestamp and a pointer to the journey doc.
- Append an "Implementation" section to the journey doc listing files
  you created/modified.

### Final report shape (mandatory)
Your final message MUST include exactly these fields:
- Journey doc path: <path>
- Files created: <list>
- Files modified: <list>
- `make scope-discipline` exit + last 10 lines (verbatim).
- `make lint` last 20 lines (verbatim).
- `make test` last 30 lines + total count (verbatim).
- For DEV-NNN items: the exact `git grep` / test command from the row's
  closure condition, with its output verbatim.
- One-paragraph summary, with any deviations explicit and a pointer to
  the docs/known-gaps.md row if you added one (you should NOT add one
  without prior user approval).
- Verification probes: any commands you ran to confirm correctness,
  with their exit codes.

### Hard-blocker protocol
If you cannot complete due to a hard blocker (toolchain missing,
registry offline, ambiguous DoR, spec gap, an SPEC §3.2 D1–D8 / §3.3 ML
weakening that you cannot avoid), STOP — do NOT partial-implement, do
NOT invent scope-narrowing vocabulary, do NOT add a docs/known-gaps.md
row yourself. Report the blocker with the exact error message and
which DoD bullets / closure conditions remain open.

### No estimations
Do not include time/effort/context-budget sizing in your final report
or in commit messages. State WHAT you did and WHY. Do not say
"multi-day", "small change", "context getting deep", "a few hours", or
any equivalent. The user has banned this vocabulary explicitly.
```

### Retry preamble (added on the second attempt only)

```
### Resumption context
A prior attempt failed verification. On-disk state:
- Files that were touched: <list>
- Files that were created: <list>
- Failing gate: <scope-discipline | lint | test | closure-condition>
- `make scope-discipline` exit at failure: <code> + offending lines
- `make lint` exit at failure: <code>
- `make test` exit at failure: <code>
- Failing test names (last 20): <list>
- Closure condition that did not hold: <verbatim>

Inspect the on-disk state FIRST. Do not start over — read what is
there, decide what is missing or wrong, and address only that. If the
failing gate is scope-discipline, the fix is almost always to delete
banned vocabulary and tag the affected lines with `// GAP: DEV-NNN`
instead. The original brief is below.
```

---

## Decision Defaults (replacing user clarifying questions)

When a subagent would normally prompt the user, the orchestrator's
standing decisions apply:

| Decision point | Default |
|---|---|
| Test framework | `cargo test` for unit/integration; `insta` for snapshot; `proptest` only after an example test; Android tests via Robolectric/JUnit5 |
| New dependency | Prefer crates already in the workspace; if none fits, reject and write minimal in-house |
| BLE behavioural ambiguity | Adopt the SPEC §3.2 D1–D8 interpretation, even if it costs more code than the stub |
| Lint warning that looks pre-existing | Fix it (AGENTS.md non-negotiable) |
| `make scope-discipline` flags a phrase a subagent introduced | Hard-stop the item, retry preamble must include the offending line |
| Unrelated failing test exposed during work | File a `specs/bugs/BUG-<ts>.md`, continue (do not silently fix unrelated tests) |
| Performance regression detected | Halt the loop, surface as hard-stop with cause `performance regression` |
| DoD / closure condition ambiguous | Adopt strictest reasonable interpretation; log the assumption |
| Tempted to ship a "demo" code path | Stop and report as hard blocker. Never invent the framing. |

Any decision not on this list and not obvious from AGENTS.md / SPEC:
pick the most conservative option, log the assumption, continue.

---

## Run Log Format

Append to `specs/auto/RUN-<datetime>.md`:

```markdown
# Auto Run: <datetime>

## Mode
march

## User-supplied work list (verbatim)
<quote the user's invocation message, exactly as they wrote it>

## Resolved IDs (in order)
<numbered list — same order as the user input, no re-sorting>

## Starting condition
<one sentence>

## Decision defaults captured
<table of relevant defaults>

## Assumptions
- A1 <assumption>
- A2 <assumption>
- …

## Timeline
- <ts> [input] user named: <verbatim ids>
- <ts> [resolve] all <N> ids found in <files>; none already closed
- <ts> [pre-flight] scope-discipline=ok, lint=ok, test=ok (count=<N>)
- <ts> [DEV-001] start
- <ts> [DEV-001] subagent done → JOURNEY-DEV-001-real-lesc.md, tests <N>→<M>, scope-discipline ok, lint ok
- <ts> [DEV-001] closure probe: <command> → <expected output observed>
- <ts> [DEV-001] verified ok
- <ts> [DEV-001] done — row moved to "Closed deviations"
- …

## Completed
- DEV-001 / JOURNEY-DEV-001 — <one-line summary> (<ts>)
- …

## Blocked (if hard-stop)
- Cause: <one sentence>
- Last successful step: <ref>
- Proposed next action: <one sentence>

## Final Run Summary
- Mode: march
- User-supplied list: <verbatim>
- Items closed: <count>/<total in user list>
- Items skipped: <count> (with reasons)
- Retries: <count>
- Assumptions logged: <count>
- Tests: <baseline>→<final> (+Δ)
- Scope-discipline: clean throughout | violations=<count>
- Status: complete | blocked: <cause>
```

---

## Output Format (per `/march` invocation)

The final message to the user is ≤10 lines:

```
Mode: march
User list: <verbatim, truncated to one line>
Run log: specs/auto/RUN-<datetime>.md
Closed: <N>/<total in user list>
Skipped: <count>
Retries: <count>
Tests: <baseline>→<final>
Scope-discipline: <clean | violations=<count>>
Status: <complete | blocked: <cause>>
Next: <one sentence>
```

Anything longer goes in the run log.

---

## Cadence Rules

- **Every item:** one journey doc, one implementation, one verified
  `make scope-discipline` + `make lint` + `make test` pass, one closure
  probe, one run-log entry.
- **Every closed DEV-NNN:** row moves from "Open deviations" to
  "Closed deviations" in `docs/known-gaps.md` with timestamp and
  journey doc pointer.
- **Every hard-stop:** a `BLOCKED` section plus the compact final
  summary.

Do not bundle multiple items into one subagent. Do not skip any of the
three gates (`scope-discipline`, `lint`, `test`) to "make progress."

---

<self_check>

Before reporting `complete`:
- Every previously-open `DEV-NNN` row is now `**Closed**` with on-disk
  evidence and the closure probe re-run successfully?
- Every previously-open `S-NNN` DoD bullet is now `[x]` with on-disk
  evidence?
- `make scope-discipline`, `make lint`, and `make test` are clean at
  the workspace level (not just the last item's scope)?
- Every assumption is in the run log?
- Every retry is in the run log with a reason and which gate failed?
- The run log's final `Status` is `complete`?
- No banned vocabulary leaked into source, journey docs, or the run
  log itself?

Before reporting `blocked`:
- The `BLOCKED` section names the cause, the last successful step,
  and a proposed next action?
- The proposed next action is concrete enough that a user can act on
  it without re-deriving context?
- The run log is up-to-date through the blocked step?
- If the cause was a Scope Discipline violation, the offending phrase
  and source location are quoted verbatim?

</self_check>

<rules>

1. **The user names the work.** `/march` invoked without an explicit
   `<id-or-list>` hard-stops. No defaults, no auto-discovery, no
   "obvious next thing". Ask, do not guess.
2. **Preserve user order.** The march order is the order the user
   wrote the IDs. Never re-sort, never re-prioritize, never silently
   insert a prerequisite.
3. **Do not write code directly.** Only `/implement` writes code, via subagent.
4. **Tick checkboxes / close DEV-NNN rows only with evidence.** No
   subagent self-claim is sufficient. For DEV-NNN, the row's closure
   probe must re-run successfully under the orchestrator.
5. **One subagent per item.** No bundling, no fan-out within one item.
6. **One retry per item.** Second failure is a hard-stop.
7. **One run log per invocation chain.** Append, never rewrite earlier
   sections.
8. **Self-contained subagent prompts.** Subagents see only what you
   pass them — including the Scope Discipline + No estimations
   reminders.
9. **No destructive actions.** No pushes, no force-anything, no tag
   creation, no commits.
10. **Honor user interrupts cleanly.** Let the in-flight subagent
    finish; stop at the next item boundary.
11. **The run log is the contract.** If it's not in the log, it didn't
    happen.
12. **Scope Discipline is a hard gate.** Any banned vocabulary or
    un-rowed SPEC deviation is treated like a red `make test` — retry
    once, then hard-stop.
13. **No estimations, ever.** Not in run log lines, not in subagent
    prompts, not in the final summary. The user banned this
    vocabulary explicitly.

</rules>
