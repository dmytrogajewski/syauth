//! S-011 integration test: `syauth pair` flow against an injected mock
//! [`PairBackend`].
//!
//! Per the DoD, this test drives the public surface of the `syauth_cli::pair`
//! module against a `MockPairBackend` that emits LESC simulation events. It
//! NEVER touches a real Bluetooth adapter — the integration is purely
//! in-process tokio + library calls.
//!
//! Coverage matrix:
//!
//! | TC  | DoD line                                  | Scenario                                                                    |
//! |-----|-------------------------------------------|-----------------------------------------------------------------------------|
//! | 01  | "syauth pair ... writes the bond"          | Golden: golden LESC, OOB accepted, `BondStore` gains exactly one entry.    |
//! | 02  | "Refuses ... without LE Secure Connections" | Adapter capability flag false → typed `LescUnsupported` error.             |
//! | 03  | "On timeout (default 60 s) ... no partial bond" | Mock LESC never resolves; `--timeout-secs 1` → `Revoked { Timeout }`, file byte-equal. |
//! | 04  | "non-interactive when `--yes` is passed" / operator-rejects path | Mock supplies `N`; result is `Revoked { OperatorReject }`, file byte-equal. |
//! | 05  | "syauth list shows the new peer immediately" | TC-01 store passed to `render_list_to` ⇒ output contains the peer name.    |
//! | 06  | "ambiguous --peer with --yes"               | Two candidates match `--peer pixel` + `--yes` → `AmbiguousPeer { ... }`.   |
//! | 07  | "--yes does not skip the LESC check"         | `LescUnsupported` is returned even when `--yes` is set.                    |
//!
//! All seven cases collectively satisfy the brief's "at least 4 cases" floor
//! (golden, LESC-unsupported, timeout, rejected-Y/N) plus three additional
//! coverage rows.

#![allow(clippy::expect_used)] // tests are allowed to expect()

use std::{
    fs,
    io::Cursor,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use syauth_cli::{
    list::render_list_to,
    pair::{
        AdapterInfo, LescOutcome, ListOpts, PairBackend, PairCandidate, PairError, PairOpts, PairingPhase, RevokeReason, bonds_path,
        run_pair_with_io,
    },
};
use syauth_core::{BondStore, peer_id_from_pubkey};
use tempfile::TempDir;
use tokio::{
    sync::Notify,
    time::{Duration as TokioDuration, sleep},
};

// ---------------------------------------------------------------------------
// Test fixtures.
// ---------------------------------------------------------------------------

const TEST_ADAPTER: &str = "hci0";
const TEST_PEER_NAME: &str = "alex-pixel";
const TEST_PEER_ADDR: &str = "AA:BB:CC:DD:EE:01";
const TEST_PEER_NAME_SPARE: &str = "alex-pixel-spare";
const TEST_PEER_ADDR_SPARE: &str = "AA:BB:CC:DD:EE:02";
const GOLDEN_PUBKEY: [u8; 32] = [0x21; 32];
const GOLDEN_BOND_KEY: [u8; 32] = [0x42; 32];
const GOLDEN_NUMERIC_CODE: u32 = 482_615;
const TIMEOUT_SECS_TIGHT: u64 = 1;
const TIMEOUT_SECS_LOOSE: u64 = 60;
const TIMEOUT_GRACE: TokioDuration = TokioDuration::from_millis(500);

fn temp_bond_dir() -> TempDir {
    TempDir::new().expect("tempdir")
}

/// `--bond-dir` resolved against `td`. Mirrors S-005's
/// `temp_bonds_path(td).parent()` convention so the parent directory mode
/// is 0o700 after `BondStore::save` runs (which `tempfile::TempDir` itself
/// does NOT guarantee — on most Linux setups `/tmp` is 0o755 and so is a
/// fresh `TempDir`).
fn bond_dir_path(td: &TempDir) -> std::path::PathBuf {
    td.path().join("syauth")
}

fn pair_opts(td: &TempDir, peer_filter: Option<&str>, timeout_secs: u64, yes: bool) -> PairOpts {
    PairOpts {
        adapter: TEST_ADAPTER.to_owned(),
        peer: peer_filter.map(str::to_owned),
        timeout_secs,
        bond_dir: bond_dir_path(td),
        yes,
        waybar: false,
        // S-019 added the hidden `--scripted-oob` flag; the S-011 cases
        // never set it (they exercise the interactive path or `--yes`).
        scripted_oob: None,
        // `--force` was added with the typed `PeerAlreadyBonded` error;
        // tests that exercise the `--force` path set this explicitly.
        force: false,
    }
}

fn list_opts(td: &TempDir) -> ListOpts {
    ListOpts {
        bond_dir: bond_dir_path(td),
    }
}

// ---------------------------------------------------------------------------
// Mock backend.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LescBehavior {
    /// Resolve immediately with the golden outcome.
    Golden,
    /// Block forever; the caller's `--timeout-secs` must fire.
    HangForever,
}

