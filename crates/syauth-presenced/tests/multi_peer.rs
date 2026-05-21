// Journey: specs/journeys/JOURNEY-S-005-multi-peer-bonds-reload.md
// Roadmap row: specs/unlock-proximity/ROADMAP.md Step S-005.
//
// Integration tests for the multi-peer reload pipeline:
//   TC-01 — three_bonds_advertise_three_uuids: three bonds on disk,
//           one reload, FakePeripheral records three peers and one
//           three-element session_uuid_calls() set.
//   TC-02 — reload_removes_revoked_bond: mark one of the three
//           bonds revoked, trigger reload via the mpsc channel,
//           assert the peripheral converges on two peers and the
//           next set_session_uuids set has two elements.
//   TC-03 — sighup_reloads_bond_set: actually fire SIGHUP via
//           `nix::sys::signal::kill(getpid(), SIGHUP)`, prove the
//           signal-handler-driven mpsc push reaches the
//           orchestrator's reload loop, peripheral converges.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant as StdInstant, SystemTime, UNIX_EPOCH},
};

use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use syauth_core::{Bond, BondStatus, BondStore, bond::PUBKEY_LEN, peer_id_from_pubkey};
use syauth_presenced::{Orchestrator, ReloadCommand, ReloadTrigger};
use syauth_transport::{BOND_KEY_BYTES, FakePeripheral, Peripheral, session_uuid_for};
use tempfile::TempDir;
use time::OffsetDateTime;
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::oneshot,
};
use uuid::Uuid;

/// Three deterministic pubkeys so the fixture peer ids are stable.
const PUBKEY_A: [u8; PUBKEY_LEN] = [0xA1; PUBKEY_LEN];
const PUBKEY_B: [u8; PUBKEY_LEN] = [0xB2; PUBKEY_LEN];
const PUBKEY_C: [u8; PUBKEY_LEN] = [0xC3; PUBKEY_LEN];

/// Three deterministic bond keys. Distinct so the rotating UUIDs are
/// distinct and the test asserts on a 3-element `HashSet<Uuid>`.
const BOND_KEY_A: [u8; BOND_KEY_BYTES] = [0xAA; BOND_KEY_BYTES];
const BOND_KEY_B: [u8; BOND_KEY_BYTES] = [0xBB; BOND_KEY_BYTES];
const BOND_KEY_C: [u8; BOND_KEY_BYTES] = [0xCC; BOND_KEY_BYTES];

/// Maximum wall-clock budget for the polling loop in TC-03 (the OS
/// signal-delivery model has no synchronous "done" hook).
const POLL_BUDGET: Duration = Duration::from_secs(2);

/// Cadence at which the test polls `FakePeripheral::peers()` while
/// waiting for the reload to converge.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Per-peer bond-key file extension under `<keys_dir>/<peer_id>.bin`.
/// Mirrors `runtime::BOND_KEY_FILE_EXT`; held literal here so the
/// test does not depend on a `pub` re-export of that constant.
const BOND_KEY_FILE_EXT: &str = ".bin";

/// Build a `Bond` fixture without going through the pair flow.
fn fixture_bond(pubkey: [u8; PUBKEY_LEN], name: &str) -> Bond {
    Bond {
        peer_id: peer_id_from_pubkey(&pubkey),
        pubkey,
        name: name.to_owned(),
        created_at: OffsetDateTime::UNIX_EPOCH,
        status: BondStatus::Bonded,
    }
}

/// One bonded peer record on disk: bond entry in `bonds.toml` AND
/// `<keys_dir>/<peer_id>.bin` with the matching 32-byte key. The
/// orchestrator's reload pipeline reads both, so the fixture must
/// write both atomically.
struct OnDiskPeer {
    bond: Bond,
    bond_key: [u8; BOND_KEY_BYTES],
}

