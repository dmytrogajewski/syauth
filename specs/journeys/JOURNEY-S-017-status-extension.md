# JOURNEY-S-017: Extend `sy syauth status` (daemon liveness + per-peer metrics)

> **Spec anchors:**
>
> - `specs/unlock-proximity/SPEC.md` §3 Scope item 24 (verbatim):
>
>   > `syauth status` (existing subcommand) is extended to report:
>   > daemon liveness, count of bonded peers being advertised, time
>   > since last challenge, time since last connect by each peer.
>
>   S-017 owns the per-peer extension of the existing `syauth status`
>   subcommand. The daemon-side counter-part is the orchestrator's
>   `peers_snapshot()` inspection method, which feeds `Response::Status`
>   so the `syauth status` client can render one row per bonded peer
>   with `last_challenge`, `last_connect`, `current_session_uuid`, and
>   `in_flight_challenges` columns.
>
> - `specs/unlock-proximity/SPEC.md` §3 Decisions row "PAM ↔ daemon
>   transport" (verbatim):
>
>   > `${XDG_RUNTIME_DIR}/syauth/auth.sock` carries length-prefixed
>   > CBOR-encoded typed messages.
>
>   The `syauth status` client speaks the SAME wire format the PAM
>   module uses — `Request::Status` / `Response::Status` via
>   `read_frame_blocking` / `write_frame_blocking`. No new transport.
>
> - `specs/unlock-proximity/SPEC.md` §3 Decisions row "Rotating UUID
>   cadence" (verbatim):
>
>   > Per-minute, derived from `session_uuid_for(bond_key, minute)`.
>
>   The `current_session_uuid` column surfaces the orchestrator's
>   computed UUID for the wall-clock minute the snapshot was taken in,
>   so an operator can `bluetoothctl scan on | grep <uuid-short>` and
>   confirm advertising end-to-end.
>
> **Roadmap row:** `specs/unlock-proximity/ROADMAP.md` Step S-017.
>
> **Closure condition (verbatim from ROADMAP.md):**
>
> ```
> cargo test -p syauth-cli --test status_flow
> ```

## Roadmap Link
- Source roadmap: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-017.
- Feature: Extend the existing `syauth status` subcommand so it asks
  the daemon (over the same `${XDG_RUNTIME_DIR}/syauth/auth.sock` Unix
  socket the PAM module speaks) for per-peer liveness columns:
  `peer_id`, time since the daemon last issued a challenge for that
  peer, time since the daemon last acquired the per-peer challenge
  slot (the closest proxy the daemon owns to a "connect" event — see
  Note A below), the current rotating session UUID, and the count of
  in-flight challenges (0 or 1, since SPEC §3 scope item #7 pins the
  per-peer semaphore to one permit). Falls back to a single
  `daemon=down: <reason>` line when the socket is unreachable. The
  waybar pill in the `sy` repo's roadmap consumes this output via the
  `--json` flag.

  **Note A (load-bearing terminology):** The daemon does NOT track
  GATT connect events at the orchestrator layer; the only
  per-peer-keyed timestamp it owns is the per-peer challenge-slot
  acquisition (the `Semaphore(1)` permit grant from SPEC §3 item #7).
  We surface that timestamp as `last_connect` so the operator-facing
  column name matches SPEC §3 scope item #24 ("time since last
  connect"); the journey, the column doc-comment, and the renderer
  all clarify the semantics so dashboards do not interpret the
  column as a BLE-link-up event.

## 1. Journey

When **a syauth operator wants a one-glance liveness view of every
bonded peer the daemon currently serves** I want to **type `sy syauth
status` (or pin it to a waybar pill via `--json`, or run `--watch` on
the host while debugging a stuck unlock)** so I can **see, in one
greppable table, which peers are reachable, when each was last
challenged, what rotating UUID the daemon is currently advertising
for each peer, and whether a challenge is in flight right now —
without `strace`-ing the PAM module, tailing `last.log`, or running
`bluetoothctl` in a side terminal**.

## 2. CJM

The syauth operator runs `sudo` on this desktop dozens of times a
day. The PAM module unlocks if the bonded phone is in proximity and
responds to a fresh nonce within the SPEC §4.3 budget; if the phone
is offline, `sudo` falls through to FIDO within ~1.2 s. Today the
operator has no real-time visibility into what the daemon thinks
about each peer between unlocks: they can grep `last.log` for the
last attempted transaction, but the log records only completed
attempts (not the rotating UUID the daemon is advertising right
now, nor whether a challenge is currently in flight). S-016's
`syauth doctor` answers "is the chain green at this instant" (one
snapshot, exit 0); S-017's extended `syauth status` answers "what
is the daemon doing per peer right now" (one row per peer, with
optional `--watch` polling).

