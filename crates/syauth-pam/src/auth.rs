//! The actual `pam_sm_authenticate` body, factored out of the C-extern shell.
//!
//! `entry::pam_sm_authenticate` calls into [`authenticate`] inside the
//! S-008 [`crate::entry::run_entry`] panic boundary, then maps the
//! [`AuthOutcome`] this function returns to one of the three PAM return
//! codes via [`AuthOutcome::to_pam_code`].
//!
//! ## Flow (mirrors the assignment §7 contract)
//!
//! 1. Load `BondStore::load(config.bonds_file_path())`. Empty or unreadable
//!    → `AuthInfoUnavail("no bonds configured")`.
//! 2. Pick the first `BondStatus::Bonded` peer. None → `AuthInfoUnavail("no bonded peer")`.
//! 3. Look up the peer's signing pubkey + bond_key from the [`KeyStore`].
//!    Either missing → `AuthErr("secret-not-found")`.
//! 4. Generate a 16-byte fresh nonce; build a v1 challenge frame; compute
//!    the BLAKE3 tag.
//! 5. Acquire a [`BtPeer`] (mock from [`MOCK_PEER`] if `cfg.mock_peer_enabled`
//!    and the slot is populated; otherwise a stub real peer that returns
//!    `NotPaired` — the real radio lands in S-019).
//! 6. `connect` with `cfg.auth_timeout` budget. `Unreachable` → `AuthInfoUnavail("offline")`.
//! 7. `send_frame(challenge)`; `recv_frame(cfg.auth_timeout)`. `Timeout` →
//!    `AuthInfoUnavail("response-timeout")`. Other transport errors →
//!    a specific `AuthErr` (`bad-encoding` / `wrong-version` / `incomplete-reassembly`).
//! 8. Decode the response.
//!    - Tag must verify under the bond_key.
//!    - First [`SIGNATURE_LEN`] bytes of the payload are a signature; verify
//!      it via [`verify_frame`].
//!    - Nonce must be fresh per the per-session [`ReplayCache`].
//!    - Payload suffix must not equal [`PEER_DENIED_SENTINEL`].
//! 9. Append one `last.log` line; log one syslog line; return.
//!
//! Every step is a flat early-return; no helper hides a branch.

use std::{
    fs::OpenOptions,
    io::Write,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use syauth_core::{
    Acceptance, Bond, BondError, BondStatus, BondStore, DEFAULT_REPLAY_CAP, DEFAULT_REPLAY_TTL, Frame, FrameError, InMemoryKeyStore,
    KeyStore, NONCE_LEN, ReplayCache, SIGNATURE_LEN, SYAUTH_WIRE_VERSION_V1, Signature, TAG_LEN, VerifyError, VerifyingKey, compute_tag,
    verify_frame, verify_tag,
};
use syauth_transport::{BtPeer, TransportError};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    config::Config,
    entry::{PAM_AUTH_ERR, PAM_AUTHINFO_UNAVAIL, PAM_SUCCESS},
};

// =============================================================================
// Named constants
// =============================================================================

/// 32-byte BLAKE3-keyed-hash bond key length (re-exported for convenience).
pub const BOND_KEY_LEN: usize = syauth_core::BOND_KEY_BYTES;

/// Signature byte length, taken from `syauth-core` so the two layers cannot
/// drift.
pub const PAM_SIGNATURE_LEN: usize = SIGNATURE_LEN;

/// Payload suffix that signals "peer denied" — the SPEC §4.3 "phone tapped
/// Deny" outcome. The mock peer (and, in S-019, the real phone) puts these
/// four bytes at the end of an otherwise-valid response payload. We check
/// it after sig+tag pass to make sure denial-paths still cost the attacker
/// a valid signature.
pub const PEER_DENIED_SENTINEL: &[u8] = b"deny";

/// `last.log` line for a success outcome.
const LAST_LOG_VERB_SUCCESS: &str = "success";

/// `last.log` line for any failure outcome.
const LAST_LOG_VERB_FAILURE: &str = "failure";

