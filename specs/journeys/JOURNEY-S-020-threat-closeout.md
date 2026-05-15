# JOURNEY-S-020: Threat-model close-out + `/threat` artifact

<!-- Authored per .agents/skills/journey/SKILL.md template. -->

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md](../syauth/ROADMAP.md) — item **S-020**.
- Feature: produce `specs/threat/THREAT-2026-05-15.md` per `.agents/skills/threat/SKILL.md`
  Phase 1 through Phase 7, resolve every finding (mitigated with code+test
  citation, fixed-in-this-step with a failing-then-passing test, deferred to a
  named roadmap item, or accepted-residual with a rationale), and ship
  `docs/security.md` for end-users.

## 1. Journey

When **a syauth maintainer cutting v0.1.0 wants to prove the unlock flow
withstands the ten canonical proximity-unlock abuse paths** I want to
**run the `/threat` skill against the real wire-level code (not just the
spec), cite each mitigation by file+line+test, and explicitly accept any
residual risk** so I can **ship the release with an auditable contract
between the spec, the code, and the user-facing security document**.

## 2. CJM

SPEC §6 already enumerates ten canonical threats (T-001..T-010) and
sketches one-line mitigations. S-001..S-019 implemented those
mitigations — replay cache, constant-time MAC, LE Secure Connections
gate, rotating session UUID, biometric-gated Keystore signer, password
fallback in the PAM install helper, and so on. What S-020 does is
**close the loop**: it walks the SPEC table against the actual on-disk
code, cites the file path, line range, and test that pins each
mitigation, and writes the result into a single auditable artifact at
`specs/threat/THREAT-2026-05-15.md`. Anything the audit cannot cite
becomes either a roadmap follow-up or an explicitly accepted residual
risk. Nothing stays in `open` state.

The non-negotiables for this item:

1. **Every "mitigated" claim must cite a real file:line and a real test
   name.** Claiming "mitigated" without a citation is the failure mode
   `/threat` explicitly forbids.
2. **The threat doc is dated with today's ISO-8601 date so the artifact
   is a snapshot, not a moving target.** Future protocol-touching
   changes re-run `/threat` and produce a new dated file.
3. **`docs/security.md` is end-user prose, not a treatise.** Audience is
   a security-conscious Linux user evaluating syauth, not a
   cryptographer. Under 800 words, markdown only.
4. **Residual risks are documented in one place** (the threat-doc
   "Accepted residual risks" table). Anything that is not in that table
   and not in the mitigated table is a bug.

### Phase 1: Read the contracts

**User Intent:** Make sure the threat-closeout artifact matches the
`/threat` skill's Phase 1-7 contract and reflects what the SPEC
already claims is mitigated.

**Actions:**
1. Read `.agents/skills/threat/SKILL.md` end-to-end (Phase 1 to Phase
   7, self-check, rules).
2. Re-read `specs/syauth/SPEC.md` §3 (transport), §4 (auth flow), §5
   (storage), §6 (the T-001..T-010 table).
3. Re-read `AGENTS.md` for the no-`unwrap`, no-`TODO`, no-magic-numbers
   constraints that apply to any test the audit ends up filing.

**Pain / Risk:**
- Misnumbering threats: SPEC §6 uses T-001..T-010, `/threat` §4 uses
  4.1..4.10. Mismatching these is the first way an auditor stops
  believing the artifact.
- Confusing "accepted-residual" with "wontfix": residuals must carry an
  argued rationale, not just a shrug.
- Missing a mitigation that exists in code but not in the SPEC table
  (e.g. `BOND_FILE_MODE = 0o600` is not called out in SPEC §6 but is a
  real defense against T-007).

**Success Signal:** A note-list mapping each SKILL §4 path 4.x to the
SPEC §6 row T-00x, with deltas (additions, name drift) explicit.

### Phase 2: Walk each abuse path against the code

**User Intent:** For each canonical abuse path, locate the in-code
mitigation, name the test that exercises it, and assess what remains
residual.

**Actions:**
1. For each of the ten paths, open the file the mitigation lives in,
   find the line range, and run the test under
   `cargo test --workspace --all-features` so the citation is
   load-bearing, not aspirational.
2. Where the SPEC names a mitigation that does not have a
   corresponding test (e.g. PAM stack install-helper hard-codes
   `required` not `sufficient`), confirm by reading the constant,
   then cite a test that asserts the constant.
3. Where the mitigation is documentation-only (e.g. T-007 root-key
   extraction), mark it `accepted-residual` and write the rationale.

**Pain / Risk:**
- A test passes today but does not assert the security-relevant
  property the audit needs (false-citation risk). Mitigation: read the
  test body before citing, not just the name.