### Phase 1: Daemon up, one peer recently challenged

**User Intent:** Confirm the bonded phone is reachable and the
daemon is healthy before relying on a `sudo` unlock (e.g., before a
remote demo, or as the first step of a CI smoke run after `sy
syauth install-pam`).

**Actions:**

1. Operator runs `sy syauth status` in their interactive shell.
2. Reads the daemon-state header line: `daemon=up
   started_at=<RFC3339>`.
3. Reads the table header: `peer_id  last_challenge  last_connect
   uuid  in_flight`.
4. Reads a single row for the bonded phone: `peer_id=<32 hex>`,
   `last_challenge=3.2s ago`, `last_connect=3.2s ago`,
   `uuid=<short>`, `in_flight=0`.
5. Moves on, confident the daemon is alive and the phone responded
   to the most recent challenge.

**Pain / Risk:**

- The daemon could report `daemon=up` but emit zero peers, hiding a
  reload regression that dropped the bond from the orchestrator —
  the `peers_snapshot()` method MUST be source-of-truth for the
  live peer set (no separate cache that can drift). The integration
  test pins this by injecting a fake daemon that returns two
  hard-coded peers and asserts both peer_ids appear in stdout.
- The `last_challenge` column could render as a stale "N seconds
  ago" computed at the daemon side (so a slow socket round-trip
  pollutes the rendered duration) — the duration is computed
  CLIENT-SIDE from `Response::Status.started_at` and the daemon's
  per-peer `last_challenge_ms_ago` field is offset by the
  round-trip in milliseconds. The status renderer uses the
  daemon-reported `*_ms_ago` directly so the operator's wall-clock
  reading is whatever the daemon observed at frame-emit time.
- The `uuid` column could leak the full 36-char UUID and crowd the
  table, undoing the alignment work — the renderer truncates to
  the same `SHORT_UUID_HEX_LEN = 8`-char prefix the rotation audit
  line uses (orchestrator.rs `short_hex`), so a `journalctl -t
  syauth-presenced | grep uuid=<short>` lookup matches end-to-end.

**Success Signal:** Operator sees `daemon=up` plus exactly one row
per bonded peer; the `last_challenge` cell reads in human-friendly
seconds-ago (or `never`); the `in_flight` cell reads `0`.

### Phase 2: Daemon up, peer offline for 5 minutes

**User Intent:** Diagnose why `sudo` just fell through to FIDO.
Suspected cause: the bonded phone is out of BLE range or the
operator left it in the kitchen.

**Actions:**

1. Operator runs `sy syauth status --watch`.
2. The status table redraws once a second (ANSI clear + cursor home).
3. Reads the row for the bonded phone: `last_challenge=312s ago`,
   `last_connect=312s ago`, `in_flight=0`.
4. Recognises the 5-minute gap, picks up the phone, returns to the
   desk; `last_challenge` stays at 312s+ on the watch redraws.
5. Triggers a fresh `sudo whoami` from another shell; the next watch
   redraw shows `last_challenge=0.1s ago` and unlock succeeds.

**Pain / Risk:**

- `--watch` could spin at a hot rate (e.g., 10 Hz) and turn the
  status command into a daemon-load generator — the polling cadence
  is pinned to `WATCH_INTERVAL = Duration::from_secs(1)` (one second,
  per the prompt) and never changes at runtime.