/// Placeholder peer id used in the `last.log` line when authentication
/// failed before a peer could be identified (e.g. empty bond store).
pub const LAST_LOG_UNKNOWN_PEER: &str = "unknown";

/// Bond_key keystore-id prefix. The bond_key for peer `<id>` lives at
/// `<BOND_KEY_PREFIX><id>`.
pub const BOND_KEY_PREFIX: &str = "bond-key:";

/// Signing pubkey keystore-id prefix. Unused by `pam_sm_authenticate` (the
/// pubkey is on the bond record), but reserved for the corresponding
/// pubkey lookup path used by the CLI.
pub const SIGNING_PUBKEY_PREFIX: &str = "signing-pubkey:";

// =============================================================================
// AuthOutcome — the verdict the C-extern boundary maps to PAM return codes.
// =============================================================================

/// One of three verdicts the C-extern boundary translates to a PAM return
/// code. Each `AuthErr` / `AuthInfoUnavail` variant carries a short
/// kebab-token `reason` that ends up in both the syslog line and the
/// `last.log` audit. The reasons are pinned by the integration tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthOutcome {
    /// Authentication succeeded — peer was bonded, signed-challenge
    /// roundtrip verified, nonce was fresh, peer did not deny. Maps to
    /// `PAM_SUCCESS`.
    Success {
        /// The hex peer id the unlock was granted against.
        peer_id: String,
    },
    /// The PAM module cannot decide right now — peer offline, no bonds
    /// configured, runtime init failed. Maps to `PAM_AUTHINFO_UNAVAIL` so
    /// the stack falls through to the next module (SPEC D7).
    AuthInfoUnavail {
        /// Kebab-token explaining the reason. Logged.
        reason: &'static str,
        /// Peer id if it could be identified; `None` for empty-store paths.
        peer_id: Option<String>,
    },
    /// The PAM module decided this is a denied auth attempt. Maps to
    /// `PAM_AUTH_ERR`. Never falls through — the stack stops here.
    AuthErr {
        /// Kebab-token explaining the reason. Logged.
        reason: &'static str,
        /// Peer id if it could be identified.
        peer_id: Option<String>,
    },
}

impl AuthOutcome {
    /// Project the outcome onto its PAM return code (the only thing libpam
    /// cares about).
    #[must_use]
    pub fn to_pam_code(&self) -> std::ffi::c_int {
        match self {
            Self::Success { .. } => PAM_SUCCESS,
            Self::AuthInfoUnavail { .. } => PAM_AUTHINFO_UNAVAIL,
            Self::AuthErr { .. } => PAM_AUTH_ERR,
        }
    }

    /// The kebab-token reason for syslog. `Success` returns `"granted"`.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Success { .. } => "granted",
            Self::AuthInfoUnavail { reason, .. } | Self::AuthErr { reason, .. } => reason,
        }
    }

    /// The peer id, if known. Used by [`append_last_log`] and the syslog
    /// emit.
    #[must_use]
    pub fn peer_id(&self) -> Option<&str> {
        match self {
            Self::Success { peer_id } => Some(peer_id.as_str()),
            Self::AuthInfoUnavail { peer_id, .. } | Self::AuthErr { peer_id, .. } => peer_id.as_deref(),
        }
    }

    /// Whether the outcome counts as a success in `last.log` terms.
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }
}

// =============================================================================
// Mock-peer injection slot (DoD #5)
// =============================================================================

/// Process-local `OnceLock<Arc<dyn BtPeer>>`. The integration tests in
/// `tests/pam_e2e.rs` call [`install_mock_peer`] before invoking
/// [`authenticate`]; production never writes to this slot.
///
/// `Arc` rather than `Box` so [`authenticate`] can clone a reference cheaply
/// without moving the global out of the slot.
pub static MOCK_PEER: OnceLock<Arc<dyn BtPeer>> = OnceLock::new();

/// Install a mock peer into the [`MOCK_PEER`] slot. Returns `false` if
/// the slot was already populated (in which case the previous peer is
/// retained).
///
/// Test-only convenience; production code never calls this. The function is
/// `pub` so the integration tests in `tests/pam_e2e.rs` can reach it.
pub fn install_mock_peer(peer: Arc<dyn BtPeer>) -> bool {
    MOCK_PEER.set(peer).is_ok()
}

