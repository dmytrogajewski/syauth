// Journey: specs/journeys/JOURNEY-S-017-status-extension.md
// Roadmap row: specs/unlock-proximity/ROADMAP.md Step S-017.
//
// Integration tests for the S-017 extension of `syauth status`:
// daemon-aware per-peer table + `--watch` polling + `--json` typed
// output + daemon-down fallback. Every probed path is hermetic
// (tempdir + fake daemon thread); the real
// `${XDG_RUNTIME_DIR}/syauth/auth.sock` is never touched.
//
// Coverage:
//
// | TC  | DoD | Scenario                                                 |
// |-----|-----|----------------------------------------------------------|
// | 01  | #1  | `reports_per_peer_liveness`                              |
// | 02  | #2  | `falls_back_when_daemon_down`                            |

#![allow(clippy::expect_used)]

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
use syauth_presenced::{PeerStatus, Request, Response, read_frame_blocking, write_frame_blocking};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

const TEST_PEER_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TEST_PEER_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const TEST_LAST_CHALLENGE_MS: u64 = 3_200;
const TEST_LAST_CONNECT_MS: u64 = 3_200;

fn syauth() -> Command {
    Command::cargo_bin("syauth").expect("locate built syauth binary")
}

/// Make a tempdir-rooted directory with 0o700 perms so child
/// fixtures (sockets, bonds, keys) live under a restrictive parent.
fn make_dir(td: &TempDir, name: &str) -> PathBuf {
    let p = td.path().join(name);
    fs::create_dir_all(&p).expect("mkdir");
    fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).expect("chmod");
    p
}

/// Tiny background thread that binds `socket`, accepts one
/// connection, reads a single `Request::Status`, and writes back a
/// `Response::Status` carrying the supplied `peers` list and
/// `started_at`.
struct FakeStatusDaemon {
    handle: Option<thread::JoinHandle<()>>,
    _stop: mpsc::Sender<()>,
}

impl FakeStatusDaemon {
    fn new(socket: PathBuf, peers: Vec<PeerStatus>, started_at: SystemTime) -> Self {
        let listener = UnixListener::bind(&socket).expect("bind fake daemon listener");
        listener.set_nonblocking(false).expect("blocking listener");
        let (tx, _rx) = mpsc::channel::<()>();
        let handle = thread::spawn(move || {
            if let Ok((mut stream, _addr)) = listener.accept() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                if let Ok(Request::Status) = read_frame_blocking::<_, Request>(&mut stream) {
                    let resp = Response::Status { peers, started_at };
                    let _ = write_frame_blocking(&mut stream, &resp);
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

impl Drop for FakeStatusDaemon {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn peer_row(peer_id: &str) -> PeerStatus {
    PeerStatus {
        peer_id: peer_id.to_owned(),
        last_challenge_ms_ago: Some(TEST_LAST_CHALLENGE_MS),
        last_connect_ms_ago: Some(TEST_LAST_CONNECT_MS),
        current_session_uuid: uuid::Uuid::from_bytes([0xd9; 16]),
        in_flight_challenges: 0,
    }
}

// ---------------------------------------------------------------------------
// TC-01: daemon reachable → per-peer table contains both peer_ids.
// ---------------------------------------------------------------------------

#[test]
fn reports_per_peer_liveness() {
    let td = TempDir::new().expect("tempdir");
    let runtime_dir = make_dir(&td, "syauth");
    let socket = runtime_dir.join("auth.sock");
    let bond_dir = make_dir(&td, "bonds");
    let peers = vec![peer_row(TEST_PEER_A), peer_row(TEST_PEER_B)];
    let _daemon = FakeStatusDaemon::new(socket.clone(), peers, SystemTime::now());

    let out = syauth()
        .args(["status", "--socket"])
        .arg(&socket)
        .arg("--adapter")
        .arg("absent-adapter")
        .arg("--bond-dir")
        .arg(&bond_dir)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(stdout.contains("daemon=up"), "expected daemon=up in:\n{stdout}");
    assert!(stdout.contains(TEST_PEER_A), "expected peer A in:\n{stdout}");
    assert!(stdout.contains(TEST_PEER_B), "expected peer B in:\n{stdout}");
}

// ---------------------------------------------------------------------------
// TC-02: socket file missing → `daemon=down: socket-missing`.
// ---------------------------------------------------------------------------

#[test]
fn falls_back_when_daemon_down() {
    let td = TempDir::new().expect("tempdir");
    let runtime_dir = make_dir(&td, "syauth");
    let socket = runtime_dir.join("does-not-exist.sock");
    let bond_dir = make_dir(&td, "bonds");

    let out = syauth()
        .args(["status", "--socket"])
        .arg(&socket)
        .arg("--adapter")
        .arg("absent-adapter")
        .arg("--bond-dir")
        .arg(&bond_dir)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(stdout.contains("daemon=down"), "expected daemon=down in:\n{stdout}");
    assert!(
        stdout.contains("socket-missing"),
        "expected reason token socket-missing in:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// TC-03: `--json` mode emits a parseable object with the daemon
// section.
// ---------------------------------------------------------------------------

#[test]
fn json_mode_emits_typed_object() {
    let td = TempDir::new().expect("tempdir");
    let bond_dir = make_dir(&td, "bonds");
    let socket = td.path().join("absent.sock");

    let out = syauth()
        .args(["status", "--json", "--socket"])
        .arg(&socket)
        .arg("--adapter")
        .arg("absent-adapter")
        .arg("--bond-dir")
        .arg(&bond_dir)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("parse json output");
    let obj = parsed.as_object().expect("top-level object");
    assert!(obj.contains_key("daemon_socket"), "missing daemon_socket: {stdout}");
    assert!(obj.contains_key("daemon"), "missing daemon: {stdout}");
    let daemon = obj.get("daemon").and_then(|v| v.as_object()).expect("daemon object");
    let state = daemon.get("state").and_then(|v| v.as_str()).expect("state token");
    assert!(matches!(state, "up" | "down"), "unexpected state token {state}");
}
