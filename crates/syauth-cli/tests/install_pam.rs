//! Integration tests for `syauth install-pam` and `syauth uninstall-pam`.
//!
//! Hermetic: every test uses a `tempfile::TempDir` for the PAM service tree
//! and passes `--pam-dir <tempdir>`. The real `/etc/pam.d` is never touched.
//!
//! Journey: specs/journeys/JOURNEY-S-013-pam-install-helper.md
//! Roadmap: specs/syauth/ROADMAP.md item S-013.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use assert_cmd::Command;
use tempfile::TempDir;

/// A realistic stock /etc/pam.d/sudo from Fedora 39. Embedded verbatim so
/// every test starts from a known-good byte sequence.
const FIXTURE_SUDO: &[u8] = b"#%PAM-1.0
# Used with polkit, sudo, and graphical sudo wrappers.

auth       include      system-auth
account    include      system-auth
password   include      system-auth
session    include      system-auth
session    optional     pam_xauth.so
";

/// Canonical inserted line per the S-013 DoD.
const CANONICAL_LINE: &str = "auth    required    pam_syauth.so timeout=1200";

/// Service name used by every test.
const SERVICE_NAME: &str = "sudo";

/// Default mode for PAM service files on Linux.
const DEFAULT_PAM_MODE: u32 = 0o644;

fn pam_dir() -> TempDir {
    tempfile::tempdir().expect("create tempdir for pam.d fixture")
}

fn write_fixture(dir: &Path, service: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(service);
    fs::write(&path, bytes).expect("write fixture");
    fs::set_permissions(&path, fs::Permissions::from_mode(DEFAULT_PAM_MODE)).expect("chmod fixture");
    path
}

fn syauth() -> Command {
    Command::cargo_bin("syauth").expect("locate built syauth binary")
}

#[test]
fn tc01_install_inserts_canonical_line_at_top_of_auth_block() {
    let dir = pam_dir();
    let service = write_fixture(dir.path(), SERVICE_NAME, FIXTURE_SUDO);
    let bak = dir.path().join(format!("{SERVICE_NAME}.bak"));

    syauth()
        .args(["install-pam", "--service", SERVICE_NAME, "--pam-dir"])
        .arg(dir.path())
        .arg("--yes")
        .arg("--with-presenced=false")
        .assert()
        .success();

    let after = fs::read_to_string(&service).expect("read post-install");
    // The canonical line must precede the first existing `auth` directive.
    let canonical_idx = after.find(CANONICAL_LINE).expect("canonical line present");
    let first_orig_auth = after
        .find("auth       include      system-auth")
        .expect("original auth line preserved");
    assert!(
        canonical_idx < first_orig_auth,
        "canonical line must be above the original auth stack; got file:\n{after}"
    );
    // Bak exists and equals the original.
    assert_eq!(
        fs::read(&bak).expect("read bak"),
        FIXTURE_SUDO,
        "bak should be byte-equal to the pre-install file"
    );
}

#[test]
fn tc02_install_is_idempotent() {
    let dir = pam_dir();
    let service = write_fixture(dir.path(), SERVICE_NAME, FIXTURE_SUDO);
    let bak = dir.path().join(format!("{SERVICE_NAME}.bak"));

    // First install.
    syauth()
        .args(["install-pam", "--service", SERVICE_NAME, "--pam-dir"])
        .arg(dir.path())
        .arg("--yes")
        .arg("--with-presenced=false")
        .assert()
        .success();
    let after_first = fs::read(&service).expect("read post-first");
    let bak_first = fs::read(&bak).expect("read bak post-first");

    // Second install.
    syauth()
        .args(["install-pam", "--service", SERVICE_NAME, "--pam-dir"])
        .arg(dir.path())
        .arg("--yes")
        .arg("--with-presenced=false")
        .assert()
        .success();

    let after_second = fs::read(&service).expect("read post-second");
    let bak_second = fs::read(&bak).expect("read bak post-second");
    assert_eq!(
        after_first, after_second,
        "service file must be byte-identical after second install"
    );
    assert_eq!(bak_first, bak_second, "bak must be byte-identical after second install");
}