#[derive(Debug, Clone)]
struct MockConfig {
    supports_lesc: bool,
    candidates: Vec<PairCandidate>,
    lesc_behavior: LescBehavior,
}

impl MockConfig {
    fn golden() -> Self {
        Self {
            supports_lesc: true,
            candidates: vec![PairCandidate {
                name: TEST_PEER_NAME.to_owned(),
                address: TEST_PEER_ADDR.to_owned(),
            }],
            lesc_behavior: LescBehavior::Golden,
        }
    }
}

struct MockPairBackend {
    cfg: Mutex<MockConfig>,
    /// Signaled if `initiate_lesc_with_peer` is ever entered. Useful for the
    /// timeout test to assert the backend was actually reached.
    entered_lesc: Arc<Notify>,
}

impl MockPairBackend {
    fn new(cfg: MockConfig) -> Self {
        Self {
            cfg: Mutex::new(cfg),
            entered_lesc: Arc::new(Notify::new()),
        }
    }
}

#[async_trait]
impl PairBackend for MockPairBackend {
    async fn adapter_info(&self, adapter_id: &str) -> Result<AdapterInfo, PairError> {
        let supports_lesc = self.cfg.lock().expect("cfg lock").supports_lesc;
        Ok(AdapterInfo {
            name: adapter_id.to_owned(),
            supports_lesc,
        })
    }

    async fn scan_peers(&self) -> Result<Vec<PairCandidate>, PairError> {
        Ok(self.cfg.lock().expect("cfg lock").candidates.clone())
    }

