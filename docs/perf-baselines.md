# syauth performance baselines (S-019)

Generated and appended-to by the e2e-real harness at
`tests/e2e_real.rs`. One section per SPEC §4.3 case. Each section is a
Markdown table; the test harness appends one row per run when
`SYAUTH_E2E_REAL_WRITE_BASELINES=1` is set.

## Recording protocol

- The histogram-collecting cases (`golden_case`, `offline_case`) run
  `E2E_RUN_COUNT` iterations (default 100). The first iteration is
  dropped as warmup; the remaining N-1 samples are sorted and reported
  as p50 / p95 / p99 using the nearest-rank method.
- Single-shot cases (every case other than `golden_case` /
  `offline_case`) record their elapsed time as both p50 and p95/p99 of
  a one-sample histogram. This keeps the row format uniform across all
  nine cases.
- The recording is **opt-in** via `SYAUTH_E2E_REAL_WRITE_BASELINES=1`.
  Default behaviour (`make e2e-real` with the gate on) is to assert
  the documented budget without writing — so two concurrent CI runs
  do not race on this file.
- The run id comes from `E2E_RUN_ID` if set, else from a hex of the
  current unix timestamp.

## Flake budget

**Flake budget is 0.** If a case flakes once on a CI host, file a bug
via `/bug` with the run log; either fix the underlying race or
quarantine the case via `#[ignore]` with a
`// QUARANTINED: <bug-id>` comment before merge. Quarantines are a
stop-the-line signal, not a maintenance pattern — every quarantine
ages out within one release cycle or the bug it points at gets
elevated to a roadmap item.

## Running e2e-real locally

```bash
# 1. Build the syauth APK once.
( cd syauth-android && ./gradlew :app:assembleDebug )

# 2. Boot the emulator + script the pair. This writes .env.e2e at the
#    repo root with SYAUTH_E2E_PEER_BOND_ID set.
./scripts/e2e-emulator-up.sh

# 3. Run the suite.
SYAUTH_E2E_REAL=1 make e2e-real

# 4. Optional: append a row per case to this file.
SYAUTH_E2E_REAL_WRITE_BASELINES=1 SYAUTH_E2E_REAL=1 make e2e-real

# 5. Tear down.
./scripts/e2e-emulator-down.sh
```

Prerequisites:

- `adb`, `emulator`, `avdmanager` on PATH (Android command-line tools).
- A pre-created AVD named `syauth_e2e` (system image API 34+
  recommended; an `android-34` `default/x86_64` image is the
  reference).
- BlueZ on the host with at least one adapter (`hci0`) reachable via
  DBus.
- The syauth debug APK built (step 1 above).

## SPEC §4.3 case matrix

| Case | What it asserts | Budget |
|------|-----------------|--------|
| `golden_case` | `AuthOutcome::Success` on the bonded peer | p95 < 2.0 s |
| `offline_case` | `AuthInfoUnavail{reason:"offline"}` when the phone is unreachable | p99 ≤ 1.2 s |
| `slow_case` | `AuthOutcome::Success` when the phone takes its time | p95 < 2.0 s |
| `replay_case` | `AuthErr{reason:"replay"}` when a prior response is resent | n/a |
| `bad_sig_case` | `AuthErr{reason:"bad-signature"}` when the phone's signature is corrupted | n/a |
| `wrong_version_case` | `AuthErr{reason:"wrong-version"}` when the version byte differs | n/a |
| `revoked_case` | `AuthInfoUnavail{reason:"no bonded peer"}` — radio is never touched | < 200 ms |
| `mtu_split_case` | `AuthOutcome::Success` when GATT MTU forces fragment reassembly | p95 < 2.0 s |
| `panic_in_core_case` | `AuthErr{reason:"panicked-in-core"}` — `catch_unwind` boundary intact | n/a |

## golden_case

| run_id | timestamp | p50_ms | p95_ms | p99_ms | duration_window_s |
|--------|-----------|--------|--------|--------|-------------------|
<!-- baseline-rows: golden_case -->
| placeholder | n/a | 0 | 0 | 0 | 0.0 |

## offline_case

| run_id | timestamp | p50_ms | p95_ms | p99_ms | duration_window_s |
|--------|-----------|--------|--------|--------|-------------------|
<!-- baseline-rows: offline_case -->
| placeholder | n/a | 0 | 0 | 0 | 0.0 |

## slow_case

| run_id | timestamp | p50_ms | p95_ms | p99_ms | duration_window_s |
|--------|-----------|--------|--------|--------|-------------------|
<!-- baseline-rows: slow_case -->
| placeholder | n/a | 0 | 0 | 0 | 0.0 |

## replay_case

| run_id | timestamp | p50_ms | p95_ms | p99_ms | duration_window_s |
|--------|-----------|--------|--------|--------|-------------------|
<!-- baseline-rows: replay_case -->
| placeholder | n/a | 0 | 0 | 0 | 0.0 |

## bad_sig_case

| run_id | timestamp | p50_ms | p95_ms | p99_ms | duration_window_s |
|--------|-----------|--------|--------|--------|-------------------|
<!-- baseline-rows: bad_sig_case -->
| placeholder | n/a | 0 | 0 | 0 | 0.0 |

## wrong_version_case

| run_id | timestamp | p50_ms | p95_ms | p99_ms | duration_window_s |
|--------|-----------|--------|--------|--------|-------------------|
<!-- baseline-rows: wrong_version_case -->
| placeholder | n/a | 0 | 0 | 0 | 0.0 |

## revoked_case

| run_id | timestamp | p50_ms | p95_ms | p99_ms | duration_window_s |
|--------|-----------|--------|--------|--------|-------------------|
<!-- baseline-rows: revoked_case -->
| placeholder | n/a | 0 | 0 | 0 | 0.0 |

## mtu_split_case

| run_id | timestamp | p50_ms | p95_ms | p99_ms | duration_window_s |
|--------|-----------|--------|--------|--------|-------------------|
<!-- baseline-rows: mtu_split_case -->
| placeholder | n/a | 0 | 0 | 0 | 0.0 |

## panic_in_core_case

| run_id | timestamp | p50_ms | p95_ms | p99_ms | duration_window_s |
|--------|-----------|--------|--------|--------|-------------------|
<!-- baseline-rows: panic_in_core_case -->
| placeholder | n/a | 0 | 0 | 0 | 0.0 |