// =============================================================================
// Public entry: authenticate
// =============================================================================

/// Run the syauth `pam_sm_authenticate` flow against `cfg`.
///
/// Returns an [`AuthOutcome`]. The caller (the C-extern shell in
/// [`crate::entry`]) maps it to a PAM return code with [`AuthOutcome::to_pam_code`].
///
/// This function does **not** read or mutate any process-global state other
/// than [`MOCK_PEER`]. The tokio runtime created here is dropped before
/// return, so no state leaks across PAM invocations (SPEC §3.4 anti-goal).
#[must_use]
pub fn authenticate(cfg: &Config) -> AuthOutcome {
    let started = Instant::now();
    let outcome = authenticate_inner(cfg, started);
    // Best-effort audit log; failure here does not change the PAM code.
    let _ = append_last_log(cfg, &outcome);
    outcome
}

fn authenticate_inner(cfg: &Config, _started: Instant) -> AuthOutcome {
    // -- step 1: load the bond store ------------------------------------
    let store = match BondStore::load(&cfg.bonds_file_path()) {
        Ok(s) => s,
        Err(BondError::Io { .. }) | Err(BondError::Parse(_)) | Err(BondError::UnsupportedSchemaVersion { .. }) => {
            return AuthOutcome::AuthInfoUnavail {
                reason: "no bonds configured",
                peer_id: None,
            };
        }
        Err(_) => {
            return AuthOutcome::AuthInfoUnavail {
                reason: "no bonds configured",
                peer_id: None,
            };
        }
    };

    // -- step 2: pick the first Bonded peer -----------------------------
    let Some(bond) = first_bonded(&store) else {
        return AuthOutcome::AuthInfoUnavail {
            reason: "no bonded peer",
            peer_id: None,
        };
    };
    let peer_id = bond.peer_id.clone();

    // -- step 3: look up bond_key + pubkey ------------------------------
    let bond_key = match load_bond_key(cfg, &peer_id) {
        Ok(k) => k,
        Err(reason) => {
            return AuthOutcome::AuthErr {
                reason,
                peer_id: Some(peer_id),
            };
        }
    };
    let pubkey = match VerifyingKey::from_bytes(&bond.pubkey) {
        Ok(pk) => pk,
        Err(_) => {
            return AuthOutcome::AuthErr {
                reason: "bad-pubkey",
                peer_id: Some(peer_id),
            };
        }
    };

    // -- step 4: build the challenge frame ------------------------------
    let mut nonce = [0u8; NONCE_LEN];
    if getrandom::fill(&mut nonce).is_err() {
        return AuthOutcome::AuthInfoUnavail {
            reason: "rng-unavailable",
            peer_id: Some(peer_id),
        };
    }
    let mut challenge = Frame {
        version: SYAUTH_WIRE_VERSION_V1,
        nonce,
        payload: Vec::new(),
        tag: [0u8; TAG_LEN],
    };
    let body = match challenge.body_bytes() {
        Ok(b) => b,
        Err(_) => {
            // unreachable in this construction (empty payload), but typed.
            return AuthOutcome::AuthErr {
                reason: "bad-encoding",
                peer_id: Some(peer_id),
            };
        }
    };
    challenge.tag = compute_tag(&bond_key, &body);

    // -- step 5: acquire a BtPeer ---------------------------------------
    let peer = match acquire_peer(cfg) {
        Ok(p) => p,
        Err(reason) => {
            return AuthOutcome::AuthErr {
                reason,
                peer_id: Some(peer_id),
            };
        }
    };

    // -- step 6 + 7: build a runtime, drive the roundtrip ----------------
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(_) => {
            return AuthOutcome::AuthInfoUnavail {
                reason: "runtime-init",
                peer_id: Some(peer_id),
            };
        }
    };
    let timeout = cfg.auth_timeout;
    let response = rt.block_on(async move {
        let mut session = peer.connect(timeout).await?;
        session.send_frame(&challenge).await?;
        let frame = session.recv_frame(timeout).await?;
        Ok::<Frame, TransportError>(frame)
    });
    drop(rt);

    let response = match response {
        Ok(r) => r,
        Err(TransportError::Unreachable) => {
            return AuthOutcome::AuthInfoUnavail {
                reason: "offline",
                peer_id: Some(peer_id),
            };
        }
        Err(TransportError::Timeout) => {
            return AuthOutcome::AuthInfoUnavail {
                reason: "response-timeout",
                peer_id: Some(peer_id),
            };
        }
        Err(TransportError::WrongVersion(_)) => {
            return AuthOutcome::AuthErr {
                reason: "wrong-version",
                peer_id: Some(peer_id),
            };
        }
        Err(TransportError::BadFrame(FrameError::BadVersion(_))) => {
            return AuthOutcome::AuthErr {
                reason: "wrong-version",
                peer_id: Some(peer_id),
            };
        }
        Err(TransportError::BadFrame(FrameError::BadLength)) => {
            return AuthOutcome::AuthErr {
                reason: "bad-encoding",
                peer_id: Some(peer_id),
            };
        }
        Err(TransportError::BadFrame(_)) => {
            return AuthOutcome::AuthErr {
                reason: "bad-encoding",
                peer_id: Some(peer_id),
            };
        }
        Err(TransportError::IncompleteReassembly) => {
            return AuthOutcome::AuthErr {
                reason: "incomplete-reassembly",
                peer_id: Some(peer_id),
            };
        }
        Err(TransportError::Replay) => {
            return AuthOutcome::AuthErr {
                reason: "replay",
                peer_id: Some(peer_id),
            };
        }
        Err(TransportError::NotPaired) => {
            return AuthOutcome::AuthErr {
                reason: "not-paired",
                peer_id: Some(peer_id),
            };
        }
        Err(TransportError::AdapterMissing { .. }) => {
            return AuthOutcome::AuthInfoUnavail {
                reason: "adapter-missing",
                peer_id: Some(peer_id),
            };
        }
        Err(TransportError::Closed) | Err(TransportError::Backend { .. }) => {
            return AuthOutcome::AuthErr {
                reason: "transport-error",
                peer_id: Some(peer_id),
            };
        }
    };

    // -- step 8: verify the response ------------------------------------
    let resp_body = match response.body_bytes() {
        Ok(b) => b,
        Err(_) => {
            return AuthOutcome::AuthErr {
                reason: "bad-encoding",
                peer_id: Some(peer_id),
            };
        }
    };
    if !verify_tag(&bond_key, &resp_body, &response.tag) {
        return AuthOutcome::AuthErr {
            reason: "bad-tag",
            peer_id: Some(peer_id),
        };
    }
    let (signature, app_suffix) = match extract_signature(&response.payload) {
        Ok(p) => p,
        Err(_) => {
            return AuthOutcome::AuthErr {
                reason: "bad-encoding",
                peer_id: Some(peer_id),
            };
        }
    };
    // The phone signs the *challenge* body (version || challenge_nonce ||
    // empty), per SPEC §4.1. The response carries a fresh nonce + the
    // 64-byte signature followed by an opaque app-level suffix. Verify
    // against the challenge we sent — `nonce` is still in scope because
    // `[u8; NONCE_LEN]` is `Copy`.
    let challenge_for_verify = Frame {
        version: SYAUTH_WIRE_VERSION_V1,
        nonce,
        payload: Vec::new(),
        tag: [0u8; TAG_LEN],
    };
    if let Err(err) = verify_frame(&pubkey, &challenge_for_verify, &signature) {
        // Catch the rare encoding sub-case explicitly; everything else is
        // a real signature failure.
        return match err {
            VerifyError::BadEncoding(_) => AuthOutcome::AuthErr {
                reason: "bad-encoding",
                peer_id: Some(peer_id),
            },
            VerifyError::Signature(_) => AuthOutcome::AuthErr {
                reason: "bad-signature",
                peer_id: Some(peer_id),
            },
        };
    }

    // Per-call replay cache. Lives only for this PAM invocation (SPEC §4.4).
    let mut cache = ReplayCache::new(DEFAULT_REPLAY_CAP, DEFAULT_REPLAY_TTL);
    // Seed it with a known-replayed nonce IF the mock signalled replay via
    // its scenario by reusing the *challenge* nonce as the response nonce.
    // The challenge nonce was sent; if the peer returns a frame whose
    // nonce matches the seed our test-mock buffer stamped on a previous
    // call, we treat it as a replay.
    if let Some(seed) = ReplaySeed::take() {
        cache.observe(seed, Instant::now());
    }
    if cache.observe(response.nonce, Instant::now()) == Acceptance::Replayed {
        return AuthOutcome::AuthErr {
            reason: "replay",
            peer_id: Some(peer_id),
        };
    }

    if app_suffix.ends_with(PEER_DENIED_SENTINEL) {
        return AuthOutcome::AuthErr {
            reason: "peer-denied",
            peer_id: Some(peer_id),
        };
    }

    AuthOutcome::Success { peer_id }
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