- A mitigation lives in two places (replay defense is in both
  `syauth-core::replay` AND `syauth-pam::auth::authenticate_inner`).
  Cite both; the layer that ships the constant-time guarantee is the
  audit anchor, the layer that wires it into the unlock path is the
  e2e anchor.
- Side-channel claims rest on a third-party crate's contract
  (`subtle::ConstantTimeEq`). Cite the crate by name; do not pretend
  we re-verified the constant-time property at the assembly level.

**Success Signal:** A row per path with (mitigation paragraph, file
range, test name, residual sentence). Empty rows = bugs that need
fixing or items that need filing.

### Phase 3: Identify genuine new findings

**User Intent:** Find anything the SPEC §6 audit didn't catch, classify
it, and either fix-in-this-commit or file-as-follow-up.

**Actions:**
1. Walk the four "actors" (local radio attacker, root-on-host, phone
   thief, desktop thief) against the seven in-scope components (BLE
   link, bond store, pairing UI, PAM module, CLI, Android companion,
   syauth.conf). For each cell, ask: "what new abuse path emerges
   from this pairing that SPEC §6 didn't list?"
2. Classify each new finding:
   - Trivially fixable in this commit → write a failing test in the
     affected crate, then fix the code, then check the test passes.
   - Bigger than one commit → file as a new step in
     `specs/syauth/ROADMAP.md` (next available step id) and link the
     finding to that id.
   - Real but unfixable in v0.1 → accepted-residual with rationale.
3. Confirm the inventory of unsafe-Rust call sites: the only place
   `unsafe` lives is the FFI boundary in `syauth-pam/src/entry.rs`
   (the `pam_sm_*` `extern "C"` symbols and the `catch_unwind`
   boundary). No silent `unsafe` outside of FFI per AGENTS.md.

**Pain / Risk:**
- Over-finding: every speculative threat dilutes the high-value ones.
  Constrain the new-findings list to plausible v0.1-shipping attacks.
- Under-fixing: deferring a trivial-to-fix issue ("rotate the
  rotation interval", "add a constant where there's a literal") is a
  smell. The DoD says fix-in-this-commit beats file-as-S-022.

**Success Signal:** A short table of new findings, each with one of
{fixed-in-this-commit, deferred-to-S-022, accepted-residual}.

### Phase 4: Write the artifacts

**User Intent:** Produce the dated threat doc, the user-facing
security doc, and the roadmap-evidence bullets.

**Actions:**
1. Write `specs/threat/THREAT-2026-05-15.md` with all `/threat`
   Phase 7 sections (Scope, Assets+Actors, STRIDE matrix, Domain
   abuse paths, Findings table, Test mapping, Accepted residual
   risks, Sign-off).
2. Write `docs/security.md` for end-users — protects/does-not-protect
   lists, operational hygiene checklist, v0.1→v0.2 pointers. Under
   800 words.
3. Tick the four DoD checkboxes in `specs/syauth/ROADMAP.md` under
   S-020, append an `### Evidence` subsection with one bullet per
   checkbox.
4. Remove any in_progress claim comment from the S-020 heading.

**Pain / Risk:**
- Length creep on `docs/security.md`. A treatise is worse than a
  honest list. 600-800 words is the budget.
- Emoji or shock-value framing in either doc — AGENTS.md forbids it
  and the audience does not need it.
- Drift between the threat doc's mitigation table and the actual
  file/line citations — if a line range shifts before review, the
  citation rots. Mitigation: cite by symbol name plus line range, so
  a reviewer can re-anchor.

**Success Signal:** `make lint` exits 0, `make test` exits 0, every
DoD box reads `[x]`, the threat-doc table cites a real test for every
mitigated row, the residual table carries a rationale for every accepted
row.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| SPEC §6 numbering vs `/threat` §4 numbering diverge | 1 | Maintain a side-by-side mapping in the threat doc itself |
| Mitigation lives across two crates (e.g. replay defense in core + pam) | 2 | Cite both lines; the audit reads one path top-to-bottom |
| End-users want a one-paragraph "should I install this" summary, not a STRIDE matrix | 4 | `docs/security.md` opens with that paragraph, then drills down |

### North Star Summary

A potential operator skims `docs/security.md`, decides syauth's
protected-against / not-protected-against tradeoff matches their
threat model, runs `syauth pair`, and never has to read the threat
doc. An auditor or security reviewer opens `specs/threat/THREAT-2026-05-15.md`,
walks the table top-to-bottom, clicks each file:line citation, and
agrees with every claim. Both flows exit happy.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] `docs/security.md` answers "should I install this" in the first paragraph.
- [x] The threat doc opens with a one-line summary of which abuse paths are
      mitigated.

### Onboarding Clarity
- [x] The threat doc cross-references SPEC §6 explicitly.
- [x] `docs/security.md` linkable from `README.md` (existing convention).

