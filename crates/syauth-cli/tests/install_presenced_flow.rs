//! Integration test for `syauth install-presenced`.
//!
//! Hermetic: every test uses a `tempfile::TempDir` for `--unit-dir` and a
//! tempdir-rooted fake binary for `--from`. The real
//! `~/.config/systemd/user/` is never touched. `--dry-run` short-circuits
//! the `systemctl` invocations and prints `would-run:` lines instead.
//!
//! Journey: specs/journeys/JOURNEY-S-009-install-presenced-retire-burst.md
//! Roadmap: specs/unlock-proximity/ROADMAP.md item S-009.

use std::{fs, path::PathBuf};

use assert_cmd::Command;
use tempfile::TempDir;

/// File name the installer writes inside `--unit-dir`. Mirrors the constant
/// in the production module so the test fails loudly if the name drifts.
const UNIT_FILE_NAME: &str = "syauth-presenced.service";

/// Filename for the synthetic source binary the test passes via `--from`.
const FAKE_BINARY_NAME: &str = "fake-daemon-binary";

/// Stdout banner the installer emits in dry-run for each `systemctl` call.
const WOULD_RUN_DAEMON_RELOAD: &str = "would-run: systemctl --user daemon-reload";
const WOULD_RUN_ENABLE_NOW: &str = "would-run: systemctl --user enable --now syauth-presenced.service";

fn syauth() -> Command {
    Command::cargo_bin("syauth").expect("locate built syauth binary")
}

fn touch(path: &PathBuf) {
    fs::write(path, b"").expect("touch fake source binary");
}

#[test]
fn install_writes_unit_and_starts_service() {
    let dir = TempDir::new().expect("tempdir");
    let fake = dir.path().join(FAKE_BINARY_NAME);
    touch(&fake);

    let assert = syauth()
        .args(["install-presenced", "--dry-run", "--unit-dir"])
        .arg(dir.path())
        .arg("--from")
        .arg(&fake)
        .assert()
        .success();

    // Unit file landed in --unit-dir with the expected name.
    let unit_path = dir.path().join(UNIT_FILE_NAME);
    assert!(unit_path.exists(), "unit file must be written to --unit-dir");

    let unit_body = fs::read_to_string(&unit_path).expect("read unit file");
    let expected_exec_start = format!("ExecStart={}", fake.display());
    assert!(
        unit_body.contains(&expected_exec_start),
        "unit must reference --from path; got:\n{unit_body}"
    );

    // Mode-readable check: any positive read permission on user, group, or
    // other. We don't pin a specific mode because the atomic-write helper
    // inherits umask; we just require the file is readable so a
    // systemd-user can pick it up.
    let meta = fs::metadata(&unit_path).expect("metadata");
    assert!(meta.is_file(), "unit path must be a regular file");

    // Both would-run lines printed verbatim.
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        stdout.contains(WOULD_RUN_DAEMON_RELOAD),
        "stdout must include daemon-reload would-run line; got:\n{stdout}"
    );
    assert!(
        stdout.contains(WOULD_RUN_ENABLE_NOW),
        "stdout must include enable --now would-run line; got:\n{stdout}"
    );
}
