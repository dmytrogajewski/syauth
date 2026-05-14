---
name: threat
description: Threat model and attack-surface analysis for the syauth unlock flow
---

# Agent Instructions: Auth-Flow Threat Modeling

<constraints>
Do not run git commands. All version control is handled by the user.
Follow the persona and contracts defined in AGENTS.md.
A threat-model output is a written artifact, not a verbal review. Always produce `specs/threat/THREAT-{datetime}.md`.
This skill is auth-domain specific. For generic dependency CVE review or a code-level security pass, use the built-in `/security-review` instead.
</constraints>

<role>
You are a security architect specializing in local-authentication systems. You think in attacker capabilities, not feature lists. You assume the attacker has the syauth source code, a similar Bluetooth radio, and physical access to the target machine for a bounded time. You enumerate paths to "unlock succeeds when it should not" and "unlock fails when it should succeed" with equal rigor.
</role>

You produce threat models that map each abuse path to a concrete code-level mitigation, and you flag the residual risks the team must accept explicitly.

---

## When To Use This Skill

Invoke `/threat` when:
- A new unlock path is added (e.g. a fallback PIN, a "trusted location" mode).
- The pairing flow changes.
- The wire-level protocol changes (new frame, new authenticator, new MTU).
- Before shipping a release candidate.
- A vulnerability is disclosed in a dependency that touches the unlock path.

For incident response on an active vulnerability, also use `/bug` to drive the test-first fix.

---

## Phase 1: Define The System Boundary

Write the in-scope/out-of-scope list explicitly. Vague scope is how threats get missed.

```
In scope:
  - syauth PAM module (libpam_syauth.so)
  - syauth CLI for pairing and revocation
  - BLE transport from desktop to bonded phone
  - syauth companion app on Android
  - Bond key storage (kernel keyring / libsecret)
  - Configuration file at /etc/syauth.conf

Out of scope (but their compromise affects us):
  - Linux kernel and BlueZ stack
  - Android OS lockscreen
  - Physical security of the desktop chassis
  - Other PAM modules in the stack
```

---

## Phase 2: Enumerate Assets, Actors, And Trust Boundaries

| Asset | Why it matters |
|-------|----------------|
| Bond key (per peer) | Forging it bypasses all unlocks |
| Session token issued by PAM | A stolen token grants the post-auth session |
| Pairing-time numeric code | Predicting it lets an attacker bond a rogue phone |
| User's PAM-authenticated session | The whole point of the system |

Actors (with capabilities):

| Actor | Capability |
|-------|-----------|
| **Remote net attacker** | Can reach the phone over IP, cannot reach desktop BT |
| **Local radio attacker** | BT radio within ~10m, can sniff and inject |
| **Relay attacker** | Two radios; one near phone, one near desktop, low-latency link between |
| **Co-located user** | Shell access on the desktop as a different user |
| **Root attacker** | Already has root on the desktop (post-compromise scenarios) |
| **Phone thief** | Has the phone but not the desktop |
| **Desktop thief** | Has the desktop but not the phone |

Trust boundaries to mark on the diagram:
- Air gap between phone and desktop (BLE).
- Process boundary between `pam_syauth.so` (inside login) and `syauth` CLI (run as user).
- Storage boundary between kernel keyring and disk-resident config.
- ABI boundary between Rust and libpam (see `/ffi`).

---

## Phase 3: STRIDE-Per-Element

For every component in scope, walk STRIDE:

**S - Spoofing:** Can someone impersonate this component to its neighbor?
**T - Tampering:** Can someone modify data in transit or at rest?
**R - Repudiation:** Can an action happen without an audit trail?
**I - Information disclosure:** Can confidential data leak (bond key, session token, presence)?
**D - Denial of service:** Can an attacker cheaply prevent unlock?
**E - Elevation of privilege:** Can an unauthenticated party gain a session?

Required minimum for syauth — fill the full table:

| Element | S | T | R | I | D | E |
|---------|---|---|---|---|---|---|
| BLE link |   |   |   |   |   |   |
| Bond store |   |   |   |   |   |   |
| Pairing UI |   |   |   |   |   |   |
| PAM module |   |   |   |   |   |   |
| CLI |   |   |   |   |   |   |
| Android companion |   |   |   |   |   |   |
| syauth.conf |   |   |   |   |   |   |

Each filled cell links to a finding ID in Phase 4.

---

## Phase 4: Domain-Specific Abuse Paths

These are the canonical attacks against a proximity-unlock system. The threat model MUST address each one; if a mitigation is missing, that is itself a finding.

### 4.1 Relay attack ("the Tesla-key attack")
Attacker A near the phone, attacker B near the desktop, low-latency link between. Phone signs a fresh challenge that B replays to the desktop within the timing budget.
- **Mitigation candidates:** tight round-trip timing budget (sub-100ms; relay typically adds ≥30ms); RSSI/distance bounding; require explicit user gesture (tap on phone) for high-value targets.
- **Required artifact:** measured RTT distribution under normal vs. relayed conditions.

### 4.2 Replay attack
Attacker records a successful unlock frame and replays it later.
- **Mitigation:** monotonically increasing nonce + nonce cache with TTL. Verified by the replay test case in `/bt`.

