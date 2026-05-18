// Journey: specs/journeys/JOURNEY-S-001-scaffold-syauth-presenced.md
// Roadmap row: specs/unlock-proximity/ROADMAP.md Step S-001.
//
// Smoke tests for the daemon's lifecycle:
//   TC-01 — starts_and_terminates_cleanly: spawn, wait for pidfile,
//           SIGTERM, assert exit 0 + pidfile removed.
//   TC-02 — refuses_second_instance: spawn one instance, spawn a
//           second with the same flags, assert the second exits
//           non-zero while the first keeps running.

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use tempfile::TempDir;

/// Maximum wait for the pidfile to appear after spawning the daemon.
const PIDFILE_APPEAR_BUDGET: Duration = Duration::from_secs(5);

/// Maximum wait for the daemon to exit after SIGTERM.
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);

/// Maximum wait for the second-instance refusal exit.
const REFUSAL_BUDGET: Duration = Duration::from_secs(5);

/// Polling cadence for the file-existence / exit polls. 25 ms keeps
/// the smoke tests responsive without spinning.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

#[test]
fn starts_and_terminates_cleanly() {
    let env = TestEnv::new();
    let mut child = env.spawn_daemon();
    wait_for_pidfile(&env.pidfile_path()).expect("pidfile should appear before TC-01 budget elapses");
    send_sigterm(&child);
    let status = wait_for_exit(&mut child, SHUTDOWN_BUDGET).expect("daemon should exit before TC-01 shutdown budget elapses");
    assert!(status.success(), "daemon should exit 0 on SIGTERM; got {status:?}");
    assert!(
        !env.pidfile_path().exists(),
        "pidfile {:?} should be unlinked on clean shutdown",
        env.pidfile_path()
    );
}

#[test]
fn refuses_second_instance() {
    let env = TestEnv::new();
    let mut first = env.spawn_daemon();
    wait_for_pidfile(&env.pidfile_path()).expect("first-instance pidfile should appear before TC-02 budget elapses");

    let mut second = env.spawn_daemon();
    let status = wait_for_exit(&mut second, REFUSAL_BUDGET).expect("second instance should exit before TC-02 refusal budget elapses");
    assert!(
        !status.success(),
        "second instance should exit non-zero when lock is held; got {status:?}"
    );
    assert!(
        env.pidfile_path().exists(),
        "first instance's pidfile {:?} should still exist after second-instance refusal",
        env.pidfile_path()
    );

    send_sigterm(&first);
    let status = wait_for_exit(&mut first, SHUTDOWN_BUDGET).expect("first instance should exit before TC-02 cleanup budget elapses");
    assert!(
        status.success(),
        "first instance should exit 0 on SIGTERM after TC-02; got {status:?}"
    );
    assert!(
        !env.pidfile_path().exists(),
        "pidfile {:?} should be unlinked once first instance exits",
        env.pidfile_path()
    );
}

struct TestEnv {
    #[allow(dead_code)]
    tempdir: TempDir,
    runtime_dir: PathBuf,
    bonds_file: PathBuf,
    keys_dir: PathBuf,
    socket: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let tempdir = tempfile::Builder::new()
            .prefix("syauth-presenced-smoke-")
            .tempdir()
            .expect("tempdir should be creatable");
        let runtime_dir = tempdir.path().join("runtime");
        let bonds_file = tempdir.path().join("bonds.toml");
        let keys_dir = tempdir.path().join("keys");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir create");
        std::fs::create_dir_all(&keys_dir).expect("keys dir create");
        let socket = runtime_dir.join("auth.sock");
        Self {
            tempdir,
            runtime_dir,
            bonds_file,
            keys_dir,
            socket,
        }
    }

    fn pidfile_path(&self) -> PathBuf {
        self.runtime_dir.join("syauth").join("presenced.pid")
    }

    fn spawn_daemon(&self) -> Child {
        let bin = daemon_bin_path();
        Command::new(&bin)
            .arg("--socket")
            .arg(&self.socket)
            .arg("--bonds-file")
            .arg(&self.bonds_file)
            .arg("--keys-dir")
            .arg(&self.keys_dir)
            .arg("--pidfile")
            .arg(self.pidfile_path())
            .arg("--log-level")
            .arg("error")
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|err| panic!("failed to spawn {}: {err}", bin.display()))
    }
}

fn daemon_bin_path() -> PathBuf {
    // `CARGO_BIN_EXE_syauth-presenced` is set by cargo for integration
    // tests of the same package; it points at the freshly-built
    // binary so the smoke test exercises the same code path as the
    // user-visible `cargo build`.
    let path = env!("CARGO_BIN_EXE_syauth-presenced");
    PathBuf::from(path)
}

fn wait_for_pidfile(path: &Path) -> Result<(), String> {
    let deadline = Instant::now() + PIDFILE_APPEAR_BUDGET;
    while Instant::now() < deadline {
        if path.exists() {
            return Ok(());
        }
        sleep(POLL_INTERVAL);
    }
    Err(format!("pidfile {path:?} did not appear within {PIDFILE_APPEAR_BUDGET:?}"))
}

fn wait_for_exit(child: &mut Child, budget: Duration) -> Result<std::process::ExitStatus, String> {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => sleep(POLL_INTERVAL),
            Err(err) => return Err(format!("try_wait failed: {err}")),
        }
    }
    Err(format!("child {:?} did not exit within {budget:?}", child.id()))
}

fn send_sigterm(child: &Child) {
    let pid = Pid::from_raw(i32::try_from(child.id()).expect("child PID fits in i32"));
    kill(pid, Signal::SIGTERM).expect("SIGTERM delivery should succeed");
}