#[test]
fn tc03_install_refuses_to_overwrite_existing_bak() {
    let dir = pam_dir();
    let service = write_fixture(dir.path(), SERVICE_NAME, FIXTURE_SUDO);
    let bak = dir.path().join(format!("{SERVICE_NAME}.bak"));
    fs::write(&bak, b"unrelated backup\n").expect("seed bak");
    let service_before = fs::read(&service).expect("read service before");
    let bak_before = fs::read(&bak).expect("read bak before");

    syauth()
        .args(["install-pam", "--service", SERVICE_NAME, "--pam-dir"])
        .arg(dir.path())
        .arg("--yes")
        .arg("--with-presenced=false")
        .assert()
        .failure()
        .stderr(predicates::str::contains(bak.to_string_lossy().to_string()));

    assert_eq!(fs::read(&service).expect("re-read service"), service_before);
    assert_eq!(fs::read(&bak).expect("re-read bak"), bak_before);
}

#[test]
fn tc04_uninstall_restores_byte_equality_from_bak() {
    let dir = pam_dir();
    let service = write_fixture(dir.path(), SERVICE_NAME, FIXTURE_SUDO);
    let bak = dir.path().join(format!("{SERVICE_NAME}.bak"));

    syauth()
        .args(["install-pam", "--service", SERVICE_NAME, "--pam-dir"])
        .arg(dir.path())
        .arg("--yes")
        .arg("--with-presenced=false")
        .assert()
        .success();
    assert!(bak.exists(), "bak must be present after install");

    syauth()
        .args(["uninstall-pam", "--service", SERVICE_NAME, "--pam-dir"])
        .arg(dir.path())
        .arg("--yes")
        .assert()
        .success();

    assert_eq!(
        fs::read(&service).expect("read restored service"),
        FIXTURE_SUDO,
        "service must be byte-identical to FIXTURE_SUDO after uninstall"
    );
    assert!(!bak.exists(), "bak must be removed after successful uninstall");
}

#[test]
fn tc05_uninstall_is_noop_when_no_syauth_line_present() {
    let dir = pam_dir();
    let service = write_fixture(dir.path(), SERVICE_NAME, FIXTURE_SUDO);
    let bak = dir.path().join(format!("{SERVICE_NAME}.bak"));
    fs::write(&bak, b"belongs to another tool\n").expect("seed unrelated bak");
    let service_before = fs::read(&service).expect("read service before");
    let bak_before = fs::read(&bak).expect("read bak before");

    syauth()
        .args(["uninstall-pam", "--service", SERVICE_NAME, "--pam-dir"])
        .arg(dir.path())
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicates::str::contains("nothing to uninstall"));

    assert_eq!(fs::read(&service).expect("re-read service"), service_before);
    assert_eq!(fs::read(&bak).expect("re-read bak"), bak_before);
}

#[test]
fn tc06_uninstall_refuses_when_bak_missing_but_line_present() {
    let dir = pam_dir();
    let service_path = dir.path().join(SERVICE_NAME);
    let mut hand_crafted = String::from("#%PAM-1.0\n");
    hand_crafted.push_str(CANONICAL_LINE);
    hand_crafted.push('\n');
    hand_crafted.push_str("auth       include      system-auth\n");
    fs::write(&service_path, hand_crafted.as_bytes()).expect("write hand-crafted service");
    fs::set_permissions(&service_path, fs::Permissions::from_mode(DEFAULT_PAM_MODE)).expect("chmod");
    let service_before = fs::read(&service_path).expect("read before");

    syauth()
        .args(["uninstall-pam", "--service", SERVICE_NAME, "--pam-dir"])
        .arg(dir.path())
        .arg("--yes")
        .assert()
        .failure()
        .stderr(predicates::str::contains("no backup found"));

    assert_eq!(fs::read(&service_path).expect("re-read"), service_before);
}

