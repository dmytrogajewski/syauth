//! S-016 integration tests: `syauth doctor` subcommand.
//!
//! Journey: specs/journeys/JOURNEY-S-016-syauth-doctor.md
//!
//! Each test is hermetic — all probed paths (socket, bonds file, keys
//! dir, audit log) are rooted in a `tempfile::TempDir`. The real
//! `/var/lib/syauth/` and `${XDG_RUNTIME_DIR}/syauth/` are never
//! touched.
//!
//! Coverage:
//!
//! | TC  | DoD | Scenario                                                |
//! |-----|-----|---------------------------------------------------------|
//! | 01  | #3  | `reports_daemon_up_when_socket_responds`                |
//! | 02  | #4  | `reports_daemon_down_when_socket_missing`               |
//! | 03  | #5  | `flags_keys_file_not_0600`                              |
//! | 04  | #2  | `json_mode_emits_typed_object`                          |

#![allow(clippy::expect_used)] // tests are allowed to expect()

use std::{
    fs,
    io::Read as _,
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, SystemTime},
};

use assert_cmd::Command;
use syauth_presenced::{Request, Response, read_frame_blocking, write_frame_blocking};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

const TEST_PEER_ID: &str = "0123456789abcdef0123456789abcdef";

fn syauth() -> Command {
    Command::cargo_bin("syauth").expect("locate built syauth binary")
}

/// Make a tempdir-rooted directory with mode 0o700 so child fixtures
/// (sockets, bonds, keys) live under a restrictive parent.
fn make_dir(td: &TempDir, name: &str) -> PathBuf {
    let p = td.path().join(name);
    fs::create_dir_all(&p).expect("mkdir");
    fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).expect("chmod 0o700");
    p
}

/// Spawn a tiny background thread that binds `socket`, accepts one
/// connection, reads a single `Request::Status`, and writes back
/// `Response::Status { peers: [], started_at: now }`. Returns the
/// thread handle so the test can join it on drop.
struct FakeDaemon {
    handle: Option<thread::JoinHandle<()>>,
    _stop: mpsc::Sender<()>,
}

impl FakeDaemon {
    fn new(socket: PathBuf) -> Self {
        let listener = UnixListener::bind(&socket).expect("bind fake daemon listener");
        listener.set_nonblocking(false).expect("blocking listener");
        let (tx, _rx) = mpsc::channel::<()>();
        let handle = thread::spawn(move || {
            if let Ok((mut stream, _addr)) = listener.accept() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                if let Ok(req) = read_frame_blocking::<_, Request>(&mut stream) {
                    if matches!(req, Request::Status) {
                        let resp = Response::Status {
                            peers: vec![],
                            started_at: SystemTime::now(),
                        };
                        let _ = write_frame_blocking(&mut stream, &resp);
                    }
                }
                let mut buf = [0u8; 1];
                let _ = stream.read(&mut buf);
            }
        });
        Self {
            handle: Some(handle),
            _stop: tx,
        }
    }
}

