//! S-008 integration test: spawn the real `syauth-presenced` binary
//! in its hidden `--peripheral=fake` mode, point `auth::authenticate`
//! at the same tempdir socket, and drive a full
//! challenge → response → `PAM_SUCCESS` round-trip.
//!
//! Journey: specs/journeys/JOURNEY-S-008-pam-unix-socket-client.md TC-14.
//!
//! Why a real-binary test (not just an in-process mock): the unit
//! tests in `crates/syauth-pam/src/auth.rs::tests` cover the
//! Unix-socket client surface against a hand-rolled `MockDaemonHandle`.
//! That harness proves the PAM module speaks the wire format correctly
//! but does NOT prove the daemon binary itself wires up
//! `Request::Challenge` → `Orchestrator::issue_challenge` →
//! `Peripheral::notify_challenge` → `Peripheral::wait_for_response` →
//! `Response::Challenge { ok: true, signature: <bytes>, reason:
//! "ok" }`. The integration test closes that gap by exercising the
//! shipped binary end-to-end against a `FakePeripheral` test seam.

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

use ed25519_dalek::Signer;
use pam_syauth::{
    auth::{self, AuthOutcome},
    config::Config,
};
use syauth_core::{
    BOND_KEY_BYTES, Bond, BondStatus, BondStore, Frame, NONCE_LEN, SYAUTH_WIRE_VERSION_V1, SigningKey, TAG_LEN, peer_id_from_pubkey,
};
use tempfile::TempDir;
use time::OffsetDateTime;

/// Path to the `syauth-presenced` binary. Cargo does not set
/// `CARGO_BIN_EXE_*` for cross-crate binaries (the variable is only
/// defined for binaries in the same crate as the integration test),
/// so we resolve the path manually by walking up from this crate's
/// manifest dir into the workspace `target/<profile>/` directory.
fn daemon_binary_path() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        // CARGO_MANIFEST_DIR is `<workspace>/crates/syauth-pam` for
        // this test target.
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir
            .parent()
            .and_then(Path::parent)
            .expect("workspace root above crates/syauth-pam");
        let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
        let candidate = workspace_root.join("target").join(profile).join("syauth-presenced");
        assert!(
            candidate.exists(),
            "daemon binary not built at {}; run `cargo build -p syauth-presenced` first or invoke this test via `cargo test --workspace`",
            candidate.display()
        );
        candidate
    })
    .as_path()
}

/// Max wall-clock the test waits for the daemon to bind its socket
/// before failing the test (the daemon's accept loop typically binds
/// in < 100 ms on a CI runner).
const SOCKET_READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Polling interval while waiting for the daemon's socket to appear.
const SOCKET_READY_POLL: Duration = Duration::from_millis(25);

/// Pinned 32-byte signing seed for the synthetic phone. Deterministic
/// so the test reads its own signed-response bytes back.
const TEST_SIGNING_SEED: [u8; 32] = [
    0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD, 0xAE, 0xAF, 0xB0, 0xB1, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6,
    0xB7, 0xB8, 0xB9, 0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF, 0xC0,
];

/// Pinned bond key for the synthetic phone.
const TEST_BOND_KEY: [u8; BOND_KEY_BYTES] = [0x5A; BOND_KEY_BYTES];

/// Tempdir with 0o700 perms — `BondStore::save` refuses looser parent
/// permissions (SPEC §4.4).
fn make_tempdir_0o700() -> TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut perm = std::fs::metadata(tmp.path()).expect("stat").permissions();
    perm.set_mode(0o700);
    std::fs::set_permissions(tmp.path(), perm).expect("chmod");
    tmp
}

/// RAII guard around a spawned `syauth-presenced` child process.
struct DaemonProcess {
    child: Child,
}