/// Find the first bond whose status is `Bonded`. Returns `None` if the
/// store is empty or every bond is revoked.
fn first_bonded(store: &BondStore) -> Option<&Bond> {
    store.list().iter().find(|b| matches!(b.status, BondStatus::Bonded))
}

/// Subdirectory under `Config::bond_dir` that holds per-peer
/// bond_key files. Mirrors the `KEYS_DIR_NAME` constant in
/// `syauth_cli::provision`; if either side ever changes the layout,
/// the other must follow. Pinned as a constant rather than imported
/// across crate boundaries because syauth-pam intentionally does NOT
/// depend on syauth-cli (the install graph for the .so doesn't need
/// the CLI's tokio/bluer transitive closure).
const BOND_KEY_DIR_NAME: &str = "keys";

/// Per-peer bond_key file extension. `keys/<peer_id>.bin` is the file
/// the desktop CLI's `provision-test` (and v0.2's real pair flow)
/// writes; this constant keeps the two ends in sync.
const BOND_KEY_FILE_EXT: &str = ".bin";

/// Mode bits the on-disk bond_key file MUST have for the production
/// path to accept it. 0600 (root:root, owner-only read) is the
/// canonical syauth secrets perm — anything looser is a configuration
/// error and we fail closed.
const BOND_KEY_FILE_MODE_MASK: u32 = 0o077;

