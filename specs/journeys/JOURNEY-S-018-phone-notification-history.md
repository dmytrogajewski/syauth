# JOURNEY-S-018: Phone-side challenge notification + audit history

> **Spec anchors:** `specs/unlock-proximity/SPEC.md` §3 Scope item #23
> (verbatim):
>
> > Phone `SyauthCompanionService` writes a notification per challenge
> > (suppressed if the operator dismisses; rate-limited to 1 per 5 s).
>
> §4.3 Observability — the phone-side per-challenge notification is
> the UX mirror of the desktop's `/var/lib/syauth/last.log`.
>
> **Roadmap row:** `specs/unlock-proximity/ROADMAP.md` Step S-018.
>
> **Closure condition (verbatim from ROADMAP.md):**
>
> ```
> ./gradlew :app:testDebugUnitTest --tests "*ChallengeNotificationTest*" --tests "*ChallengeHistoryTest*"
> ```

## Roadmap Link
- Source roadmap: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-018.
- Feature: phone-side UX-visible audit signal that mirrors the
  desktop's `last.log`. Posts one low-priority notification per
  challenge transaction (rate-limited to 1 per 5 s); appends every
  transaction to an on-disk JSONL history file
  (`app.filesDir/challenge_history.jsonl`); surfaces the last 50
  records on a new `HISTORY` route under `MainActivity`.

## 1. Journey

When **an Android user who has bonded their phone with their desktop
sees a `BiometricPrompt` flash on their lock screen, taps Approve (or
Cancel), and minutes later wonders "wait — which desktop just asked
for sudo, and when?"**, I want to **see a small history-tagged
notification land in the shade for every sudo my desktop sent (subject
to a 5-second cool-down so a flurry of sudos does not spam me), and
tap any one of them to open a tidy per-transaction list on the Home
route that shows the hostname, the short peer id, the outcome
(granted / denied / timed-out), and the time of the last 50
transactions**, so I can **trust that every sudo my desktop fired
was, in fact, MY sudo (a relayed or phished one would still show up
in the history but I would not recognise the desktop or the time),
catch a runaway sudo loop on a misbehaving desktop without having to
open a terminal, and have a UX-visible audit trail that mirrors the
desktop daemon's `/var/lib/syauth/last.log` — same data, phone-side**.

## 2. CJM

