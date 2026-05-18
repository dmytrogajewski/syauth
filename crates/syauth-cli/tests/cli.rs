//! S-012 integration tests: `syauth list`, `revoke`, `status`, `--version`,
//! `--help` snapshots.
//!
//! Every test is hermetic — `--bond-dir` (and `--last-log` where applicable)
//! is rooted in a `tempfile::TempDir`. The real `/var/lib/syauth/` is never
//! touched. The integration runs the built `syauth` binary via `assert_cmd`
//! so we exercise the exact same code path the operator would.
//!
//! Coverage matrix (every row maps to a DoD line in S-012):
//!
//! | TC  | DoD | Scenario                                                       |
//! |-----|-----|----------------------------------------------------------------|
//! | 01  | #4  | `--version` prints semver, exits 0.                            |
//! | 02  | #5  | Top-level `--help` snapshot pinned.                            |
//! | 03  | #5  | `pair --help` snapshot.                                        |
//! | 04  | #5  | `list --help` snapshot.                                        |
//! | 05  | #5  | `revoke --help` snapshot.                                      |
//! | 06  | #5  | `status --help` snapshot.                                      |
//! | 07  | #5  | `install-pam --help` snapshot.                                 |
//! | 08  | #5  | `uninstall-pam --help` snapshot.                               |
//! | 09  | #1  | `list` on empty store prints the hint, exits 0.                |
//! | 10  | #2  | `revoke --id <known>` flips status to Revoked.                 |
//! | 11  | #2  | `revoke` twice is idempotent (exit 0, reason preserved).       |
//! | 12  | #2  | `revoke --id <unknown>` exits non-zero, id named in stderr.    |
//! | 13  | #3  | `status` prints all five documented labels.                    |
//! | 14  | #3  | `status` parses a synthetic `last.log` entry.                  |
//! | 15  | #3  | `status` reports `adapter-state: Missing` for unknown adapter. |
//! | 16  | #3  | `status` prints `(no entries)` when `last.log` is absent.      |

#![allow(clippy::expect_used)] // tests are allowed to expect()

use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

use assert_cmd::Command;
use syauth_core::{Bond, BondStatus, BondStore, peer_id_from_pubkey};
use tempfile::TempDir;
use time::macros::datetime;

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

const TEST_ADAPTER_MISSING: &str = "not-a-real-adapter-syauth-test";
const TEST_REASON: &str = "phone-lost";
const TEST_PUBKEY: [u8; 32] = [0x21; 32];

fn syauth() -> Command {
    Command::cargo_bin("syauth").expect("locate built syauth binary")
}

/// Create a fresh `--bond-dir`-suitable directory under `td` with mode
/// 0o700 (so `BondStore::save` does not refuse it as too-permissive).
fn make_bond_dir(td: &TempDir) -> PathBuf {
    let p = td.path().join("syauth");
    fs::create_dir_all(&p).expect("mkdir bond dir");
    fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).expect("chmod 0o700");
    p
}

/// Seed `bond_dir/bonds.toml` with exactly one Bonded record. Returns
/// the bond's peer id.
fn seed_one_bond(bond_dir: &std::path::Path, name: &str) -> String {
    let mut store = BondStore::empty();
    let bond = Bond {
        peer_id: peer_id_from_pubkey(&TEST_PUBKEY),
        pubkey: TEST_PUBKEY,
        name: name.to_owned(),
        created_at: datetime!(2026-05-15 12:00:00 UTC),
        status: BondStatus::Bonded,
    };
    let id = bond.peer_id.clone();
    store.add(bond).expect("add");
    store.save(&bond_dir.join("bonds.toml")).expect("save");
    id
}

// ---------------------------------------------------------------------------
// TC-01: `syauth --version`.
// ---------------------------------------------------------------------------

#[test]
fn version_prints_semver_and_exits_0() {
    let out = syauth().arg("--version").assert().success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    let re = regex::Regex::new(r"^syauth \d+\.\d+\.\d+").expect("compile re");
    assert!(re.is_match(stdout.trim()), "version line shape: {stdout:?}");
}