    async fn initiate_lesc_with_peer(&self, _peer: &PairCandidate) -> Result<LescOutcome, PairError> {
        self.entered_lesc.notify_one();
        let behavior = self.cfg.lock().expect("cfg lock").lesc_behavior;
        match behavior {
            LescBehavior::Golden => Ok(LescOutcome {
                peer_pubkey: GOLDEN_PUBKEY,
                bond_key: GOLDEN_BOND_KEY,
                numeric_code: GOLDEN_NUMERIC_CODE,
            }),
            LescBehavior::HangForever => {
                // Sleep well past anything the test would set as
                // `--timeout-secs`. The caller's `tokio::time::timeout`
                // wrapper is what should fire here.
                sleep(TokioDuration::from_secs(3600)).await;
                unreachable!("hang-forever future must be canceled by timeout");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tiny stdio fakes.
// ---------------------------------------------------------------------------

async fn drive_pair(opts: &PairOpts, backend: &dyn PairBackend, stdin: &str) -> Result<(PairingPhase, String), PairError> {
    let mut reader = Cursor::new(stdin.as_bytes().to_vec());
    let mut writer: Vec<u8> = Vec::new();
    let phase = run_pair_with_io(opts, backend, &mut reader, &mut writer).await?;
    let out = String::from_utf8_lossy(&writer).into_owned();
    Ok((phase, out))
}

// ---------------------------------------------------------------------------
// TC-01: Golden — pair writes the bond, list shows it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pair_golden_flow_writes_bond_and_list_shows_it() {
    let td = temp_bond_dir();
    let backend = MockPairBackend::new(MockConfig::golden());
    let opts = pair_opts(&td, None, TIMEOUT_SECS_LOOSE, true);

    let (phase, out) = drive_pair(&opts, &backend, "").await.expect("golden pair must succeed");

    assert_eq!(phase, PairingPhase::Bonded);
    assert!(out.contains("LE Secure Connections: yes"), "adapter banner: {out}");
    assert!(out.contains(&format!("{GOLDEN_NUMERIC_CODE:06}")), "BT numeric code: {out}");
    assert!(out.contains("bonded"), "bonded banner: {out}");

    // Bond file landed and the peer is in it.
    let path = bonds_path(&bond_dir_path(&td));
    let store = BondStore::load(&path).expect("load");
    assert_eq!(store.list().len(), 1);
    let bond = &store.list()[0];
    assert_eq!(bond.name, TEST_PEER_NAME);
    assert_eq!(bond.peer_id, peer_id_from_pubkey(&GOLDEN_PUBKEY));

    // TC-05 piggy-back: `render_list_to` includes the new peer.
    let mut buf: Vec<u8> = Vec::new();
    let mut cur = Cursor::new(&mut buf);
    render_list_to(&mut cur, &store).expect("render");
    let listed = String::from_utf8(buf).expect("utf8");
    assert!(listed.contains(TEST_PEER_NAME), "list output must include peer name: {listed}");
    assert!(listed.contains(&bond.peer_id), "list output must include peer id: {listed}");

    // `run_list` would print the same content; we exercise it indirectly via
    // the library helper above to keep the test in-process.
    let _ = list_opts(&td);
}

// ---------------------------------------------------------------------------
// TC-02: Adapter without LE Secure Connections — refused.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pair_rejects_when_adapter_lacks_lesc() {
    let td = temp_bond_dir();
    let mut cfg = MockConfig::golden();
    cfg.supports_lesc = false;
    let backend = MockPairBackend::new(cfg);
    let opts = pair_opts(&td, None, TIMEOUT_SECS_LOOSE, false);

    let err = drive_pair(&opts, &backend, "").await.expect_err("must refuse without LESC");
    match err {
        PairError::LescUnsupported { adapter, hint } => {
            assert_eq!(adapter, TEST_ADAPTER);
            assert!(hint.contains("LE Secure Connections"), "hint names the issue: {hint}");
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert!(!bonds_path(&bond_dir_path(&td)).exists(), "no bond file written");
}

// TC-07: --yes does NOT bypass the LESC safety gate. Same as TC-02 but with
// `--yes` set — DoD: "--yes controls only the operator-facing y/N OOB
// prompt".
#[tokio::test]
async fn pair_rejects_when_adapter_lacks_lesc_even_with_yes() {
    let td = temp_bond_dir();
    let mut cfg = MockConfig::golden();
    cfg.supports_lesc = false;
    let backend = MockPairBackend::new(cfg);
    let opts = pair_opts(&td, None, TIMEOUT_SECS_LOOSE, true);

    let err = drive_pair(&opts, &backend, "")
        .await
        .expect_err("must refuse without LESC even with --yes");
    assert!(matches!(err, PairError::LescUnsupported { .. }));
    assert!(!bonds_path(&bond_dir_path(&td)).exists());
}

// ---------------------------------------------------------------------------
// TC-03: Timeout — state machine transitions to Revoked, no bond on disk.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pair_timeout_writes_no_bond_to_disk() {
    let td = temp_bond_dir();
    let mut cfg = MockConfig::golden();
    cfg.lesc_behavior = LescBehavior::HangForever;
    let backend = MockPairBackend::new(cfg);
    let opts = pair_opts(&td, None, TIMEOUT_SECS_TIGHT, true);

    let path = bonds_path(&bond_dir_path(&td));
    // Pre-pair snapshot: file does not exist.
    let pre_existed = path.exists();

    let start = std::time::Instant::now();
    let err = drive_pair(&opts, &backend, "")
        .await
        .expect_err("timeout must surface as PairError::Revoked");
    let elapsed = start.elapsed();

    match err {
        PairError::Revoked { reason } => assert_eq!(reason, RevokeReason::Timeout),
        other => panic!("unexpected error: {other:?}"),
    }
    // Timing sanity: the timeout must have actually fired, not returned
    // immediately. Some tolerance for CI noise.
    assert!(
        elapsed >= Duration::from_millis(800),
        "timeout must wait at least most of the deadline, got {elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(TIMEOUT_SECS_TIGHT) + TIMEOUT_GRACE);

    // Bond file unchanged: still does not exist.
    assert_eq!(path.exists(), pre_existed, "bond file presence unchanged after timeout");
}

// Variant: pre-existing bond file is byte-equal after a timeout.
#[tokio::test]
async fn pair_timeout_leaves_pre_existing_bonds_file_byte_equal() {
    let td = temp_bond_dir();
    // Pre-seed the bonds file with a non-trivial body so the byte-equality
    // assertion has real bytes to compare.
    let path = bonds_path(&bond_dir_path(&td));
    let parent = path.parent().expect("parent");
    fs::create_dir_all(parent).expect("mkdir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    let seed_body = b"schema_version = 1\n";
    fs::write(&path, seed_body).expect("seed");
    let pre = fs::read(&path).expect("pre");

    let mut cfg = MockConfig::golden();
    cfg.lesc_behavior = LescBehavior::HangForever;
    let backend = MockPairBackend::new(cfg);
    let opts = pair_opts(&td, None, TIMEOUT_SECS_TIGHT, true);

    let _ = drive_pair(&opts, &backend, "").await.expect_err("timeout expected");
    let post = fs::read(&path).expect("post");
    assert_eq!(post, pre, "pre-existing bond file must be byte-equal after timeout");
}

// ---------------------------------------------------------------------------
// TC-04: Operator rejects on Y/N — no bond.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pair_operator_reject_writes_no_bond() {
    let td = temp_bond_dir();
    let backend = MockPairBackend::new(MockConfig::golden());
    // `yes = false` ⇒ prompt is genuinely interactive; mock stdin supplies "n".
    let opts = pair_opts(&td, None, TIMEOUT_SECS_LOOSE, false);

    let err = drive_pair(&opts, &backend, "n\n").await.expect_err("operator N must abort");
    match err {
        PairError::Revoked { reason } => assert_eq!(reason, RevokeReason::OperatorReject),
        other => panic!("unexpected error: {other:?}"),
    }
    assert!(!bonds_path(&bond_dir_path(&td)).exists(), "no bond file written on N");
}

// ---------------------------------------------------------------------------
// TC-06: ambiguous --peer with --yes — AmbiguousPeer error.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pair_ambiguous_peer_with_yes_errors_with_match_list() {
    let td = temp_bond_dir();
    let mut cfg = MockConfig::golden();
    cfg.candidates = vec![
        PairCandidate {
            name: TEST_PEER_NAME.to_owned(),
            address: TEST_PEER_ADDR.to_owned(),
        },
        PairCandidate {
            name: TEST_PEER_NAME_SPARE.to_owned(),
            address: TEST_PEER_ADDR_SPARE.to_owned(),
        },
    ];
    let backend = MockPairBackend::new(cfg);
    let opts = pair_opts(&td, Some("alex-pixel"), TIMEOUT_SECS_LOOSE, true);

    let err = drive_pair(&opts, &backend, "").await.expect_err("ambiguous must error");
    match err {
        PairError::AmbiguousPeer { matches } => {
            assert_eq!(matches.len(), 2);
            assert!(matches.iter().any(|m| m == TEST_PEER_NAME));
            assert!(matches.iter().any(|m| m == TEST_PEER_NAME_SPARE));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert!(!bonds_path(&bond_dir_path(&td)).exists());
}

// ---------------------------------------------------------------------------
// TC-05 (extra angle): empty `syauth list` prints the documented hint.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_on_empty_store_prints_documented_hint() {
    let td = temp_bond_dir();
    let path = bonds_path(&bond_dir_path(&td));
    assert!(!path.exists(), "tempdir starts empty");

    // Library-level render so we exercise the same code path the binary
    // would call after `BondStore::load(path)` succeeds with zero bonds.
    let store = BondStore::load(&path).expect("empty load");
    let mut buf: Vec<u8> = Vec::new();
    let mut cur = Cursor::new(&mut buf);
    render_list_to(&mut cur, &store).expect("render");
    let s = String::from_utf8(buf).expect("utf8");
    assert!(s.contains("no bonds"), "hint must contain 'no bonds': {s}");
    // Path on disk is still untouched.
    assert!(!path.exists());

    // Smoke: ListOpts construct cleanly.
    let _ = list_opts(&td);
}

// ---------------------------------------------------------------------------
// Compile-time guard: the bond_dir / adapter / peer-filter / timeout / --yes
// flag plumbing is intact. Tests above exercise behavior; this is a sanity
// scan that prevents a future refactor from silently dropping a field.
// ---------------------------------------------------------------------------

#[test]
fn pair_opts_round_trip_via_struct_default_paths() {
    let td = temp_bond_dir();
    let opts = pair_opts(&td, Some(TEST_PEER_NAME), TIMEOUT_SECS_LOOSE, true);
    let path: &Path = &opts.bond_dir;
    assert!(path.starts_with(td.path()));
    assert_eq!(opts.adapter, TEST_ADAPTER);
    assert_eq!(opts.timeout_secs, TIMEOUT_SECS_LOOSE);
    assert!(opts.yes);
    assert_eq!(opts.peer.as_deref(), Some(TEST_PEER_NAME));
}
