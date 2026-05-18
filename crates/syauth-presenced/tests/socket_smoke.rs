// Journey: specs/journeys/JOURNEY-S-002-cbor-unix-socket-rpc-stub.md
// Roadmap row: specs/unlock-proximity/ROADMAP.md Step S-002.
//
// Smoke tests for the Unix-socket RPC server:
//   TC-01 — challenge_request_returns_stub: bind, connect, send a
//           well-framed Challenge, receive the stub response, assert
//           the socket file mode is 0o600.
//   TC-02 — rejects_non_matching_peer_credential: configure the
//           daemon with an unreachable expected uid (`Some(0)` for a
//           non-root test process) and assert the connection is
//           EOF'd without a response body.
//   TC-03 — concurrent_accept_cap_enforced: open four connections,
//           leave their responses unread so each per-connection task
//           holds a semaphore permit, open a fifth connection and
//           assert its read times out, then drop one of the first
//           four and assert the fifth's read now returns the stub.

use std::{
    os::unix::fs::PermissionsExt as _,
    path::PathBuf,
    time::{Duration, Instant},
};

use syauth_presenced::{LISTEN_MODE, Request, Response, STUB_CHALLENGE_REASON, ServeConfig, read_frame, serve, write_frame};
use tempfile::TempDir;
use tokio::{
    net::UnixStream,
    sync::oneshot,
    task::JoinHandle,
    time::{sleep, timeout},
};

/// Test-side wall-clock budget for the daemon to bind its socket
/// before the first client connects. 5 seconds is the same budget
/// `lifecycle_smoke.rs` uses for pidfile appearance.
const BIND_WAIT_BUDGET: Duration = Duration::from_secs(5);

/// Test-side wall-clock budget for a single connect/send/recv
/// round-trip against the stub responder.
const ROUND_TRIP_BUDGET: Duration = Duration::from_secs(1);

/// Deadline the queued-fifth connection's read must NOT complete
/// within. 200 ms is the same window the journey doc's "5th queued
/// behind semaphore" assertion uses.
const QUEUED_READ_DEADLINE: Duration = Duration::from_millis(200);

/// Polling cadence for the bind-readiness poll.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Concurrent-accept cap mirrored here so test math is grep-able
/// against the SPEC §7 T-Daemon-DoS row. Kept literal (4) rather
/// than re-imported because the test asserts the cap *value*, not
/// the binding name — a refactor that changes the cap should make
/// the test fail loudly.
const SPEC_CONCURRENT_CAP: usize = 4;

/// 16-byte deterministic nonce used by every Challenge request the
/// smoke tests build.
const TEST_NONCE: [u8; 16] = [
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
];

/// In-process daemon spawned by each test. Owns the tempdir +
/// shutdown handle so `Drop` tears down the listener cleanly.
struct Daemon {
    socket: PathBuf,
    shutdown: Option<oneshot::Sender<()>>,
    handle: Option<JoinHandle<Result<(), syauth_presenced::ServeError>>>,
    #[allow(dead_code)]
    tempdir: TempDir,
}

impl Daemon {
    async fn spawn(expected_uid: Option<u32>) -> Self {
        let tempdir = tempfile::Builder::new()
            .prefix("syauth-presenced-socket-smoke-")
            .tempdir()
            .expect("tempdir create");
        let socket = tempdir.path().join("auth.sock");
        let (tx, rx) = oneshot::channel::<()>();
        let serve_config = ServeConfig {
            socket_path: socket.clone(),
            expected_uid,
            reload_tx: None,
            // No orchestrator wired for the S-002 socket smoke; the
            // dispatcher preserves the S-002 stub semantics
            // (`reason="not-implemented"`) on this path so the
            // existing assertions stay green.
            orchestrator: None,
            test_fixed_nonce: None,
            started_at: None,
        };
        let handle = tokio::spawn(async move {
            serve(serve_config, async move {
                let _ = rx.await;
            })
            .await
        });
        let deadline = Instant::now() + BIND_WAIT_BUDGET;
        while Instant::now() < deadline {
            if socket.exists() {
                break;
            }
            sleep(POLL_INTERVAL).await;
        }
        assert!(socket.exists(), "socket {socket:?} did not appear within {BIND_WAIT_BUDGET:?}");
        Self {
            socket,
            shutdown: Some(tx),
            handle: Some(handle),
            tempdir,
        }
    }

    async fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Best-effort sync teardown — the explicit `shutdown().await`
        // in each test runs the clean path. Drop only fires if a
        // panic/early-return short-circuits the test.
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

fn current_uid() -> u32 {
    nix::unistd::geteuid().as_raw()
}

async fn send_challenge(stream: &mut UnixStream) -> Result<(), syauth_presenced::FrameError> {
    let request = Request::Challenge {
        peer_id: "test-peer".to_string(),
        nonce: TEST_NONCE.to_vec(),
    };
    write_frame(stream, &request).await
}

#[tokio::test]
async fn challenge_request_returns_stub() {
    let mut daemon = Daemon::spawn(Some(current_uid())).await;

    let mut stream = UnixStream::connect(&daemon.socket).await.expect("connect succeeds");
    send_challenge(&mut stream).await.expect("send succeeds");
    let response: Response = timeout(ROUND_TRIP_BUDGET, read_frame(&mut stream))
        .await
        .expect("response within budget")
        .expect("decode succeeds");

    match response {
        Response::Challenge { ok, signature, reason } => {
            assert!(!ok, "stub response must have ok=false");
            assert!(signature.is_none(), "stub response must have no signature");
            assert_eq!(reason, STUB_CHALLENGE_REASON, "stub reason must match SPEC");
        }
        other => panic!("expected Response::Challenge, got {other:?}"),
    }

    let meta = std::fs::metadata(&daemon.socket).expect("socket metadata");
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode, LISTEN_MODE, "socket file mode must be {LISTEN_MODE:o}");