- The `--watch` redraw could scroll past the operator's tmux
  scrollback every second and lose the SSH-from-laptop diagnostic
  trail — we clear the screen with `\x1b[2J\x1b[H` so the table
  is always in the same place; the operator can `Ctrl-C` to
  break out and inspect a frozen snapshot.
- The `--watch` loop could refuse to exit on `Ctrl-C` because
  stdin is `lock()`ed inside the renderer — the loop checks an
  AtomicBool toggled by a SIGINT handler at the top of every
  iteration, before issuing the next `Request::Status`.

**Success Signal:** Operator sees the `last_challenge` cell tick
upward once a second (offline path) or snap back to `0.1s ago`
within one watch tick after they trigger a fresh `sudo`.

### Phase 3: Daemon down — fallback to greppable `daemon=down` line

**User Intent:** Diagnose a stuck unlock when `sudo` falls through
to FIDO immediately AND `syauth doctor` is not yet installed (S-016
shipped the doctor but this operator's `sy` install predates the
S-016 binary). They want a single-line answer for an ops-channel
paste.

**Actions:**

1. Operator runs `sy syauth status`.
2. Reads exactly one line: `daemon=down: socket-missing`.
3. Recognises this is the same reason token the S-016 `doctor`
   surfaces; runs `systemctl --user start syauth-presenced.service`.
4. Re-runs `sy syauth status`; sees `daemon=up` plus the per-peer
   table.

**Pain / Risk:**

- The daemon-down path could swallow the `connect()` error and
  print an empty table, leaving the operator unsure whether the
  command crashed or the daemon is down — the renderer ALWAYS
  emits the `daemon=down: <reason>` line on a socket failure
  (never a silent zero-row table).
- The fallback reason token could differ between `status` and
  `doctor` (e.g., `socket-missing` vs `not-found`) — we re-use
  the SAME `connect_error_reason()` mapping the S-016 doctor
  uses, so dashboards that already alert on `daemon=down:
  socket-missing` from doctor work unchanged on status.
- The `--json` mode could emit nothing on daemon-down and break
  downstream parsers — `--json` emits a `{ "daemon": { "state":
  "down", "reason": "..." }, "peers": [], "started_at": null }`
  object so the schema is total over the daemon-down branch.

**Success Signal:** Operator sees the single `daemon=down:
<reason>` line, fixes the daemon, re-runs status, and immediately
sees the populated table.

### Phase 4: `--json` consumer — waybar pill

**User Intent:** A waybar pill in the operator's `sy` desktop
applet wants to render an icon: green when the daemon is up and
every peer's `last_challenge` is < 60 s, amber when at least one
peer is stale, red when the daemon is down. The pill's `exec`
script is `sy syauth status --json`; it parses the output every 5 s.

**Actions:**

1. The waybar `exec` script invokes `sy syauth status --json`.
2. The script parses the output as JSON, reads the `peers` array
   and the `daemon.state` token.
3. The pill icon updates to green / amber / red based on the
   parsed values; the tooltip shows the per-peer `last_challenge`
   column.

**Pain / Risk:**

- The `--json` output could embed RFC3339 timestamps the waybar
  applet has to re-parse — we keep the wire format simple:
  `last_challenge_ms_ago: Option<u64>`, `last_connect_ms_ago:
  Option<u64>`, `current_session_uuid: String` (full 36-char UUID
  on the JSON path so machine readers see the canonical form;
  the text renderer truncates).
- The `--json` output could change shape between releases and
  break the waybar pill — the schema is typed at the daemon side
  via `serde_json::to_string_pretty(&CliStatusReport)` and the
  CLI's `CliStatusReport` struct is the canonical contract;
  field renames require a `cargo insta accept` on the help
  snapshot AND an explicit changelog row.