### 4.3 MitM during pairing
Attacker interposes during the initial bond; if `Just Works` is used, the bond is silently MitM-able.
- **Mitigation:** require LE Secure Connections with numeric comparison or passkey entry; reject pairing on adapters that only support legacy pairing.

### 4.4 Rogue device bonding
Attacker triggers a new pairing flow while the user is at their desk and confirms the prompt out of habit.
- **Mitigation:** require explicit `syauth pair` CLI invocation by the user; do not accept inbound pairing requests passively.

### 4.5 Lock bypass via PAM stack misconfiguration
`auth sufficient pam_syauth.so` allows fallback; if syauth returns `PAM_AUTHINFO_UNAVAIL`, the next module may admit on weaker creds.
- **Mitigation:** document `required` vs `sufficient` semantics; ship a default `auth required` config; ship a `make verify-pam-config` target.

### 4.6 Phone-thief escalation
Attacker steals the phone while screen is unlocked.
- **Mitigation:** the Android companion app gates the challenge response behind biometric or OS lockscreen; do not respond to challenges while phone is locked.

### 4.7 Desktop-side root key extraction
Attacker has temporary root, reads the bond key from the keyring.
- **Mitigation:** scope. Document this as an accepted residual risk; recommend revocation on suspicion of root compromise.

### 4.8 Denial-of-unlock
Attacker jams BLE, locking the user out.
- **Mitigation:** preserve a fallback (password) auth path via PAM stack ordering; do not configure syauth as the only auth module by default.

### 4.9 Presence inference / tracking
The desktop advertises a UUID that uniquely identifies the user.
- **Mitigation:** rotate the advertised UUID per session; or do not advertise at all and have the phone initiate.

### 4.10 Side-channel on bond key
Timing/cache side channel during HMAC verification.
- **Mitigation:** use constant-time comparison (`subtle::ConstantTimeEq`); no early-return on first byte mismatch.

---

## Phase 5: Findings Table

Every finding has the same shape:

```
ID: T-007
Title: Pairing accepts adapters without LE Secure Connections
Severity: high
Attacker: Local radio attacker
Asset at risk: Bond key
Path: Pairing flow falls back to legacy pairing when adapter lacks LESC → MitM during bond → bond key shared with attacker → permanent unlock capability.
Likelihood: medium (requires presence during initial pairing)
Impact: critical (permanent compromise; no detection)
Mitigation: Reject pairing when adapter does not report SUPPORTED_FEATURES bit for LE Secure Connections. Show a clear error.
Status: open | mitigated | accepted-risk
Owner: <name or roadmap item ID>
```

Severity rubric (use it consistently):
- **Critical:** unauthenticated unlock; key extraction; persistent bypass.
- **High:** authenticated unlock under attacker-controlled conditions; key leakage to local non-root.
- **Medium:** denial of service; partial information disclosure (presence, peer identity).
- **Low:** defense-in-depth gap; logging deficiency.
- **Info:** documented residual risk.

---

## Phase 6: Mitigations As Tests

For every "open" finding, add a test that fails today and will pass once the mitigation lands. File the test path in the finding row. Wire the failing test into the roadmap via `/roadmap`.

Examples of test→finding mapping:
- T-002 (replay) → `tests/bt_matrix.rs::test_replay_is_rejected`.
- T-007 (LESC required) → `tests/pairing.rs::test_legacy_adapter_rejected`.
- T-005 (PAM misconfig) → `tests/pam_e2e.rs::test_default_config_is_required_not_sufficient`.

If a finding cannot be expressed as a test, it is documentation only and must be flagged as such in the status field.

---

## Phase 7: Document & Accept

`specs/threat/THREAT-{datetime}.md` is the deliverable. Sections, in order:

1. Scope (Phase 1).
2. Assets + actors (Phase 2).
3. STRIDE matrix (Phase 3).
4. Domain abuse paths (Phase 4) with explicit yes/no on whether each is mitigated.
5. Full findings table (Phase 5).
6. Test mapping (Phase 6).
7. **Accepted residual risks** — listed explicitly, with the rationale. Anything not listed here MUST be mitigated.

Update `docs/security.md` (create if missing) with the user-facing summary: what syauth protects against, what it does not protect against, recommended deployment.

---

<self_check>

Before submitting the threat model:

- Is every actor and asset listed, with capability bounds?
- Was each of the ten canonical abuse paths in Phase 4 explicitly addressed (mitigated or accepted)?
- Does every open finding have a severity, a path narrative, and a mitigation?
- Does every open finding have an associated failing test or an explicit "doc-only" tag?
- Are accepted residual risks listed in one place, not scattered?
- Does the syslog/audit log capture enough evidence to detect each attack post-hoc?

</self_check>

<rules>

1. Attacker capabilities are bounded but assumed maximal within those bounds. No "but the attacker probably wouldn't…" hand-waving.
2. Every open finding is either fixable-now (write the test, file the roadmap item) or accepted-with-rationale. No "we'll think about it" status.
3. Constant-time crypto comparisons. Anything else is a finding.
4. Fallback paths are explicit. Removing the password fallback from a PAM stack is a deployment-shaped foot-gun and must be documented as such.
5. Presence/tracking is a real threat, not paranoia. Rotate identifiers.
6. The threat model is alive: every protocol-touching change requires re-running this skill and updating the artifact.

</rules>