#[test]
fn tc07_install_preserves_file_mode() {
    let dir = pam_dir();
    let service = write_fixture(dir.path(), SERVICE_NAME, FIXTURE_SUDO);
    // Set an unusual but valid mode to prove preservation.
    let target_mode: u32 = 0o640;
    fs::set_permissions(&service, fs::Permissions::from_mode(target_mode)).expect("chmod target");

    syauth()
        .args(["install-pam", "--service", SERVICE_NAME, "--pam-dir"])
        .arg(dir.path())
        .arg("--yes")
        .arg("--with-presenced=false")
        .assert()
        .success();

    let post_mode = fs::metadata(&service).expect("metadata").permissions().mode() & 0o7777;
    assert_eq!(post_mode, target_mode, "install must preserve the source file's mode bits");
}

#[test]
fn tc08_install_honors_module_args() {
    let dir = pam_dir();
    let service = write_fixture(dir.path(), SERVICE_NAME, FIXTURE_SUDO);

    syauth()
        .args([
            "install-pam",
            "--service",
            SERVICE_NAME,
            "--module-args",
            "timeout=60 debug",
            "--pam-dir",
        ])
        .arg(dir.path())
        .arg("--yes")
        .arg("--with-presenced=false")
        .assert()
        .success();

    let after = fs::read_to_string(&service).expect("read after");
    assert!(
        after.contains("auth    required    pam_syauth.so timeout=60 debug"),
        "expected custom module-args to be honored; got:\n{after}"
    );
}

#[test]
fn tc09_install_honors_so_path() {
    let dir = pam_dir();
    let service = write_fixture(dir.path(), SERVICE_NAME, FIXTURE_SUDO);

    syauth()
        .args([
            "install-pam",
            "--service",
            SERVICE_NAME,
            "--so-path",
            "pam_syauth_test.so",
            "--pam-dir",
        ])
        .arg(dir.path())
        .arg("--yes")
        .arg("--with-presenced=false")
        .assert()
        .success();

    let after = fs::read_to_string(&service).expect("read after");
    assert!(
        after.contains("auth    required    pam_syauth_test.so timeout=1200"),
        "expected custom so-path to be honored; got:\n{after}"
    );
}

#[test]
fn tc10_help_invocations_succeed() {
    syauth().args(["install-pam", "--help"]).assert().success();
    syauth().args(["uninstall-pam", "--help"]).assert().success();
}

#[test]
fn tc11_install_pam_bundles_presenced_by_default() {
    // S-009: `--with-presenced=true` is the default. The bundled path
    // writes the systemd user unit alongside the PAM edit. We pass
    // `--presenced-dry-run --presenced-unit-dir <tempdir> --presenced-from
    // <fake>` so neither systemctl nor /usr/local/libexec is touched.
    let dir = pam_dir();
    write_fixture(dir.path(), SERVICE_NAME, FIXTURE_SUDO);
    let presenced_dir = tempfile::tempdir().expect("presenced tempdir");
    let fake = presenced_dir.path().join("fake-daemon-binary");
    fs::write(&fake, b"").expect("touch fake");

    let assert = syauth()
        .args(["install-pam", "--service", SERVICE_NAME, "--pam-dir"])
        .arg(dir.path())
        .arg("--yes")
        .arg("--presenced-dry-run")
        .arg("--presenced-unit-dir")
        .arg(presenced_dir.path())
        .arg("--presenced-from")
        .arg(&fake)
        .assert()
        .success();

    let unit_path = presenced_dir.path().join("syauth-presenced.service");
    assert!(unit_path.exists(), "bundled install must write the unit file");
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(stdout.contains("would-run: systemctl --user daemon-reload"), "{stdout}");
    assert!(
        stdout.contains("would-run: systemctl --user enable --now syauth-presenced.service"),
        "{stdout}"
    );
}