- The JSON object could omit `daemon.state` entirely when the
  daemon is up and leave waybar guessing — we always emit
  `daemon.state = "up" | "down"`, with `down` carrying a
  `reason` field; up carries a `started_at_unix_seconds: u64`
  field.

**Success Signal:** waybar pill cycles green / amber / red every
5 s; the operator confirms by toggling
`systemctl --user stop syauth-presenced.service` and watching the
pill turn red within one poll cycle.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Operator has no real-time per-peer view between unlocks | All | Extended `status` prints one row per peer with last-challenge / last-connect / UUID / in-flight columns |
| `--watch` flag missing — operator polls by re-typing the command | Phase 2 | `WATCH_INTERVAL = 1 s` polling loop redraws via ANSI clear screen + cursor home |
| Daemon-down path could swallow the connect error and print nothing | Phase 3 | Fallback to a single `daemon=down: <reason>` line that mirrors the S-016 doctor's reason vocabulary |
| Waybar pill cannot consume the human prose surface | Phase 4 | `--json` emits a typed object with `peers: []`, `daemon: { state, reason?, started_at_unix_seconds? }` |
| `current_session_uuid` is hard to correlate with `journalctl` lines | Phase 1 | Text renderer truncates to the SAME 8-char short form `orchestrator::short_hex` uses for the rotation audit line |

### North Star Summary

A single `sy syauth status` run prints a daemon-state header plus
one row per bonded peer with `last_challenge`, `last_connect`,
`uuid`, and `in_flight` columns; `--watch` redraws once a second;
`--json` emits a typed object for tooling. Daemon-down falls back
to a single greppable `daemon=down: <reason>` line that re-uses
the S-016 doctor's reason vocabulary so dashboards alert
identically across the two subcommands. The orchestrator's
`peers_snapshot()` is source-of-truth for the live peer set, and
the `Response::Status` wire frame is the SAME the PAM module
already speaks.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] One `sy syauth status` run prints daemon state + every
      bonded peer's row in under one second on a healthy host.
- [x] No new sub-commands; the existing `status` surface is
      extended in place so muscle memory carries over.

### Onboarding Clarity
- [x] `--help` lists every column header so the operator knows
      what `last_challenge`, `last_connect`, `uuid`, `in_flight`
      mean before running the command.
- [x] The `daemon=down: <reason>` reason vocabulary is identical
      to the S-016 doctor's reason vocabulary; one mental model.

### Production-Ready Defaults
- [x] Defaults match the SPEC: socket at
      `${XDG_RUNTIME_DIR}/syauth/auth.sock`, bonds at
      `/var/lib/syauth/bonds.toml`, `last.log` at
      `/var/lib/syauth/last.log`.
- [x] No `--bond-dir` / `--socket` flags are required for the
      operator-typical case.

### Golden Path Quality
- [x] Phase 1 (happy path) prints `daemon=up` plus the per-peer
      table; integration test `reports_per_peer_liveness` pins it.
- [x] The daemon-state header line format is stable
      (`daemon=up started_at=<RFC3339>`).

### Decision Load
- [x] One subcommand, three optional flags (`--watch`, `--json`,
      `--socket`). No mode-toggles, no per-column enable/disable.

### Progressive Complexity
- [x] Default mode is the tabular text rendering for human
      consumption; `--json` is opt-in for tooling; `--watch` is
      opt-in for debugging.
- [x] No verbose / quiet modes — every probe always emits its
      row.

### Error Quality
- [x] `daemon=down: <reason>` names the reason explicitly
      (`socket-missing`, `connect-refused`, `frame-error`,
      `timeout`) so the operator can act without re-running with
      a `--debug` flag.
- [x] `--json` daemon-down branch is a total schema — `peers: []`,
      `daemon: { state: "down", reason: <token> }` — so downstream
      parsers do not need to special-case the failure path.

### Failure Safety
- [x] Status is read-only by contract: no writes anywhere; the
      operator can re-run between probes without state mutation.
- [x] `--json` mode parses cleanly even when the daemon is down;
      the schema is total over both variants.