impl DaemonProcess {
    fn spawn(args: Vec<String>) -> Self {
        let child = Command::new(daemon_binary_path())
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn syauth-presenced binary");
        Self { child }
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Block until `socket_path` exists or `SOCKET_READY_TIMEOUT` elapses.
fn wait_for_socket(socket_path: &Path) {
    let started = Instant::now();
    while !socket_path.exists() {
        if started.elapsed() > SOCKET_READY_TIMEOUT {
            panic!(
                "daemon socket did not appear at {} within {SOCKET_READY_TIMEOUT:?}",
                socket_path.display()
            );
        }
        thread::sleep(SOCKET_READY_POLL);
    }
}

/// Deterministic nonce wired into the daemon via
/// `--test-fixed-nonce`. Sixteen bytes of `0xAB`. The PAM client
/// sends a separate (randomly-generated) nonce on the wire but the
/// daemon ignores it — the daemon generates its own nonce server-
/// side per SPEC §3 #6. With `--test-fixed-nonce` set, that
/// server-side nonce becomes deterministic so the test can pre-sign
/// the response body the orchestrator will verify.
const TEST_FIXED_NONCE: [u8; NONCE_LEN] = [0xABu8; NONCE_LEN];

/// Produce a hex-encoded Ed25519 signature over the challenge body
/// the orchestrator will construct under `--test-fixed-nonce`. The
/// orchestrator's challenge frame is `Frame { version =
/// SYAUTH_WIRE_VERSION_V1, nonce = TEST_FIXED_NONCE, payload =
/// Vec::new(), tag = [0u8; TAG_LEN] }`. The verifier signs against
/// `body_bytes()` of the same frame.
fn signature_over_fixed_nonce_frame() -> Vec<u8> {
    let sk = SigningKey::from_bytes(&TEST_SIGNING_SEED);
    let challenge = Frame {
        version: SYAUTH_WIRE_VERSION_V1,
        nonce: TEST_FIXED_NONCE,
        payload: Vec::new(),
        tag: [0u8; TAG_LEN],
    };
    let body = challenge.body_bytes().expect("encode challenge body");
    sk.sign(&body).to_bytes().to_vec()
}

/// Drive the daemon binary end-to-end and assert the PAM client
/// reaches `AuthOutcome::Success`.
///
/// The daemon spawns with `--peripheral=fake`,
/// `--test-fixed-nonce <hex>`, and `--inject-response
/// <peer_id>:<sig-hex>`. The PAM client opens the socket, sends
/// `Request::Challenge`, and the daemon's orchestrator runs
/// `notify → wait_for_response → verify_frame → respond`. The
/// signature verifies against the deterministic nonce, so the
/// daemon returns `Response::Challenge { ok: true, signature:
/// Some(<bytes>), reason: "ok" }` and PAM returns
/// `AuthOutcome::Success`.
#[test]
fn end_to_end_against_real_daemon_binary() {
    let tmp = make_tempdir_0o700();
    let socket_path = tmp.path().join("auth.sock");
    let bonds_path = tmp.path().join("bonds.toml");
    let keys_dir = tmp.path().join("keys");
    let audit_log = tmp.path().join("last.log");
    let pidfile = tmp.path().join("presenced.pid");
    std::fs::create_dir_all(&keys_dir).expect("mkdir keys");
    // Mode 0o700 on the keys dir (same SPEC §4.4 perms as the bond
    // dir).
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut p = std::fs::metadata(&keys_dir).expect("stat keys").permissions();
        p.set_mode(0o700);
        std::fs::set_permissions(&keys_dir, p).expect("chmod keys");
    }

    // Build a bond record whose `pubkey` is the test signing key's
    // verifying half, so the daemon's verifier accepts the signature
    // bytes against the bond.
    let sk = SigningKey::from_bytes(&TEST_SIGNING_SEED);
    let pk = sk.verifying_key();
    let peer_id = peer_id_from_pubkey(pk.as_bytes());

    let mut store = BondStore::empty();
    store
        .add(Bond {
            peer_id: peer_id.clone(),
            pubkey: *pk.as_bytes(),
            name: "integration-phone".to_string(),
            created_at: OffsetDateTime::now_utc(),
            status: BondStatus::Bonded,
        })
        .expect("add bond");
    store.save(&bonds_path).expect("save bonds.toml");

    // Drop the bond key file at `<keys_dir>/<peer_id>.bin` with the
    // 0600 perm the daemon's `load_bond_key` requires.
    let key_file = keys_dir.join(format!("{peer_id}.bin"));
    std::fs::write(&key_file, TEST_BOND_KEY).expect("write bond key");
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut p = std::fs::metadata(&key_file).expect("stat key file").permissions();
        p.set_mode(0o600);
        std::fs::set_permissions(&key_file, p).expect("chmod key file");
    }

    // Pre-seed the FakePeripheral's response queue with the
    // signature the daemon's verifier will accept against the
    // deterministic `TEST_FIXED_NONCE` challenge.
    let signed_bytes = signature_over_fixed_nonce_frame();
    let inject_arg = format!("{peer_id}:{}", hex::encode(&signed_bytes));
    let nonce_hex = hex::encode(TEST_FIXED_NONCE);

    let args: Vec<String> = vec![
        "--socket".to_string(),
        socket_path.to_string_lossy().into_owned(),
        "--bonds-file".to_string(),
        bonds_path.to_string_lossy().into_owned(),
        "--keys-dir".to_string(),
        keys_dir.to_string_lossy().into_owned(),
        "--audit-log".to_string(),
        audit_log.to_string_lossy().into_owned(),
        "--pidfile".to_string(),
        pidfile.to_string_lossy().into_owned(),
        "--peripheral".to_string(),
        "fake".to_string(),
        "--inject-response".to_string(),
        inject_arg,
        "--test-fixed-nonce".to_string(),
        nonce_hex,
    ];

    let _daemon = DaemonProcess::spawn(args);
    wait_for_socket(&socket_path);

    let cfg = Config::for_tests(tmp.path()).with_socket_path(socket_path.clone());
    let outcome = auth::authenticate(&cfg);

    match &outcome {
        AuthOutcome::Success { peer_id: got_id } => {
            assert_eq!(*got_id, peer_id);
        }
        other => panic!("expected Success, got {other:?}"),
    }
}