impl Drop for FakeDaemon {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ---------------------------------------------------------------------------
// TC-01: daemon reachable → `daemon=up`.
// ---------------------------------------------------------------------------

#[test]
fn reports_daemon_up_when_socket_responds() {
    let td = TempDir::new().expect("tempdir");
    let runtime_dir = make_dir(&td, "syauth");
    let socket = runtime_dir.join("auth.sock");
    let _daemon = FakeDaemon::new(socket.clone());

    let bond_dir = make_dir(&td, "bonds");
    let keys_dir = make_dir(&td, "keys");

    let out = syauth()
        .args(["doctor", "--socket"])
        .arg(&socket)
        .arg("--bonds-file")
        .arg(bond_dir.join("bonds.toml"))
        .arg("--keys-dir")
        .arg(&keys_dir)
        .arg("--audit-log")
        .arg(td.path().join("last.log"))
        .arg("--skip-systemctl")
        .arg("--skip-bluez")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(stdout.contains("daemon=up"), "expected daemon=up in:\n{stdout}");
}

// ---------------------------------------------------------------------------
// TC-02: socket file missing → `daemon=down: socket-missing`.
// ---------------------------------------------------------------------------

#[test]
fn reports_daemon_down_when_socket_missing() {
    let td = TempDir::new().expect("tempdir");
    let runtime_dir = make_dir(&td, "syauth");
    let socket = runtime_dir.join("does-not-exist.sock");
    let bond_dir = make_dir(&td, "bonds");
    let keys_dir = make_dir(&td, "keys");

    let out = syauth()
        .args(["doctor", "--socket"])
        .arg(&socket)
        .arg("--bonds-file")
        .arg(bond_dir.join("bonds.toml"))
        .arg("--keys-dir")
        .arg(&keys_dir)
        .arg("--audit-log")
        .arg(td.path().join("last.log"))
        .arg("--skip-systemctl")
        .arg("--skip-bluez")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(stdout.contains("daemon=down"), "expected daemon=down in:\n{stdout}");
    assert!(stdout.contains("socket-missing"), "expected socket-missing reason in:\n{stdout}");
    assert!(stdout.contains("doctor=fail"), "expected summary doctor=fail in:\n{stdout}");
}

// ---------------------------------------------------------------------------
// TC-03: keys file with mode 0644 → flagged with `(expected 0600)`,
// summary downgrades to `doctor=warn`.
// ---------------------------------------------------------------------------

#[test]
fn flags_keys_file_not_0600() {
    let td = TempDir::new().expect("tempdir");
    let bond_dir = make_dir(&td, "bonds");
    let keys_dir = make_dir(&td, "keys");

    let key_path = keys_dir.join(format!("{TEST_PEER_ID}.bin"));
    fs::write(&key_path, [0u8; 32]).expect("write keys file");
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).expect("chmod 0o644");

    let socket = td.path().join("absent.sock");

    let out = syauth()
        .args(["doctor", "--socket"])
        .arg(&socket)
        .arg("--bonds-file")
        .arg(bond_dir.join("bonds.toml"))
        .arg("--keys-dir")
        .arg(&keys_dir)
        .arg("--audit-log")
        .arg(td.path().join("last.log"))
        .arg("--skip-systemctl")
        .arg("--skip-bluez")
        .arg("--skip-daemon")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    let want = format!("keys_{TEST_PEER_ID}_mode=0644");
    assert!(stdout.contains(&want), "expected {want} in:\n{stdout}");
    assert!(
        stdout.contains("(expected 0600)"),
        "expected (expected 0600) annotation in:\n{stdout}"
    );
    assert!(stdout.contains("doctor=warn"), "expected summary doctor=warn in:\n{stdout}");
}

// ---------------------------------------------------------------------------
// TC-04: `--json` emits a parseable object with the documented keys.
// ---------------------------------------------------------------------------

#[test]
fn json_mode_emits_typed_object() {
    let td = TempDir::new().expect("tempdir");
    let bond_dir = make_dir(&td, "bonds");
    let keys_dir = make_dir(&td, "keys");

    let out = syauth()
        .args(["doctor", "--json", "--socket"])
        .arg(td.path().join("absent.sock"))
        .arg("--bonds-file")
        .arg(bond_dir.join("bonds.toml"))
        .arg("--keys-dir")
        .arg(&keys_dir)
        .arg("--audit-log")
        .arg(td.path().join("last.log"))
        .arg("--skip-systemctl")
        .arg("--skip-bluez")
        .arg("--skip-daemon")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("parse json output");
    let obj = parsed.as_object().expect("top-level object");
    for key in [
        "daemon_socket",
        "daemon",
        "bonds_file",
        "keys",
        "bluez_adapter",
        "systemctl",
        "last_log_tail",
        "xdg_runtime_dir",
        "summary",
    ] {
        assert!(obj.contains_key(key), "missing key {key} in {stdout}");
    }
    let summary = obj.get("summary").and_then(|v| v.as_str()).expect("summary token");
    assert!(matches!(summary, "ok" | "warn" | "fail"), "summary token {summary}");
}