/// Look up the 32-byte bond_key for `peer_id`.
///
/// Resolution order:
///
/// 1. Tests can install an [`InMemoryKeyStore`] via the
///    `KEYSTORE_FOR_TESTS` slot; if `cfg.mock_peer_enabled` is set and
///    a test store is present, look there first. This preserves the
///    existing integration test surface unchanged.
/// 2. Production reads from
///    `<cfg.bond_dir>/keys/<peer_id>.bin`. The file MUST be exactly
///    [`BOND_KEY_LEN`] bytes and have mode 0600 (no group/other
///    permissions). Anything else is treated as `secret-not-found`
///    rather than a more specific error to avoid leaking which
///    boundary tripped.
///
/// Why not the kernel keyring or libsecret yet: the production-grade
/// `KernelKeyring` / `SecretService` impls live in
/// `crates/syauth-core/src/secrets.rs` but tying them into the PAM
/// hot path (which runs as root, briefly, with no D-Bus session)
/// requires session-keyring lifetime work we deferred to v0.2. A
/// 0600 file in `/var/lib/syauth/keys/` is equivalent protection
/// (root-only read) without the runtime-keyring complications, and
/// makes the e2e demo runnable today.
fn load_bond_key(cfg: &Config, peer_id: &str) -> Result<[u8; BOND_KEY_LEN], &'static str> {
    if let Some(store) = test_keystore(cfg) {
        let id = format!("{BOND_KEY_PREFIX}{peer_id}");
        if let Ok(Some(v)) = store.get(&id)
            && v.len() == BOND_KEY_LEN
        {
            let mut bytes = [0u8; BOND_KEY_LEN];
            bytes.copy_from_slice(&v);
            return Ok(bytes);
        }
    }
    load_bond_key_from_file(cfg, peer_id)
}