Before S-018, the phone is silent after the `BiometricPrompt`
dismisses. The desktop daemon's `/var/lib/syauth/last.log` records
every challenge transaction (peer_id, nonce_hex, outcome,
elapsed_ms — see SPEC §3 scope item #8) but the phone side has no
matching record at all. If a user wants to know whether their last
sudo was actually approved by them or by a relayed phantom, they
have to walk back to the desktop and `tail` the log. That defeats
the "the phone is the source of truth for unlock UX" framing of
SPEC §3.

S-018 closes that gap: every challenge transaction (success or
denied) appends one record to a tiny on-disk JSONL file under the
app's private `filesDir`, and (rate-limited to one per 5 seconds)
posts a low-priority notification on a dedicated channel
`syauth-challenge-history`. Tapping the notification deep-links
into the `HISTORY` route under `MainActivity` which renders the
last 50 records, descending by timestamp.

### JSONL deviation (vs. SPEC wording "Room table")

The roadmap DoD says "small Room table". The task spec explicitly
flags that Room is too heavy here (compileSdk / KSP gradle plugin
churn) and recommends a JSONL file on disk — one record per line —
as the substitute. We take that recommendation. The chosen path is
`${app.filesDir}/challenge_history.jsonl`, one
`ChallengeHistoryRecord` per line, append-only with truncate-to-200
on write (the display surface only ever reads the last 50; the
extra 150 lines of headroom is the bound on disk growth). This is
documented in `ChallengeHistoryDao.kt` and surfaced in the
"Deviations" subsection of this journey's Implementation section.

The SPEC's text "Room table" is **not load-bearing** — what is
load-bearing is the 50-row history surface, append-only semantics,
and the per-transaction record shape. JSONL preserves all three
without dragging in the KSP gradle plugin or the
`androidx.room:room-compiler` annotation processor.

### Wire-shape of `ChallengeHistoryRecord`

Fields (snake_case to match the desktop's `last.log` line shape):

- `id: String` — UUID v4 minted at record-creation time. Stable so a
  future reconciliation tool can correlate desktop ↔ phone records.
- `peer_id: String` — bond's MAC, full form (`AA:BB:CC:DD:EE:FF`).
- `peer_id_short: String` — last three octets (`DD:EE:FF`), matches
  the SPEC §9 Q2 short-peer-id formatter the activity surface
  already uses.
- `hostname: String` — bond's `hostName`, the same value the
  approval prompt surfaces.
- `outcome: String` — one of `granted` / `denied` / `timed-out`.
  Pinned to the same vocabulary the desktop's `last.log` uses so a
  future correlation tool can join the two by `outcome` directly.
- `timestamp_ms: Long` — epoch milliseconds of the record-creation
  moment. Used to sort descending in the history view.

### Notification rate-limit semantics (UI-only, NOT history-side)

The rate limiter is a single `AtomicLong lastPostMs`. If the
elapsed wall-clock since the last successful post is less than
`NOTIFICATION_RATE_LIMIT.toMillis()` (5 seconds), the dispatcher
SKIPS the visible post — but STILL appends the record to history.
The SPEC text says "rate-limited to 1 per 5 s" for the visible
UX surface; the history is the audit trail, so suppressing audit
entries on the rate-limit path would defeat the whole point.

### Phase 1: Challenge resolves — service appends + posts

**User Intent:** The user has tapped Approve or Cancel (or the
prompt timed out and the activity closed). They are not yet looking
at their phone — they want a passive audit trail.

**Actions:** None at the user level. At the system level:
`ChallengeApprovalActivity.writeResponseAndFinish(responseBytes)`
calls `responseSink.onResponse(peerId, responseBytes)` — and the
production `responseSink`, installed in
`MainActivity.installCompanionSeams`, AFTER calling
`PersistentGattClient.writeResponse(responseBytes)` (success or
fail), invokes `ChallengeNotificationDispatcher.dispatch(...)`
with the resolved hostname, short peer id, and outcome
(`granted` if `responseBytes != DENIED_FRAME_BYTES`, `denied`
otherwise). The dispatcher:

1. Appends a fresh `ChallengeHistoryRecord` via
   `ChallengeHistoryDao.insert(...)`.
2. Compares the current monotonic clock minus `lastPostMs` against
   `NOTIFICATION_RATE_LIMIT.toMillis()`. If less, skip post.
3. Else, build a low-priority notification with the hostname,
   short peer id, and outcome; post it on
   `NOTIFICATION_CHANNEL_HISTORY` with content-intent that
   deep-links to the `HISTORY` route.

**Pain / Risk:**
- If `responseSink.onResponse` were invoked BEFORE `writeResponse`
  returned, a failing GATT write would still book a `granted`
  record. The production wiring in `installCompanionSeams` keeps
  the order: write first, then dispatch, with the outcome derived
  from the response bytes (denied vs. granted is the bytes payload,
  not the write return). A `timed-out` outcome is reserved for a
  future hook from the daemon-side, not used in this step (no
  watchdog timer ships here).
- If the OS denies the notification permission (Android 13+
  POST_NOTIFICATIONS runtime grant), the post no-ops silently. The
  history append still happens, so the audit trail is intact even
  on a phone that has muted the channel.
- If the JSONL file grows unbounded over months of daily sudos
  (e.g. 100 sudos/day for 365 days = 36,500 lines = ~5 MiB), disk
  pressure becomes visible. The DAO truncates to the last 200
  records on every insert (we display 50; the extra 150 is the
  defensive bound). 200 records at ~150 bytes per line = ~30 KiB
  steady state.

**Success Signal:**
`ChallengeNotificationTest::posts_per_challenge` — invoke the
dispatcher once with a synthetic outcome, query
`ShadowNotificationManager.activeNotifications`, assert exactly
one notification posted on `NOTIFICATION_CHANNEL_HISTORY` with the
hostname and short peer id in its content text.

### Phase 2: Multiple challenges land — rate limiter kicks in

**User Intent:** The user has just sudo'd three times in quick
succession on the desktop. They expect the phone to surface ONE
heads-up — they already know they tapped Approve three times in
five seconds; spamming the shade with three identical
"granted by alex-desktop" pills would be noise, not signal.

**Actions:** Three calls to
`ChallengeNotificationDispatcher.dispatch(...)` within 4 seconds.

**Pain / Risk:**
- If the rate limiter were tied to wall-clock `System.currentTimeMillis()`
  the test could not advance the clock reliably (Robolectric ships
  `SystemClock` but not a sane `Instant` shim). The dispatcher
  takes an injectable `Clock` seam (`java.time.Clock`) so the test
  injects `Clock.fixed` and steps it forward by N seconds between
  dispatch calls. Production wires `Clock.systemUTC()`.
- If the rate-limiter forbade BOTH the notification AND the
  history append, the audit trail would lose entries during sudo
  storms. The implementation suppresses ONLY the notification —
  the history always grows.
- If the limiter were per-peer instead of global, a multi-bond
  deployment would let two desktops flood the shade at the same
  time. SPEC item #23's "1 per 5 s" is unqualified (global rate);
  the implementation matches.

**Success Signal:**
`ChallengeNotificationTest::rate_limited_to_one_per_five_seconds`
— call `dispatch` twice with the test clock at t=0s and t=4s;
assert one active notification. Advance the clock to t=6s, call a
third time; assert exactly two active notifications (the OS
collapses by id is sidestepped because each call uses a fresh
notification id). The DAO records all three inserts regardless.

### Phase 3: User taps the notification — HISTORY route renders

**User Intent:** The user pulls down the shade an hour later,
sees "syauth: granted by alex-desktop", and taps it to scroll
through their recent sudos.

**Actions:** Tap the notification. The OS dispatches the deep-link
intent (`HISTORY_ROUTE_INTENT_ACTION`,
`HISTORY_ROUTE_INTENT_SCHEME://history`) at `MainActivity`.
`parseHistoryDeepLink(intent)` returns `true`; the side-effect
`NavigateOnHistoryDeepLink` composable navigates to
`NavRoutes.HISTORY`. The route's body calls
`ChallengeHistoryDao.recent(HISTORY_DISPLAY_LIMIT)` once and
renders a `LazyColumn` of `HistoryRowCard` composables — one per
record — with the hostname, short peer id, outcome, and a
short-form timestamp.

**Pain / Risk:**
- If the deep-link parser collided with the S-014 approve deep
  link (both share `syauth` scheme), the activity could navigate
  to the wrong route. The history deep-link uses
  `HISTORY_ROUTE_INTENT_HOST = "history"`; approve uses `approve`.
  The parsers are mutually exclusive on the URI host.
- If the JSONL parser threw on a truncated file (e.g. last line
  half-written after a crash), the route would crash. The DAO
  wraps every line read in `runCatching` and skips malformed
  lines; the read path returns the parseable subset.
- If the route did not page the records (`LazyColumn` is the
  Compose primitive for that), rendering 50 rows would still be
  fine, but a future bump to 500 would jank. We use `LazyColumn`
  for forward compatibility.

**Success Signal:**
`ChallengeHistoryTest::renders_last_fifty` — insert 60 distinct
records via the DAO, call `dao.recent(HISTORY_DISPLAY_LIMIT)`,
assert exactly 50 records returned, all with descending
`timestamp_ms`. Assert the first record returned has the highest
`timestamp_ms` of all 60 inserts; the last returned has the 50th
highest.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| User does not know the channel can be muted | 1 | The notification long-press menu surfaces the channel; future enhancement to surface a "Mute audit channel" toggle in HISTORY route is out of S-018 scope |
| User wants to correlate phone history with desktop `last.log` | 3 | UUID v4 `id` field is stable per record; a future `syauth doctor` join is out of scope |
| User wants to filter history by outcome (only failures) | 3 | LazyColumn could carry a filter chip row; out of S-018 scope. Records carry the field; the surface is small |

### North Star Summary

The ideal end state: every sudo the desktop sends produces one
audit-trail record on the phone (always) and at most one heads-up
notification on the audit channel per 5-second window (visible
signal). Tapping any heads-up opens a tidy descending-time list of
the last 50 transactions. The user can verify "yes those were all
mine" at a glance; a phantom relayed sudo would still appear in
the list, and the user's reaction — "I did not do that one" —
is the human-layer detection signal that complements SPEC §7
T-Relay's per-unlock biometric tap.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] First sudo after install → first audit record on disk and
      one heads-up notification within the same dispatcher call.
- [x] HISTORY route renders < 50 records as a single Compose pass.

### Onboarding Clarity
- [x] Channel name `syauth challenge history` describes its
      purpose; description tells the user it is safe to mute.
- [x] Notification content text names the hostname + outcome so
      the user knows what they are looking at without tapping.

### Production-Ready Defaults
- [x] Channel is created at `IMPORTANCE_LOW` so it never overrides
      DND or pulls the user out of focus.
- [x] JSONL DAO truncates to the last 200 records on every
      insert; no unbounded growth.

### Golden Path Quality
- [x] Tap the notification → HISTORY route renders the last 50
      records, descending by timestamp.
- [x] Approve and Cancel paths both record + notify with the
      correct outcome label.

### Decision Load
- [x] No filters, no settings — the surface is read-only.

### Progressive Complexity
- [x] DAO API is two methods (`insert`, `recent(limit)`);
      dispatcher API is one method (`dispatch`).

### Error Quality
- [x] Malformed JSONL lines are skipped silently; the parseable
      subset is returned.
- [x] OS notification permission denied → silent no-op on the
      notification path; history still appends.

### Failure Safety
- [x] DAO writes are append-only; a crash mid-write at worst
      truncates one record.

### Runtime Transparency
- [x] Dispatcher logs "history record id=<uuid> outcome=<out>" on
      every dispatch.

### Debuggability
- [x] `adb shell run-as com.sy.syauth.android cat files/challenge_history.jsonl`
      yields the raw audit trail.

### Cross-Surface Consistency
- [x] `peer_id_short` formatting matches the S-014 short-peer-id
      formatter; outcome vocabulary matches the desktop's
      `last.log`.

### Workflow Consistency
- [x] DAO sits under `history/`, mirroring the existing `bg/` +
      `pair/` package layout.

### Change Safety
- [x] DAO + dispatcher carry no schema migration risk because the
      file is per-line records and old lines are tolerated.

### Experimentation Safety
- [x] Tests inject a `Clock` seam, a temp `filesDir`, and a fresh
      `NotificationManager` — every case is hermetic.

### Interaction Latency
- [x] HISTORY route reads ~30 KiB of JSONL in a single
      `produceState` block; sub-frame on any phone.

### Developer Feedback Speed
- [x] DAO `recent` is a one-liner the test asserts against
      directly.

### Team Scale
- [x] All constants live with their owners
      (`NOTIFICATION_CHANNEL_HISTORY` next to the dispatcher;
      `HISTORY_TABLE_NAME` next to the DAO).

### System Scale
- [x] Bounded by `MAX_HISTORY_FILE_RECORDS = 200`; ~30 KiB cap.

### Right Behavior by Default
- [x] Audit channel is muted-safe (`IMPORTANCE_LOW`).
- [x] History grows append-only with a hard truncate bound.

### Anti-Bypass Design
- [x] Rate limiter is a single global `AtomicLong`; no per-peer
      bypass exists.

## 4. Tests

### TC-01: `ChallengeNotificationTest::posts_per_challenge`

**Given** a fresh dispatcher constructed with a temp `filesDir` and
a fixed `Clock`.
**When** the dispatcher's `dispatch(hostname, peerId, outcome)` is
called once with a fixture record.
**Then** exactly one notification is present in
`ShadowNotificationManager.activeNotifications`, posted on channel
`NOTIFICATION_CHANNEL_HISTORY`; its content text contains the
hostname and the short peer id and the outcome label; and the DAO
records exactly one row at `recent(HISTORY_DISPLAY_LIMIT)`.

### TC-02: `ChallengeNotificationTest::rate_limited_to_one_per_five_seconds`

**Given** a dispatcher with a `MutableClock` seam starting at
`t=0`.
**When** `dispatch` is called at `t=0`, then at `t=4s`, then the
clock advances to `t=6s` and `dispatch` is called a third time.
**Then** the active-notifications list contains exactly two
notifications (the second call within the 5-s window is
suppressed); the DAO's `recent(HISTORY_DISPLAY_LIMIT)` returns
three records (audit always grows).

### TC-03: `ChallengeHistoryTest::renders_last_fifty`

**Given** an `app.filesDir`-backed DAO and a loop that inserts 60
records with strictly increasing `timestamp_ms`.
**When** `dao.recent(HISTORY_DISPLAY_LIMIT)` is called.
**Then** the returned list has exactly 50 records, ordered by
`timestamp_ms` descending; the first record's `timestamp_ms` is
the highest of the 60 inserts; the last record's `timestamp_ms`
is the 50th highest (i.e. the oldest of the displayed window).

## Traceability
- Roadmap item: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-018.
- Implementation files: see "Implementation" section below.
- Test files: see "Implementation" section below.

## Implementation

Closed 2026-05-18.

### Deviations

- **JSONL substitute for Room.** The roadmap text said "small Room
  table"; the task spec recommended a JSONL file on disk to avoid
  KSP/Room gradle plugin churn. We took the JSONL path. Path:
  `${app.filesDir}/challenge_history.jsonl`. One
  `ChallengeHistoryRecord` per line, append-only, truncate-to-200
  on every write. Documented at the `ChallengeHistoryDao` class
  doc and at the `HISTORY_TABLE_NAME` constant (the constant
  doubles as the file name: `challenge_history`).

### Files created

- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/history/ChallengeHistoryDao.kt`
  — `ChallengeHistoryRecord` data class with the six fields
  (`id`, `peer_id`, `peer_id_short`, `hostname`, `outcome`,
  `timestamp_ms`); `ChallengeHistoryDao` class with `insert` and
  `recent(limit)`; `HISTORY_TABLE_NAME = "challenge_history"`,
  `HISTORY_DISPLAY_LIMIT = 50`, `MAX_HISTORY_FILE_RECORDS = 200`,
  `HISTORY_FILE_EXTENSION = ".jsonl"` constants.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/ChallengeNotification.kt`
  — `NOTIFICATION_CHANNEL_HISTORY = "syauth-challenge-history"`,
  `NOTIFICATION_RATE_LIMIT = Duration.ofSeconds(5)`,
  `NOTIFICATION_CHANNEL_HISTORY_NAME`,
  `NOTIFICATION_CHANNEL_HISTORY_DESCRIPTION`; outcome label
  constants `HISTORY_OUTCOME_GRANTED = "granted"`,
  `HISTORY_OUTCOME_DENIED = "denied"`,
  `HISTORY_OUTCOME_TIMED_OUT = "timed-out"`; the
  `ChallengeNotificationDispatcher` class with the `Clock` seam,
  `AtomicLong lastPostMs` rate gate, and `dispatch(...)` method;
  `HISTORY_ROUTE_INTENT_ACTION` /
  `HISTORY_ROUTE_INTENT_SCHEME = "syauth"` /
  `HISTORY_ROUTE_INTENT_HOST = "history"` deep-link constants;
  `peerIdShort(peerId)` helper using the same last-three-octets
  formula as the S-014 short-peer-id surface.
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/ChallengeNotificationTest.kt`
  — TC-01 + TC-02 (Robolectric SDK 34).
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/history/ChallengeHistoryTest.kt`
  — TC-03 (pure JVM; no Android shadow needed because the DAO
  takes a `File`).

### Files modified

- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
  — adds `NavRoutes.HISTORY` route + `HistoryRoute` composable
  rendering `dao.recent(HISTORY_DISPLAY_LIMIT)` as a
  `LazyColumn`; adds `parseHistoryDeepLink(intent)` and
  `NavigateOnHistoryDeepLink` side-effect mirroring the existing
  approve deep-link pattern; wraps the production `responseSink`
  to ALSO call
  `ChallengeNotificationDispatcher.dispatch(...)` after
  `writeResponse` returns with the correct
  granted/denied outcome.
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/approve/ApproveViewModelTest.kt`
  — untouched.

### Closure verification

- `make scope-discipline` — clean.
- `make lint` — clean (Rust unchanged).
- `make test` — Rust crates unchanged; new Android tests added
  under `:app:testDebugUnitTest`.
- `:app:assembleDebug` — BUILD SUCCESSFUL.
- `:app:testDebugUnitTest` — all green.
- Closure-condition probe
  (`./gradlew :app:testDebugUnitTest --tests "*ChallengeNotificationTest*" --tests "*ChallengeHistoryTest*"`)
  — BUILD SUCCESSFUL.