### Runtime Transparency
- [x] One row per peer + one daemon-state header line; every probe's
      outcome is visible in the output.
- [x] No hidden state: the status command's only side effect is
      writes to stdout.

### Debuggability
- [x] Output is `grep`-friendly:
      `sy syauth status | grep daemon=` isolates the header line for
      dashboards.
- [x] `--json` emits the same data for structured consumers.

### Cross-Surface Consistency
- [x] Uses the same `DEFAULT_BONDS_FILE`, `DEFAULT_KEYS_DIR`, and
      reason-vocabulary the S-016 doctor uses.
- [x] The `--socket` override defaults match the S-016 doctor
      defaults: `${XDG_RUNTIME_DIR}/syauth/auth.sock` (or
      `/run/user/<uid>/syauth/auth.sock` when env is unset).

### Workflow Consistency
- [x] Subcommand placement mirrors `syauth doctor` (sibling),
      `syauth list`, `syauth pair`.
- [x] `--help` snapshot is committed alongside the other
      subcommand `--help` snapshots in `tests/snapshots/`.

### Change Safety
- [x] Status never writes to the host; the operator can re-run
      `--watch` freely.
- [x] `--json` output is a stable schema (typed via
      `serde_json::to_string_pretty(&CliStatusReport)`).

### Experimentation Safety
- [x] Status is safe to run in CI, on a stranger's machine, on a
      production host: no writes, no DBus permissions required.

### Interaction Latency
- [x] Wall-clock target: < 200 ms on a healthy host (one socket
      round-trip; the daemon answers Status from in-memory state).
- [x] `--watch` polls once a second; no faster.

### Developer Feedback Speed
- [x] Tests pin the two load-bearing flows:
      `reports_per_peer_liveness`, `falls_back_when_daemon_down`.
- [x] Snapshot test pins the `--help` surface; a clap regression
      surfaces as a snapshot diff.

### Team Scale
- [x] Snapshot files are committed alongside source; team-wide
      help surface is version-controlled.
- [x] `--json` output is a stable contract for ops tooling.