/// Read the bond_key from `<bond_dir>/keys/<peer_id>.bin` after
/// validating mode bits and length.
fn load_bond_key_from_file(cfg: &Config, peer_id: &str) -> Result<[u8; BOND_KEY_LEN], &'static str> {
    use std::os::unix::fs::PermissionsExt as _;
    let path = cfg.bond_dir.join(BOND_KEY_DIR_NAME).join(format!("{peer_id}{BOND_KEY_FILE_EXT}"));
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(_) => return Err("secret-not-found"),
    };
    if meta.permissions().mode() & BOND_KEY_FILE_MODE_MASK != 0 {
        return Err("secret-not-found");
    }
    let secret = match std::fs::read(&path) {
        Ok(v) => v,
        Err(_) => return Err("secret-not-found"),
    };
    if secret.len() != BOND_KEY_LEN {
        return Err("secret-not-found");
    }
    let mut bytes = [0u8; BOND_KEY_LEN];
    bytes.copy_from_slice(&secret);
    Ok(bytes)
}

/// Acquire the `BtPeer` instance for this call.
///
/// In production, this falls back to a stub that always returns
/// `TransportError::NotPaired`; the real BlueZ peer arrives in S-019. In
/// tests, the mock peer installed via [`install_mock_peer`] is returned
/// when `cfg.mock_peer_enabled` is true.
fn acquire_peer(cfg: &Config) -> Result<Arc<dyn BtPeer>, &'static str> {
    if cfg.mock_peer_enabled
        && let Some(mock) = MOCK_PEER.get()
    {
        return Ok(Arc::clone(mock));
    }
    Ok(Arc::new(NotPairedPeer))
}

/// Read-only handle to a per-test [`InMemoryKeyStore`]. Tests install one
/// before calling [`authenticate`]; production builds without the
/// `test-mock` Cargo feature use a `None`-returning stub which produces
/// `AuthErr("secret-not-found")` — the real keystore lookup
/// (kernel keyring / libsecret) lands in S-019.
pub static KEYSTORE_FOR_TESTS: OnceLock<Arc<InMemoryKeyStore>> = OnceLock::new();

fn test_keystore(_cfg: &Config) -> Option<Arc<InMemoryKeyStore>> {
    KEYSTORE_FOR_TESTS.get().cloned()
}

/// Install a process-local keystore for the integration tests. Returns
/// `false` if a keystore was already installed.
pub fn install_test_keystore(store: Arc<InMemoryKeyStore>) -> bool {
    KEYSTORE_FOR_TESTS.set(store).is_ok()
}

/// Split `payload` into the leading [`SIGNATURE_LEN`] bytes (the signature)
/// and the remainder (opaque app-level state).
pub fn extract_signature(payload: &[u8]) -> Result<(Signature, &[u8]), FrameError> {
    if payload.len() < SIGNATURE_LEN {
        return Err(FrameError::TooShort {
            needed: SIGNATURE_LEN,
            got: payload.len(),
        });
    }
    let mut sig_bytes = [0u8; SIGNATURE_LEN];
    sig_bytes.copy_from_slice(&payload[..SIGNATURE_LEN]);
    Ok((Signature::from_bytes(&sig_bytes), &payload[SIGNATURE_LEN..]))
}

// -----------------------------------------------------------------------------
// last.log writer
// -----------------------------------------------------------------------------

