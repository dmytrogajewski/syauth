//! S-019 integration tests: `scripts/e2e-unlock.sh` percentile math + gate.
//!
//! Journey: specs/journeys/JOURNEY-S-019-e2e-latency-gate.md
//! Roadmap: specs/unlock-proximity/ROADMAP.md item S-019.
//!
//! These tests shell out to the real `scripts/e2e-unlock.sh` with a
//! hermetic fixture: a synthetic audit-log file under `tempfile::tempdir()`,
//! `SYAUTH_REAL_RADIOS=1` (the gate is satisfied symbolically), and
//! `SYAUTH_E2E_PREPOPULATED=1` (the script skips the real
//! `pamtester` loop and reads the existing audit-log tail). The
//! production code path (real-radio run) ignores
//! `SYAUTH_E2E_PREPOPULATED` and uses the `START_LINES`/`END_LINES`
//! snapshot — the prepopulated mode exists only so the
//! percentile math + JSON shape + exit-code matrix are
//! CI-enforceable without real radios.
//!
//! Coverage matrix (mirrors JOURNEY-S-019 §4 test cases):
//!
//! | TC  | Scenario                                              | Expected |
//! |-----|-------------------------------------------------------|----------|
//! | 01  | elapsed = [1000, 1100, 1200] (under budget)           | exit 0   |
//! | 02  | elapsed = [1000, 1500, 2500] (p99 over)               | exit 1   |
//! | 03  | elapsed = [1600, 1700, 1800] (p50 over)               | exit 1   |
//! | 06  | preflight: pamtester missing                          | exit 2   |
//! | 07  | preflight: audit log missing                          | exit 2   |
//!
//! TC-04 (stub-pamtester failures) and TC-05 (Makefile gate) are
//! exercised at the Makefile level / by inspection; the
//! `SYAUTH_E2E_PREPOPULATED=1` fixture path does not invoke
//! `pamtester` so a per-iteration-failure scenario is moot here.
//! TC-08 (real-radio probe) requires hardware in hand and is
//! documented in the journey doc's Closure Appendix.

#![allow(clippy::expect_used)] // tests are allowed to expect()

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

/// Audit-log column separator (SPEC §3 #8). Mirrors
/// `crates/syauth-presenced/src/audit.rs::AUDIT_FIELD_SEPARATOR`.
const AUDIT_FIELD_SEPARATOR: &str = ",";

/// Fixed peer id used in every synthetic audit line (any non-empty
/// no-comma string works).
const FIXTURE_PEER_ID: &str = "abc123";

/// Fixed nonce hex (32 chars matches `2 * NONCE_LEN` from SPEC).
const FIXTURE_NONCE_HEX: &str = "00112233445566778899aabbccddeeff";

/// Outcome / reason strings used in synthetic lines. The script
/// only consumes column 6 (reason) to count timeouts; this fixture
/// uses "ok" so `n_timeouts = 0`.
const FIXTURE_OUTCOME: &str = "ok";
const FIXTURE_REASON: &str = "ok";

/// Locate the repo root by walking up from `CARGO_MANIFEST_DIR`
/// (`crates/syauth-cli`) two levels.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .to_path_buf()
}

/// Absolute path to the script under test.
fn script_path() -> PathBuf {
    repo_root().join("scripts/e2e-unlock.sh")
}

/// Write a synthetic audit-log file with one line per `elapsed_ms`.
/// `t_start_ms` starts at `1_700_000_000_000`; `t_end_ms = t_start_ms
/// + elapsed`. Returns the file path.
fn write_synthetic_audit(td: &TempDir, elapsed_ms: &[u64]) -> PathBuf {
    let path = td.path().join("last.log");
    let mut buf = String::new();
    let t0: u64 = 1_700_000_000_000;
    for (i, e) in elapsed_ms.iter().enumerate() {
        let t_start = t0 + (i as u64) * 1_000;
        let t_end = t_start + e;
        buf.push_str(&format!(
            "{peer}{sep}{nonce}{sep}{ts}{sep}{te}{sep}{outcome}{sep}{reason}\n",
            peer = FIXTURE_PEER_ID,
            nonce = FIXTURE_NONCE_HEX,
            ts = t_start,
            te = t_end,
            outcome = FIXTURE_OUTCOME,
            reason = FIXTURE_REASON,
            sep = AUDIT_FIELD_SEPARATOR,
        ));
    }
    fs::write(&path, buf).expect("write fixture audit");
    path
}

/// Run the script in prepopulated mode with the given audit-log
/// fixture. Returns (exit_code, stdout, stderr).
fn run_script_prepopulated(audit_log: &Path, iterations: usize) -> (i32, String, String) {
    let output = Command::new("bash")
        .arg(script_path())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("USER", "fixture-user")
        .env("SYAUTH_REAL_RADIOS", "1")
        .env("SYAUTH_E2E_ITERATIONS", iterations.to_string())
        .env("SYAUTH_AUDIT_LOG", audit_log)
        .env("SYAUTH_PAM_SERVICE", "fixture-service")
        .env("SYAUTH_PAM_USER", "fixture-user")
        .env("SYAUTH_E2E_PREPOPULATED", "1")
        .output()
        .expect("spawn bash + script");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8(output.stdout).expect("stdout utf8"),
        String::from_utf8(output.stderr).expect("stderr utf8"),
    )
}