fn write_bonds(bonds_file: &Path, keys_dir: &Path, peers: &[OnDiskPeer]) {
    let mut store = BondStore::empty();
    for p in peers {
        store.add(p.bond.clone()).expect("add bond to store");
    }
    store.save(bonds_file).expect("save bonds file");
    for p in peers {
        let path = keys_dir.join(format!("{}{BOND_KEY_FILE_EXT}", p.bond.peer_id));
        fs::write(&path, p.bond_key).expect("write bond key");
    }
}

fn make_fixture_three(td: &TempDir) -> (PathBuf, PathBuf, Vec<OnDiskPeer>) {
    let parent = td.path().join("var-lib-syauth");
    fs::create_dir_all(&parent).expect("mkdir parent");
    // Tighten parent dir to BOND_DIR_MODE so BondStore::save accepts.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).expect("chmod 0o700");
    }
    let bonds_file = parent.join("bonds.toml");
    let keys_dir = parent.join("keys");
    fs::create_dir_all(&keys_dir).expect("mkdir keys");
    let peers = vec![
        OnDiskPeer {
            bond: fixture_bond(PUBKEY_A, "work-pixel"),
            bond_key: BOND_KEY_A,
        },
        OnDiskPeer {
            bond: fixture_bond(PUBKEY_B, "personal-pixel"),
            bond_key: BOND_KEY_B,
        },
        OnDiskPeer {
            bond: fixture_bond(PUBKEY_C, "spare-pixel"),
            bond_key: BOND_KEY_C,
        },
    ];
    write_bonds(&bonds_file, &keys_dir, &peers);
    (bonds_file, keys_dir, peers)
}

/// Build an orchestrator under `tokio::time::pause` for the
/// reload-only tests. `start` is one minute in the future so no
/// minute tick fires during the test.
fn build_orchestrator(peripheral: Arc<dyn Peripheral + Send + Sync>, bonds_file: PathBuf, keys_dir: PathBuf) -> Arc<Orchestrator> {
    let start = tokio::time::Instant::now() + Duration::from_secs(syauth_presenced::SECONDS_PER_MINUTE);
    Arc::new(Orchestrator::with_peers(peripheral, Vec::new(), bonds_file, keys_dir, start))
}

/// Compute the current minute integer (matches the orchestrator's
/// `minute_index` arithmetic).
fn current_minute() -> i64 {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock after epoch").as_secs();
    i64::try_from(secs / syauth_presenced::SECONDS_PER_MINUTE).expect("minute fits in i64")
}