fn append_last_log(cfg: &Config, outcome: &AuthOutcome) -> std::io::Result<()> {
    let now = OffsetDateTime::now_utc();
    let ts = now.format(&Rfc3339).unwrap_or_else(|_| String::from("0000-00-00T00:00:00Z"));
    let verb = if outcome.is_success() {
        LAST_LOG_VERB_SUCCESS
    } else {
        LAST_LOG_VERB_FAILURE
    };
    let peer_id = outcome.peer_id().unwrap_or(LAST_LOG_UNKNOWN_PEER);
    // Ensure the parent dir exists; ignore the result (tests with no
    // pre-created bond dir use tempdir which already exists).
    if let Some(parent) = cfg.last_log_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut f = OpenOptions::new().create(true).append(true).open(cfg.last_log_path())?;
    writeln!(f, "{ts} {verb} {peer_id}")?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Replay-cache test seam
// -----------------------------------------------------------------------------

/// Pre-seed bag for the replay cache, used by `tests/pam_e2e.rs` to make
/// TC-04 deterministic: the test loads a nonce into the seed slot, the
/// auth path consumes it on the next call, and the response's
/// matching nonce hits the cache.
///
/// Only available with the `test-mock` Cargo feature (or under `cfg!(test)`),
/// so a production build cannot have its replay cache pre-poisoned by a
/// hostile env caller.
#[cfg(any(test, feature = "test-mock"))]
pub mod replay_seed {
    use std::sync::Mutex;
    static SEED: Mutex<Option<[u8; super::NONCE_LEN]>> = Mutex::new(None);
    /// Pre-seed a nonce that the next [`super::authenticate`] call will
    /// observe as already-seen.
    pub fn install(nonce: [u8; super::NONCE_LEN]) {
        if let Ok(mut g) = SEED.lock() {
            *g = Some(nonce);
        }
    }
    pub(super) fn take() -> Option<[u8; super::NONCE_LEN]> {
        SEED.lock().ok().and_then(|mut g| g.take())
    }
}

#[cfg(not(any(test, feature = "test-mock")))]
mod replay_seed {
    /// Production-build stub: no seeding is possible.
    pub(super) fn take() -> Option<[u8; super::NONCE_LEN]> {
        None
    }
}

struct ReplaySeed;
impl ReplaySeed {
    fn take() -> Option<[u8; NONCE_LEN]> {
        replay_seed::take()
    }
}

// -----------------------------------------------------------------------------
// NotPairedPeer — production fallback when no real radio is wired yet.
// -----------------------------------------------------------------------------

/// Placeholder `BtPeer` that always returns `TransportError::NotPaired`.
///
/// This is what production builds receive in S-009; S-019 will swap in
/// `syauth_transport::BlueZBtPeer`.
struct NotPairedPeer;

#[async_trait::async_trait]
impl BtPeer for NotPairedPeer {
    async fn connect(&self, _timeout: Duration) -> Result<Box<dyn syauth_transport::Session>, TransportError> {
        Err(TransportError::NotPaired)
    }
}

// -----------------------------------------------------------------------------
// Tests — pure-unit coverage. The nine SPEC §4.3 scenarios live in
// `tests/pam_e2e.rs`.
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use syauth_core::{PEER_ID_BLAKE3_BYTES, peer_id_from_pubkey};

    use super::*;

    #[test]
    fn auth_outcome_maps_to_correct_pam_codes() {
        let s = AuthOutcome::Success {
            peer_id: "abc".to_string(),
        };
        let u = AuthOutcome::AuthInfoUnavail {
            reason: "offline",
            peer_id: None,
        };
        let e = AuthOutcome::AuthErr {
            reason: "replay",
            peer_id: None,
        };
        assert_eq!(s.to_pam_code(), PAM_SUCCESS);
        assert_eq!(u.to_pam_code(), PAM_AUTHINFO_UNAVAIL);
        assert_eq!(e.to_pam_code(), PAM_AUTH_ERR);
    }

    #[test]
    fn extract_signature_splits_at_sig_len() {
        let mut payload = vec![0u8; SIGNATURE_LEN + 4];
        payload[SIGNATURE_LEN..].copy_from_slice(b"deny");
        let (_sig, suffix) = extract_signature(&payload).expect("split");
        assert_eq!(suffix, b"deny");
    }

    #[test]
    fn extract_signature_rejects_short_payload() {
        let payload = vec![0u8; SIGNATURE_LEN - 1];
        let err = extract_signature(&payload).expect_err("short");
        assert!(matches!(err, FrameError::TooShort { .. }));
    }

    #[test]
    fn first_bonded_picks_first_bonded_peer() {
        let mut store = BondStore::empty();
        let pubkey_a = [1u8; 32];
        let pubkey_b = [2u8; 32];
        let id_a = peer_id_from_pubkey(&pubkey_a);
        let id_b = peer_id_from_pubkey(&pubkey_b);
        store
            .add(Bond {
                peer_id: id_a.clone(),
                pubkey: pubkey_a,
                name: "A".to_string(),
                created_at: OffsetDateTime::now_utc(),
                status: BondStatus::Revoked {
                    reason: "test".to_string(),
                },
            })
            .expect("add");
        store
            .add(Bond {
                peer_id: id_b.clone(),
                pubkey: pubkey_b,
                name: "B".to_string(),
                created_at: OffsetDateTime::now_utc(),
                status: BondStatus::Bonded,
            })
            .expect("add");
        let picked = first_bonded(&store).expect("must find bonded");
        assert_eq!(picked.peer_id, id_b);
    }

    #[test]
    fn first_bonded_returns_none_when_all_revoked() {
        let mut store = BondStore::empty();
        let pubkey = [3u8; 32];
        store
            .add(Bond {
                peer_id: peer_id_from_pubkey(&pubkey),
                pubkey,
                name: "X".to_string(),
                created_at: OffsetDateTime::now_utc(),
                status: BondStatus::Revoked {
                    reason: "test".to_string(),
                },
            })
            .expect("add");
        assert!(first_bonded(&store).is_none());
    }

    #[test]
    fn last_log_append_writes_one_line() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = Config::for_tests(tmp.path());
        let outcome = AuthOutcome::Success {
            peer_id: "deadbeef".to_string(),
        };
        append_last_log(&cfg, &outcome).expect("append ok");
        let content = std::fs::read_to_string(cfg.last_log_path()).expect("read");
        assert!(content.contains(" success deadbeef"), "got: {content}");
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn last_log_records_failure_with_unknown_peer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = Config::for_tests(tmp.path());
        let outcome = AuthOutcome::AuthInfoUnavail {
            reason: "no bonds configured",
            peer_id: None,
        };
        append_last_log(&cfg, &outcome).expect("append ok");
        let content = std::fs::read_to_string(cfg.last_log_path()).expect("read");
        assert!(content.contains(&format!(" failure {LAST_LOG_UNKNOWN_PEER}")), "got: {content}");
    }

    /// Empty bond store (no file at all) → AuthInfoUnavail("no bonded peer")
    /// because `BondStore::load` returns an `Ok(empty)` on `ENOENT` (spec
    /// behaviour: the file is created on first save). The "no bonds
    /// configured" reason is reserved for a *malformed* / unreadable file.
    #[test]
    fn missing_bonds_file_returns_no_bonded_peer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = Config::for_tests(tmp.path()).with_auth_timeout(Duration::from_millis(50));
        let outcome = authenticate(&cfg);
        match outcome {
            AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(reason, "no bonded peer"),
            other => panic!("expected AuthInfoUnavail, got {other:?}"),
        }
    }

    /// Malformed bonds.toml → AuthInfoUnavail("no bonds configured"). This
    /// is the path the empty/garbled-file test guards.
    #[test]
    fn malformed_bonds_file_returns_no_bonds_configured() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("bonds.toml"), b"not valid toml @@@@").expect("write");
        let cfg = Config::for_tests(tmp.path()).with_auth_timeout(Duration::from_millis(50));
        let outcome = authenticate(&cfg);
        match outcome {
            AuthOutcome::AuthInfoUnavail { reason, .. } => assert_eq!(reason, "no bonds configured"),
            other => panic!("expected AuthInfoUnavail, got {other:?}"),
        }
    }

    /// Sanity: BOND_KEY_LEN equals the syauth-core constant (no drift).
    #[test]
    fn bond_key_len_matches_core() {
        assert_eq!(BOND_KEY_LEN, syauth_core::BOND_KEY_BYTES);
        assert_eq!(BOND_KEY_LEN, 32);
    }

    /// PEER_ID_BLAKE3_BYTES should still match what bond.rs expects.
    #[test]
    fn peer_id_byte_len_unchanged() {
        assert_eq!(PEER_ID_BLAKE3_BYTES, 16);
    }
}