/// Parse the script's single JSON line from stdout. Panics if stdout
/// is not exactly one JSON object on one line.
fn parse_json_line(stdout: &str) -> Value {
    let trimmed = stdout.trim();
    assert!(!trimmed.is_empty(), "expected one JSON line on stdout, got empty: {stdout:?}");
    let lines: Vec<&str> = trimmed.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "expected exactly one JSON line on stdout, got {} lines: {stdout:?}",
        lines.len()
    );
    serde_json::from_str(lines[0]).expect("parse JSON line")
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// TC-01: synthetic distribution under budget exits 0.
///
/// `elapsed = [1000, 1100, 1200]` → nearest-rank:
/// - p50 = rank ceil(0.50 * 3) = 2 → 1100
/// - p95 = rank ceil(0.95 * 3) = 3 → 1200
/// - p99 = rank ceil(0.99 * 3) = 3 → 1200
///
/// All under budget; n_failures=0; expect exit 0.
#[test]
fn tc01_under_budget_exits_zero() {
    let td = TempDir::new().expect("tempdir");
    let audit = write_synthetic_audit(&td, &[1000, 1100, 1200]);
    let (rc, stdout, _stderr) = run_script_prepopulated(&audit, 3);
    let json = parse_json_line(&stdout);
    assert_eq!(rc, 0, "exit code under budget must be 0; stdout={stdout:?}");
    assert_eq!(json["p50_ms"], 1100);
    assert_eq!(json["p95_ms"], 1200);
    assert_eq!(json["p99_ms"], 1200);
    assert_eq!(json["n_failures"], 0);
    assert_eq!(json["n_timeouts"], 0);
}

/// TC-02: p99 over budget exits 1.
///
/// `elapsed = [1000, 1500, 2500]` → p99 = 2500 > 2000 budget.
#[test]
fn tc02_p99_over_budget_exits_one() {
    let td = TempDir::new().expect("tempdir");
    let audit = write_synthetic_audit(&td, &[1000, 1500, 2500]);
    let (rc, stdout, _stderr) = run_script_prepopulated(&audit, 3);
    let json = parse_json_line(&stdout);
    assert_eq!(rc, 1, "exit code over p99 must be 1; stdout={stdout:?}");
    assert_eq!(json["p99_ms"], 2500);
    let p99 = json["p99_ms"].as_u64().expect("p99_ms is integer");
    assert!(p99 > 2000, "p99 must exceed budget; got {p99}");
}

/// TC-03: p50 over budget exits 1.
///
/// `elapsed = [1600, 1700, 1800]` → p50 = 1700 > 1500 budget; p99 =
/// 1800 < 2000 budget (so only the p50 path triggers the failure).
#[test]
fn tc03_p50_over_budget_exits_one() {
    let td = TempDir::new().expect("tempdir");
    let audit = write_synthetic_audit(&td, &[1600, 1700, 1800]);
    let (rc, stdout, _stderr) = run_script_prepopulated(&audit, 3);
    let json = parse_json_line(&stdout);
    assert_eq!(rc, 1, "exit code over p50 must be 1; stdout={stdout:?}");
    assert_eq!(json["p50_ms"], 1700);
    let p50 = json["p50_ms"].as_u64().expect("p50_ms is integer");
    let p99 = json["p99_ms"].as_u64().expect("p99_ms is integer");
    assert!(p50 > 1500, "p50 must exceed budget; got {p50}");
    assert!(p99 <= 2000, "p99 must NOT exceed budget; got {p99}");
}

/// TC-06: pre-flight: SYAUTH_REAL_RADIOS missing exits 2.
#[test]
fn tc06_preflight_missing_real_radios_exits_two() {
    let td = TempDir::new().expect("tempdir");
    let audit = write_synthetic_audit(&td, &[1000]);
    // Don't use the helper — we need to drop SYAUTH_REAL_RADIOS.
    let output = Command::new("bash")
        .arg(script_path())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("USER", "fixture-user")
        .env("SYAUTH_E2E_ITERATIONS", "1")
        .env("SYAUTH_AUDIT_LOG", &audit)
        .env("SYAUTH_PAM_USER", "fixture-user")
        .env("SYAUTH_E2E_PREPOPULATED", "1")
        .output()
        .expect("spawn bash + script");
    let rc = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(rc, 2, "exit code must be 2 on preflight fail; stderr={stderr:?}");
    assert!(stdout.trim().is_empty(), "no JSON on preflight fail; got: {stdout:?}");
    assert!(
        stderr.contains("SYAUTH_REAL_RADIOS=1"),
        "stderr must name the missing env var; got: {stderr:?}"
    );
}

/// TC-07: pre-flight: missing audit log exits 2.
#[test]
fn tc07_preflight_missing_audit_log_exits_two() {
    let td = TempDir::new().expect("tempdir");
    let missing = td.path().join("does-not-exist.log");
    let output = Command::new("bash")
        .arg(script_path())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("USER", "fixture-user")
        .env("SYAUTH_REAL_RADIOS", "1")
        .env("SYAUTH_E2E_ITERATIONS", "1")
        .env("SYAUTH_AUDIT_LOG", &missing)
        .env("SYAUTH_PAM_USER", "fixture-user")
        .env("SYAUTH_E2E_PREPOPULATED", "1")
        .output()
        .expect("spawn bash + script");
    let rc = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(rc, 2, "exit code must be 2 on preflight fail; stderr={stderr:?}");
    assert!(stdout.trim().is_empty(), "no JSON on preflight fail; got: {stdout:?}");
    assert!(stderr.contains("audit log"), "stderr must mention the audit log; got: {stderr:?}");
}