    daemon.shutdown().await;
}

#[tokio::test]
async fn rejects_non_matching_peer_credential() {
    // `current_uid() + 1` is guaranteed to be a different UID from
    // the test process so the ACL fires deterministically. The smoke
    // test does not need root and does not need to fork; the daemon
    // is configured with `expected_uid = Some(other_uid)` and rejects
    // the test process's well-formed connection on `SO_PEERCRED`.
    let other_uid = current_uid().wrapping_add(1);
    let mut daemon = Daemon::spawn(Some(other_uid)).await;

    let mut stream = UnixStream::connect(&daemon.socket).await.expect("connect succeeds");
    // The daemon's per-connection handler runs the ACL FIRST and
    // drops the connection without reading. Writing a frame on our
    // side may or may not flush (the socket buffer may hold it), so
    // the assertion below is on the READ side: the daemon never
    // sends a response, and the read sees EOF.
    let _ = send_challenge(&mut stream).await;
    let read_result = timeout(ROUND_TRIP_BUDGET, read_frame::<_, Response>(&mut stream)).await;
    match read_result {
        Ok(Err(syauth_presenced::FrameError::Io(err))) => {
            // The daemon closed the connection without writing a
            // response. Depending on whether the test's send_frame
            // was still mid-flush when the daemon's `close` raced
            // it, the kernel reports either UnexpectedEof (clean
            // FIN) or ConnectionReset (RST after a TCP-RST-style
            // half-close from the peer side). Both are equally
            // valid evidence the ACL fired and dropped the
            // connection.
            let kind = err.kind();
            assert!(
                matches!(kind, std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset),
                "expected EOF / ConnectionReset on ACL drop, got {err:?}"
            );
        }
        Ok(other) => panic!("expected EOF on ACL drop, got {other:?}"),
        Err(_) => panic!("expected EOF within budget, got timeout"),
    }

    // Daemon is still healthy and refuses a second connection from
    // the same client the same way — the rejection path didn't
    // crash the accept loop.
    let mut second = UnixStream::connect(&daemon.socket).await.expect("second connect succeeds");
    let _ = send_challenge(&mut second).await;
    let second_read = timeout(ROUND_TRIP_BUDGET, read_frame::<_, Response>(&mut second)).await;
    assert!(
        matches!(second_read, Ok(Err(_))),
        "second connection also EOF'd, got {second_read:?}"
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn concurrent_accept_cap_enforced() {
    let mut daemon = Daemon::spawn(Some(current_uid())).await;

    // The cap is enforced by acquiring a semaphore permit BEFORE
    // spawning the per-connection task. With the permit held by the
    // task, the accept loop blocks on `Semaphore::acquire_owned`
    // until one of the four in-flight tasks completes.
    //
    // To park each task indefinitely (holding its permit), the
    // smoke client connects but DOES NOT send a complete frame.
    // The daemon's per-task `read_frame` is blocked inside
    // `read_exact(&mut len_buf)` waiting for the 4-byte length
    // prefix. As long as we keep the connection open without
    // sending those 4 bytes, the task is parked and the permit
    // stays consumed.
    let mut held: Vec<UnixStream> = Vec::with_capacity(SPEC_CONCURRENT_CAP);
    for _ in 0..SPEC_CONCURRENT_CAP {
        let stream = UnixStream::connect(&daemon.socket).await.expect("held connect succeeds");
        held.push(stream);
    }
    // Give the daemon a moment to accept each connection and
    // consume each permit before opening the 5th. Without this
    // settle the 5th's connect may race the accept loop and
    // produce a flaky assertion.
    sleep(QUEUED_READ_DEADLINE).await;

    let mut queued = UnixStream::connect(&daemon.socket).await.expect("queued connect succeeds");
    send_challenge(&mut queued).await.expect("queued send succeeds");
    let queued_read = timeout(QUEUED_READ_DEADLINE, read_frame::<_, Response>(&mut queued)).await;
    assert!(
        queued_read.is_err(),
        "5th connection's read must time out (queued behind semaphore); got {queued_read:?}"
    );

    // Release one of the four parked tasks by dropping its client
    // stream. The daemon's `read_exact` sees EOF, the handler
    // returns, the `OwnedSemaphorePermit` drops, and the accept
    // loop's `acquire_owned` for the 5th completes.
    let releasable = held.pop().expect("held has at least one entry");
    drop(releasable);

    // The 5th's permit is now available; its handler reads the
    // already-sent Challenge frame and writes the stub response,
    // which we read within the budget.
    let unblocked: Response = timeout(ROUND_TRIP_BUDGET, read_frame(&mut queued))
        .await
        .expect("5th's response within budget after permit release")
        .expect("5th's decode succeeds");
    match unblocked {
        Response::Challenge { ok, reason, .. } => {
            assert!(!ok, "stub response is ok=false");
            assert_eq!(reason, STUB_CHALLENGE_REASON);
        }
        other => panic!("expected Response::Challenge, got {other:?}"),
    }

    daemon.shutdown().await;
}