// ---------------------------------------------------------------------------
// TC-02..08: --help snapshots. `insta::assert_snapshot!` reads (or creates,
// the first time) `tests/snapshots/cli__<name>.snap`. The committed
// snapshots pin the public CLI surface so clap regressions or copy-edits
// surface as a snapshot diff.
// ---------------------------------------------------------------------------

fn help_stdout(args: &[&str]) -> String {
    let out = syauth().args(args).assert().success();
    String::from_utf8_lossy(&out.get_output().stdout).into_owned()
}

#[test]
fn help_snapshot() {
    insta::assert_snapshot!("help_snapshot", help_stdout(&["--help"]));
}

#[test]
fn pair_help_snapshot() {
    insta::assert_snapshot!("pair_help_snapshot", help_stdout(&["pair", "--help"]));
}

#[test]
fn list_help_snapshot() {
    insta::assert_snapshot!("list_help_snapshot", help_stdout(&["list", "--help"]));
}

#[test]
fn revoke_help_snapshot() {
    insta::assert_snapshot!("revoke_help_snapshot", help_stdout(&["revoke", "--help"]));
}

#[test]
fn status_snapshot() {
    // S-017 DoD: `tests/snapshots/cli__status_snapshot.snap` updated +
    // reviewed. The S-012 `status_help_snapshot.snap` is preserved
    // in-tree as the prior reference; this snapshot pins the S-017
    // surface (`--socket`, `--watch`, `--json`).
    insta::assert_snapshot!("status_snapshot", help_stdout(&["status", "--help"]));
}

#[test]
fn install_pam_help_snapshot() {
    insta::assert_snapshot!("install_pam_help_snapshot", help_stdout(&["install-pam", "--help"]));
}

#[test]
fn uninstall_pam_help_snapshot() {
    insta::assert_snapshot!("uninstall_pam_help_snapshot", help_stdout(&["uninstall-pam", "--help"]));
}

#[test]
fn install_presenced_help_snapshot() {
    insta::assert_snapshot!("install_presenced_help_snapshot", help_stdout(&["install-presenced", "--help"]));
}

#[test]
fn doctor_help_snapshot() {
    insta::assert_snapshot!("doctor_help_snapshot", help_stdout(&["doctor", "--help"]));
}

// ---------------------------------------------------------------------------
// TC-09: `syauth list` on an empty store prints the hint and exits 0.
// ---------------------------------------------------------------------------

#[test]
fn list_on_empty_store_prints_hint() {
    let td = TempDir::new().expect("tempdir");
    let bond_dir = make_bond_dir(&td);
    let out = syauth().args(["list", "--bond-dir"]).arg(&bond_dir).assert().success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(stdout.contains("no bonds"), "empty hint shape: {stdout:?}");
}

// ---------------------------------------------------------------------------
// TC-10: `syauth revoke --id <known>` flips status to Revoked.
// ---------------------------------------------------------------------------