/// Wait until `predicate` returns true or `POLL_BUDGET` elapses.
async fn wait_until<F: Fn() -> bool>(predicate: F) -> bool {
    let deadline = StdInstant::now() + POLL_BUDGET;
    while StdInstant::now() < deadline {
        if predicate() {
            return true;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    predicate()
}

/// TC-01: three bonds on disk + one `reload_bonds` call ⇒
/// `FakePeripheral::peers()` length 3 + last `session_uuid_calls`
/// set carries three UUIDs.
#[tokio::test]
async fn three_bonds_advertise_three_uuids() {
    let td = TempDir::new().expect("tempdir");
    let (bonds_file, keys_dir, peers) = make_fixture_three(&td);
    let fake: Arc<FakePeripheral> = FakePeripheral::new();
    let peripheral: Arc<dyn Peripheral + Send + Sync> = fake.clone();
    let orchestrator = build_orchestrator(peripheral, bonds_file.clone(), keys_dir.clone());

    let store = BondStore::load(&bonds_file).expect("reload store");
    orchestrator.reload_bonds(&store).await;

    let registered = fake.peers();
    assert_eq!(registered.len(), 3, "expected three registered peers, got {registered:?}");
    let expected_ids: HashSet<String> = peers.iter().map(|p| p.bond.peer_id.clone()).collect();
    let actual_ids: HashSet<String> = registered.into_iter().collect();
    assert_eq!(actual_ids, expected_ids, "peer ids must match the on-disk bond set");

    let calls = fake.session_uuid_calls();
    let last = calls.last().expect("at least one set_session_uuids call after reload");
    // Advertised set carries one entry per bonded peer PLUS the
    // always-present pair-mode UUID (derived from the zero bond
    // key for the current minute) so a phone can pair against this
    // host without needing an existing bond.
    assert_eq!(
        last.len(),
        4,
        "advertised UUID set must carry one entry per bonded peer plus the pair-mode UUID, got {last:?}"
    );
    // The minute integer used inside the orchestrator may differ
    // from `current_minute()` by one if the test crossed a wall-
    // clock minute boundary mid-reload. Accept a 3-minute window
    // around `current_minute()` so the assertion is robust without
    // freezing the system clock.
    let minute = current_minute();
    let zero_bond = [0u8; syauth_transport::BOND_KEY_BYTES];
    let mut window: HashSet<Uuid> = HashSet::new();
    for m in (minute - 1)..=(minute + 1) {
        for p in &peers {
            window.insert(Uuid::from_bytes(session_uuid_for(&p.bond_key, m)));
        }
        window.insert(Uuid::from_bytes(session_uuid_for(&zero_bond, m)));
    }
    for u in last.iter() {
        assert!(window.contains(u), "advertised UUID {u} not in expected window");
    }
    assert!(last.is_subset(&window), "every advertised UUID must be in the expected window");
}

/// TC-02: with three bonds advertised, mark one revoked in the
/// store and push a `ReloadCommand` onto the orchestrator's mpsc
/// channel. The peripheral converges on two peers and the next
/// `session_uuid_calls` set has length 2.
#[tokio::test]
async fn reload_removes_revoked_bond() {
    let td = TempDir::new().expect("tempdir");
    let (bonds_file, keys_dir, peers) = make_fixture_three(&td);
    let fake: Arc<FakePeripheral> = FakePeripheral::new();
    let peripheral: Arc<dyn Peripheral + Send + Sync> = fake.clone();
    let orchestrator = build_orchestrator(peripheral, bonds_file.clone(), keys_dir.clone());

    // Phase A: cold-load three bonds.
    let store = BondStore::load(&bonds_file).expect("phase A load");
    orchestrator.reload_bonds(&store).await;
    assert_eq!(fake.peers().len(), 3);

    // Phase B: mark the third bond revoked in the store and persist
    // it. The orchestrator's reload pipeline re-reads bonds.toml on
    // every reload, so the on-disk truth drives the diff.
    let revoke_id = peers[2].bond.peer_id.clone();
    let mut store = BondStore::load(&bonds_file).expect("phase B load");
    store.mark_revoked(&revoke_id, "phone-lost").expect("mark revoked");
    store.save(&bonds_file).expect("save revoked store");

    let reload_tx = orchestrator.reload_sender();
    reload_tx
        .send(ReloadCommand {
            trigger: ReloadTrigger::Rpc,
        })
        .await
        .expect("push reload");

    // Drive the orchestrator's reload loop so the command is
    // processed. The same `Arc<Orchestrator>` is held by the test;
    // spawning `.run()` on a clone gives the reload consumer a
    // chance to drain the mpsc channel.
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let run_handle = tokio::spawn(Arc::clone(&orchestrator).run(shutdown_rx));

    let converged = wait_until(|| fake.peers().len() == 2 && fake.peers().iter().all(|p| p != &revoke_id)).await;
    let _ = shutdown_tx.send(());
    let _ = run_handle.await;

    assert!(
        converged,
        "peripheral did not converge on 2 peers within budget; saw {:?}",
        fake.peers()
    );
    let calls = fake.session_uuid_calls();
    let last = calls.last().expect("at least one set_session_uuids after reload");
    // Two remaining bonded peers + the always-present pair-mode UUID.
    assert_eq!(
        last.len(),
        3,
        "advertised UUID set must carry one entry per remaining bonded peer plus the pair-mode UUID, got {last:?}"
    );
    // The revoked peer's UUID must not appear in the last advertised
    // set under any minute in the surrounding 2-minute window.
    let minute = current_minute();
    let mut revoked_window: HashSet<Uuid> = HashSet::new();
    for m in (minute - 1)..=(minute + 1) {
        revoked_window.insert(Uuid::from_bytes(session_uuid_for(&peers[2].bond_key, m)));
    }
    for u in last.iter() {
        assert!(
            !revoked_window.contains(u),
            "revoked peer's UUID {u} must not appear in advertised set"
        );
    }
}

/// TC-03: fire SIGHUP at the test's own PID; prove the
/// signal-handler-driven mpsc push reaches the orchestrator's
/// reload loop. To avoid depending on the binary's `runtime::run`
/// (which constructs a `PersistentPeripheral` against a real BlueZ
/// adapter), the test installs its own `signal(SignalKind::hangup())`
/// handler and forwards each hangup receipt to the orchestrator's
/// reload sender — the same wiring `runtime::run` uses.
#[tokio::test]
async fn sighup_reloads_bond_set() {
    let td = TempDir::new().expect("tempdir");
    // Cold-start with zero bonds; the SIGHUP-driven reload picks
    // up the three bonds written below.
    let parent = td.path().join("var-lib-syauth");
    fs::create_dir_all(&parent).expect("mkdir parent");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).expect("chmod 0o700");
    }
    let bonds_file = parent.join("bonds.toml");
    let keys_dir = parent.join("keys");
    fs::create_dir_all(&keys_dir).expect("mkdir keys");

    let fake: Arc<FakePeripheral> = FakePeripheral::new();
    let peripheral: Arc<dyn Peripheral + Send + Sync> = fake.clone();
    let orchestrator = build_orchestrator(peripheral, bonds_file.clone(), keys_dir.clone());

    // Mirror the production SIGHUP wiring (`runtime::wait_for_reason`):
    // install a tokio SIGHUP handler and forward each receipt to the
    // orchestrator's reload sender.
    let mut sighup = signal(SignalKind::hangup()).expect("install SIGHUP handler");
    let reload_tx = orchestrator.reload_sender();
    let forward_handle = tokio::spawn(async move {
        while sighup.recv().await.is_some() {
            if reload_tx
                .send(ReloadCommand {
                    trigger: ReloadTrigger::Sighup,
                })
                .await
                .is_err()
            {
                return;
            }
        }
    });

    // Spawn the orchestrator's run loop so it drains reload mpsc.
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let run_handle = tokio::spawn(Arc::clone(&orchestrator).run(shutdown_rx));

    // Yield once so the SIGHUP handler is fully armed before we
    // deliver the signal.
    tokio::task::yield_now().await;

    // Phase A: write the three bonds AFTER orchestrator startup so
    // the cold-start hydration window is intentionally empty; the
    // reload is the only path that can populate the peer set.
    let peers = vec![
        OnDiskPeer {
            bond: fixture_bond(PUBKEY_A, "work-pixel"),
            bond_key: BOND_KEY_A,
        },
        OnDiskPeer {
            bond: fixture_bond(PUBKEY_B, "personal-pixel"),
            bond_key: BOND_KEY_B,
        },
        OnDiskPeer {
            bond: fixture_bond(PUBKEY_C, "spare-pixel"),
            bond_key: BOND_KEY_C,
        },
    ];
    write_bonds(&bonds_file, &keys_dir, &peers);

    // Fire SIGHUP at the test's own PID.
    let pid = Pid::from_raw(i32::try_from(std::process::id()).expect("pid fits in i32"));
    kill(pid, Signal::SIGHUP).expect("SIGHUP delivery");

    let expected_ids: HashSet<String> = peers.iter().map(|p| p.bond.peer_id.clone()).collect();
    let converged = wait_until(|| {
        let current: HashSet<String> = fake.peers().into_iter().collect();
        current == expected_ids
    })
    .await;

    let _ = shutdown_tx.send(());
    forward_handle.abort();
    let _ = run_handle.await;

    assert!(
        converged,
        "peripheral did not converge on three SIGHUP-loaded peers within {POLL_BUDGET:?}; saw {:?}",
        fake.peers()
    );
}