### Production-Ready Defaults
- [x] Password fallback is the documented default in `install_pam.rs`
      (`CONTROL_FLAG = "required"`).
- [x] Bond store mode is `0o600`; parent dir is `0o700` —
      `syauth-core::bond` enforces both.

### Golden Path Quality
- [x] Threat doc cites at least one passing test per mitigated row.

### Decision Load
- [x] `docs/security.md` enumerates a five-item operational hygiene
      checklist; no other decisions are pushed onto the user.

### Progressive Complexity
- [x] End-user docs do not require reading the threat doc.
- [x] Threat doc does not require reading the audit-time grep
      transcripts.

### Error Quality
- [x] Pair flow refuses non-LESC adapters with a named hint
      (`LESC_UNSUPPORTED_HINT`), pinned by
      `pair_rejects_when_adapter_lacks_lesc`.

### Failure Safety
- [x] PAM module returns `PAM_AUTHINFO_UNAVAIL` on transport
      failure (not `PAM_SUCCESS`), pinned by
      `tc02_offline_scenario_returns_authinfo_unavail_under_budget`.
- [x] Panic boundary in `run_entry` returns `PAM_AUTH_ERR`, pinned by
      `run_entry_catches_panic_and_returns_auth_err`.

### Runtime Transparency
- [x] Every PAM return path logs one `pam_syauth:` line via syslog
      LOG_AUTHPRIV.

### Debuggability
- [x] `last.log` appends one line per call; reason kebab-tokens
      pinned by test names.

### Cross-Surface Consistency
- [x] The SPEC §6 T-NNN ids are reused verbatim in the threat doc.

### Workflow Consistency
- [x] Threat doc lives at `specs/threat/THREAT-2026-05-15.md` per the
      `/threat` SKILL.

### Change Safety
- [x] No code edited under this step beyond named-constant additions
      and named-test additions (residual-driven).

### Experimentation Safety
- [x] No production constant changed in a way that flips a default;
      the only additions are tighter assertions, never relaxed.

### Interaction Latency
- [x] No change to hot paths; the threat doc is descriptive of
      already-shipping latency budgets.

### Developer Feedback Speed
- [x] `make lint` and `make test` cover every test the threat doc cites.

### Team Scale
- [x] Threat doc is checked in alongside specs; reviewable in the PR.

### System Scale
- [x] Numbering scheme (T-NNN) accommodates v0.2 additions.

### Right Behavior by Default
- [x] `auth required` is the install-helper default; the password
      fallback story relies on the operator keeping the next module
      in the stack.

### Anti-Bypass Design
- [x] The `--scripted-oob` CLI flag prints
      `SCRIPTED_OOB_WARNING` on every use so the bypass is never silent.
- [x] No "skip the safety gate" knob in production code paths.

## 4. Tests

### TC-01: Threat doc exists with every `/threat` Phase 7 section

**Given** the repository at this commit.
**When** an auditor opens `specs/threat/THREAT-2026-05-15.md`.
**Then** they see headings for Scope, Assets+Actors, STRIDE matrix,
Domain abuse paths, Findings, Test mapping, Accepted residual risks,
and Sign-off, in that order.

### TC-02: Every canonical abuse path is resolved

**Given** the threat doc.
**When** the auditor walks §4 path-by-path.
**Then** each row's status field is one of `mitigated` (with a
file:line+test citation) or `accepted-residual` (with a rationale).
No row is `open`.

### TC-03: Every mitigated row cites a real test

**Given** the threat doc's findings table.
**When** the auditor runs `cargo test -p <crate> <test_name>` for each
cited test.
**Then** every cited test passes.

### TC-04: `docs/security.md` ships and is under 800 words

**Given** the repository at this commit.
**When** the auditor opens `docs/security.md`.
**Then** they see a What-syauth-protects-against list, a What-it-does-NOT
list, an Operational-hygiene checklist, and a v0.1→v0.2 pointer, all
under an 800-word total budget.

### TC-05: New findings are either fixed or filed, never open

**Given** the threat doc's "new findings" section.
**When** the auditor checks each finding.
**Then** the finding maps to one of:
- a failing test that this commit landed AND fixed (status:
  fixed-in-this-commit),
- a roadmap step id with a follow-up commit (status: deferred-to-S-NNN),
- an accepted-residual rationale paragraph.
No finding stays `open`.

### TC-06: Build is green

**Given** the repository at this commit.
**When** the operator runs `make lint && make test`.
**Then** both exit 0.

## Traceability
- Roadmap item: [specs/syauth/ROADMAP.md S-020](../syauth/ROADMAP.md#step-s-020-threat-model-close-out--threat-artifact)
- Implementation files: `specs/threat/THREAT-2026-05-15.md`, `docs/security.md`
- Test files: every test cited in the threat doc lands in an existing
  test module — no new test files were required by S-020 itself.