#[test]
fn revoke_known_bond_marks_revoked() {
    let td = TempDir::new().expect("tempdir");
    let bond_dir = make_bond_dir(&td);
    let id = seed_one_bond(&bond_dir, "alex-pixel");

    syauth()
        .args(["revoke", "--id", &id, "--reason", TEST_REASON, "--bond-dir"])
        .arg(&bond_dir)
        .assert()
        .success();

    let reloaded = BondStore::load(&bond_dir.join("bonds.toml")).expect("reload");
    assert_eq!(reloaded.list().len(), 1, "bond record must be preserved");
    match &reloaded.list()[0].status {
        BondStatus::Revoked { reason } => assert_eq!(reason, TEST_REASON),
        other => panic!("expected Revoked, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// TC-11: revoke is idempotent.
// ---------------------------------------------------------------------------

#[test]
fn revoke_already_revoked_is_idempotent() {
    let td = TempDir::new().expect("tempdir");
    let bond_dir = make_bond_dir(&td);
    let id = seed_one_bond(&bond_dir, "alex-pixel");

    // First revoke.
    syauth()
        .args(["revoke", "--id", &id, "--reason", "first-reason", "--bond-dir"])
        .arg(&bond_dir)
        .assert()
        .success();

    // Second revoke: exits 0, reason preserved.
    let out = syauth()
        .args(["revoke", "--id", &id, "--reason", "second-reason", "--bond-dir"])
        .arg(&bond_dir)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(stdout.contains("already revoked"), "idempotent banner: {stdout:?}");
    assert!(stdout.contains("first-reason"), "must surface the existing reason: {stdout:?}");

    let reloaded = BondStore::load(&bond_dir.join("bonds.toml")).expect("reload");
    match &reloaded.list()[0].status {
        BondStatus::Revoked { reason } => assert_eq!(reason, "first-reason", "reason must not be overwritten"),
        other => panic!("expected Revoked, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// TC-12: revoke with unknown id exits non-zero with id in stderr.
// ---------------------------------------------------------------------------

#[test]
fn revoke_unknown_id_exits_nonzero_with_id_in_stderr() {
    let td = TempDir::new().expect("tempdir");
    let bond_dir = make_bond_dir(&td);
    let bogus_id = "deadbeefcafebabe1234567890abcdef";

    let assert = syauth()
        .args(["revoke", "--id", bogus_id, "--bond-dir"])
        .arg(&bond_dir)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.contains(bogus_id), "stderr must name the looked-up id: {stderr:?}");
}

// ---------------------------------------------------------------------------
// TC-13: status prints all documented field labels.
// ---------------------------------------------------------------------------

#[test]
fn status_prints_all_documented_fields() {
    let td = TempDir::new().expect("tempdir");
    let bond_dir = make_bond_dir(&td);

    let out = syauth()
        .args(["status", "--adapter", TEST_ADAPTER_MISSING, "--bond-dir"])
        .arg(&bond_dir)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    for label in ["adapter:", "adapter-state:", "advertising:", "bonds-count:", "last-unlock:"] {
        assert!(stdout.contains(label), "missing {label} in:\n{stdout}");
    }
}

// ---------------------------------------------------------------------------
// TC-14: status parses a synthetic last.log entry.
// ---------------------------------------------------------------------------

#[test]
fn status_with_synthetic_last_log_parses_correctly() {
    let td = TempDir::new().expect("tempdir");
    let bond_dir = make_bond_dir(&td);
    let last_log = td.path().join("last.log");
    let peer_id = "0123456789abcdef0123456789abcdef";
    let timestamp = "2026-05-15T12:34:56Z";
    fs::write(&last_log, format!("{timestamp} success {peer_id}\n")).expect("write last.log");

    let out = syauth()
        .args(["status", "--adapter", TEST_ADAPTER_MISSING, "--bond-dir"])
        .arg(&bond_dir)
        .arg("--last-log")
        .arg(&last_log)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(stdout.contains(timestamp), "timestamp must appear: {stdout}");
    assert!(stdout.contains("success"));
    assert!(stdout.contains(peer_id));
}

// ---------------------------------------------------------------------------
// TC-15: status reports Missing on a non-existent adapter.
// ---------------------------------------------------------------------------

#[test]
fn status_reports_missing_for_unknown_adapter() {
    let td = TempDir::new().expect("tempdir");
    let bond_dir = make_bond_dir(&td);
    let out = syauth()
        .args(["status", "--adapter", TEST_ADAPTER_MISSING, "--bond-dir"])
        .arg(&bond_dir)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("adapter-state:     Missing"),
        "missing-adapter must surface as Missing: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// TC-16: status prints `(no entries)` when last.log is absent.
// ---------------------------------------------------------------------------

#[test]
fn status_reports_no_entries_when_last_log_absent() {
    let td = TempDir::new().expect("tempdir");
    let bond_dir = make_bond_dir(&td);
    // No last.log exists.
    let out = syauth()
        .args(["status", "--adapter", TEST_ADAPTER_MISSING, "--bond-dir"])
        .arg(&bond_dir)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("last-unlock:       (no entries)"),
        "absent log must surface as (no entries): {stdout}"
    );
}