### System Scale
- [x] The status row set is bounded by the live peer count (per
      SPEC §3 scope item #3, the 100-peer cap); the table renders
      in O(N) memory.

### Right Behavior by Default
- [x] No `--ignore-daemon-down` flag; the daemon-down path always
      surfaces.
- [x] `--watch` exits cleanly on Ctrl-C (SIGINT handler installed
      via `ctrlc` crate or equivalent).

### Anti-Bypass Design
- [x] Per-peer rows cannot be silenced; every peer the daemon
      reports surfaces in the output.
- [x] The daemon-state token is computed from the socket round-trip,
      not from a caller-supplied flag.

## 4. Tests

### TC-01: `reports_per_peer_liveness`

**Given** a fake daemon listening on a tempdir Unix socket that
responds to `Request::Status` with a `Response::Status` carrying
two `PeerStatus` rows (peer_id `aaa...aaa` and `bbb...bbb`, each
with `seconds_since_last_challenge=Some(3)` and
`seconds_since_last_connect=Some(3)`).
**When** `syauth status --socket <tempdir>/auth.sock` runs.
**Then** stdout contains the literal token `daemon=up`, the
peer_id `aaa...aaa`, the peer_id `bbb...bbb`, and at least one
row showing the `last_challenge` column non-`never`.

### TC-02: `falls_back_when_daemon_down`

**Given** a `--socket` path under a tempdir that does not exist.
**When** `syauth status --socket <tempdir>/no-such.sock` runs.
**Then** stdout contains `daemon=down` and the reason token
`socket-missing`.

### TC-03: `status_help_snapshot`

**Given** the `syauth status --help` invocation.
**When** captured via `assert_cmd`.
**Then** `insta::assert_snapshot!` against
`tests/snapshots/cli__status_snapshot.snap` matches the committed
surface; new flags (`--watch`, `--json`, `--socket`) appear in
the snapshot so a clap-derived regression requires a conscious
`cargo insta accept` to land.

### TC-04: `peers_snapshot_returns_orchestrator_state`

**Given** an `Orchestrator` constructed with two bonds at
`Instant::now() + 60s` start.
**When** `orchestrator.peers_snapshot()` is awaited.
**Then** the returned `Vec<PeerStatus>` carries two rows, each
with `last_challenge=None`, `last_connect=None`, `in_flight=0`,
and a non-nil `current_session_uuid` for the wall-clock minute.

## Acceptance Criteria (verbatim from ROADMAP DoD)

- [x] `syauth status` reports per-peer columns when daemon is up.
- [x] `syauth status --watch` polls every 1 s and redraws.
- [x] `syauth status --json` emits typed JSON.
- [x] `crates/syauth-cli/tests/status_flow.rs::reports_per_peer_liveness`
      passes.
- [x] `crates/syauth-cli/tests/status_flow.rs::falls_back_when_daemon_down`
      passes.
- [x] `crates/syauth-cli/tests/snapshots/cli__status_snapshot.snap`
      updated + reviewed.
- [x] `make scope-discipline && make lint && make test` green.

## Implementation

**Modified production modules:**

- `crates/syauth-cli/src/status.rs` — added clap options `--socket`,
  `--watch`, `--json` to `StatusOpts`; added `DaemonProbeState`
  (Up { started_at, peers } / Down { reason }) and the
  `CliStatusReport` JSON shape; added `build_cli_report`,
  `default_socket_path`, `probe_daemon`, `connect_error_reason`,
  `write_daemon_section`, `write_json_report`, `format_rfc3339`,
  `format_ms_ago`, `short_uuid_hex` renderers; added
  `run_status_watch_loop`, `install_sigint_handler`,
  `wait_or_break` for the `--watch` cadence. Named constants:
  `WATCH_INTERVAL = Duration::from_secs(1)`,
  `WATCH_CLEAR_SCREEN = "\x1b[2J\x1b[H"`,
  `DAEMON_CONNECT_TIMEOUT = Duration::from_millis(50)`,
  `DAEMON_STATUS_READ_TIMEOUT = Duration::from_millis(200)`,
  `SHORT_UUID_HEX_LEN = 8`, `DEFAULT_SOCKET_BASENAME =
  "syauth/auth.sock"`, `WATCH_SLEEP_TICK = Duration::from_millis(50)`.
  The legacy 5-line labelled output ("adapter:", "adapter-state:",
  …, "last-unlock:") is preserved by `render_status_to` so the
  S-012 cli.rs assertions stay green; the new daemon section
  prints ABOVE the legacy section.
- `crates/syauth-presenced/src/rpc.rs` — re-shaped `PeerStatus`
  fields to match the S-017 contract: `peer_id`,
  `last_challenge_ms_ago: Option<u64>`,
  `last_connect_ms_ago: Option<u64>`,
  `current_session_uuid: uuid::Uuid`,
  `in_flight_challenges: u32`.
- `crates/syauth-presenced/src/orchestrator.rs` — added
  `PeerLiveness` field on `PeerEntry`, threaded through
  `lookup_peer` / `PeerState`; added `Orchestrator::peers_snapshot()`
  that returns `Vec<rpc::PeerStatus>` (source-of-truth for the
  live peer set, derived from the orchestrator's `BTreeMap`);
  added free helpers `stamp_liveness`, `ms_since`,
  `challenge_slot_in_flight`. Stamped both timestamps after
  per-peer semaphore permit acquisition in `issue_challenge` and
  `issue_challenge_with_nonce`.
- `crates/syauth-presenced/src/server.rs` — added
  `ServeConfig::started_at: Option<SystemTime>`; captured at
  daemon boot (`runtime::run` passes
  `Some(SystemTime::now())`); plumbed through `serve` →
  `spawn_connection` → `handle_connection` → `dispatch`; the
  `Request::Status` arm calls `orchestrator.peers_snapshot().await`
  and returns the captured `started_at`.
- `crates/syauth-presenced/src/runtime.rs` — single new field on
  the `ServeConfig` literal.
- `crates/syauth-cli/Cargo.toml` — added `signal-hook = "0.3"`
  production dep for the `--watch` SIGINT handler.

**New tests:**

- `crates/syauth-cli/tests/status_flow.rs` (3 cases):
  - `reports_per_peer_liveness` (TC-01, DoD #1) — fake Unix-socket
    daemon thread answers `Request::Status` with two `PeerStatus`
    rows; asserts the stdout contains `daemon=up` plus both
    `peer_id` tokens.
  - `falls_back_when_daemon_down` (TC-02, DoD #2) — non-existent
    socket; asserts the stdout contains `daemon=down` and the
    reason token `socket-missing`.
  - `json_mode_emits_typed_object` — `--json` parses to a
    `serde_json::Value::Object` with `daemon_socket` and `daemon`
    top-level keys plus a `daemon.state` token in `{up, down}`.
- `crates/syauth-presenced/tests/peers_snapshot.rs` (1 case):
  - `peers_snapshot_returns_one_row_per_bonded_peer` — two-bond
    orchestrator returns two rows; cold-start row has
    `in_flight_challenges=0`, `last_challenge_ms_ago=None`,
    `last_connect_ms_ago=None`, and a non-nil
    `current_session_uuid`.
- In-module unit tests added to `crates/syauth-cli/src/status.rs`:
  `watch_interval_is_one_second`, `format_ms_ago_renders_never_for_none`,
  `format_ms_ago_renders_one_decimal_seconds`,
  `short_uuid_hex_truncates_to_eight_chars`,
  `write_daemon_section_emits_down_token_on_daemon_down`,
  `connect_error_reason_maps_not_found_to_socket_missing`.
- Existing `crates/syauth-cli/tests/cli.rs`
  `status_help_snapshot` renamed to `status_snapshot` so the
  generated snapshot file is `cli__status_snapshot.snap` per the
  DoD; the prior `cli__status_help_snapshot.snap` is removed (it
  was orphaned by the rename and only carried the S-012 surface).

**Updated snapshot:**

- `crates/syauth-cli/tests/snapshots/cli__status_snapshot.snap`
  — pins the new `--socket`, `--watch`, `--json` help surface.

**Closure evidence:**

- `cargo test -p syauth-cli --test status_flow` — 3 passed, 0 failed
  (the verbatim closure-condition probe from ROADMAP).
- `cargo test -p syauth-cli --test doctor_flow` — 4 passed, 0 failed
  (S-016 regression-check).
- `cargo test -p syauth-presenced --test peers_snapshot` — 1 passed.
- `cargo test -p syauth-cli --test cli` — 18 passed
  (existing `status_prints_all_documented_fields` etc. green
  alongside the new `status_snapshot` test).
- `make scope-discipline` — exit 0 ("Scope-discipline grep clean.").
- `make lint` — exit 0 ("Linting complete").
- `make test` workspace totals: 412 passed, 0 failed, 8 ignored
  (the ignored set is the pre-existing radio-gated DEV-004 rows;
  no new ignored tests added by S-017).

## Traceability
- Roadmap item: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-017.
- Implementation files: `crates/syauth-cli/src/status.rs`,
  `crates/syauth-cli/Cargo.toml`,
  `crates/syauth-presenced/src/orchestrator.rs`,
  `crates/syauth-presenced/src/rpc.rs`,
  `crates/syauth-presenced/src/server.rs`,
  `crates/syauth-presenced/src/runtime.rs`.
- Test files:
  `crates/syauth-cli/tests/status_flow.rs` (new, 3 cases),
  `crates/syauth-presenced/tests/peers_snapshot.rs` (new, 1 case),
  `crates/syauth-cli/tests/cli.rs` (renamed `status_help_snapshot`
  to `status_snapshot`),
  `crates/syauth-cli/tests/snapshots/cli__status_snapshot.snap`
  (new — replaces orphaned `cli__status_help_snapshot.snap`).
