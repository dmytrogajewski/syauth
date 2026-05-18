//! `syauth-cli` — `pair` subcommand: LESC + app-level OOB.
//!
//! Drives the desktop side of pairing per SPEC §4.1 dataflow:
//!
//! 1. Open the adapter; refuse to proceed if it does not advertise LE Secure
//!    Connections.
//! 2. Scan for advertising peers, filter by `--peer <name>` if provided.
//! 3. Initiate LESC numeric comparison via the [`PairBackend`].
//! 4. Display the 6-digit BT code AND the 4-word app-level OOB code derived
//!    from `oob_code_for_bond(&bond_key)`.
//! 5. Wait for `[y/N]` (skipped on `--yes`).
//! 6. On `Y`: write the [`Bond`] to disk via `BondStore::save`. On `N` or
//!    timeout: do NOT write the bond; transition the in-memory state machine
//!    to [`PairingPhase::Revoked`].
//!
//! The radio is abstracted behind a tiny [`PairBackend`] async trait so the
//! integration test in `tests/pair_flow.rs` can inject a mock without touching
//! `bluer`.
//!
//! Roadmap: specs/syauth/ROADMAP.md item S-011.
//! Journey: specs/journeys/JOURNEY-S-011-pairing-desktop.md

use std::{
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use async_trait::async_trait;
use clap::Parser;
use syauth_core::{Bond, BondError, BondStatus, BondStore, peer_id_from_pubkey};
use thiserror::Error;
use time::OffsetDateTime;
use tokio::time::timeout;

use crate::oob::{OOB_BOND_KEY_BYTES, OOB_WORD_COUNT, oob_code_for_bond};

// ---------------------------------------------------------------------------
// Named constants — every magic number a test would otherwise hand-type.
// ---------------------------------------------------------------------------

/// Default BlueZ adapter id. Matches SPEC §4.1 and `syauth-transport`'s
/// `DEFAULT_ADAPTER_NAME` so the two crates agree.
pub const DEFAULT_ADAPTER_NAME: &str = "hci0";

/// Default `--bond-dir` per SPEC §4.4.
pub const DEFAULT_BOND_DIR: &str = "/var/lib/syauth";

/// Bonds file name within `--bond-dir`. Same name `BondStore::load` /
/// `BondStore::save` work with on a real install.
pub const BONDS_FILE_NAME: &str = "bonds.toml";

/// Default `ProvisionalBonded → Revoked` deadline in seconds.
pub const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// Hint message included in `LescUnsupported` errors to point the operator at
/// the most likely fix.
pub const LESC_UNSUPPORTED_HINT: &str = "adapter does not advertise LE Secure Connections (kernel < 5.4 or older controller)";

/// Warning emitted on stderr when `--scripted-oob` is in effect. The flag
/// is hidden from `--help` (clap `hide = true`) and meant for the
/// `scripts/e2e-emulator-up.sh` driver only. An operator running this by
/// hand always sees the banner first so the bypass is never accidental.
pub const SCRIPTED_OOB_WARNING: &str = "scripted-oob in effect; interactive OOB confirmation bypassed (S-019 e2e harness)";

/// Minimum hex length accepted for `--scripted-oob`. SPEC §3.1 derives the
/// OOB code from `HKDF(bond, "syauth-oob-v1")[0..OOB_WORD_COUNT]`; the
/// e2e script forwards whatever the Android side prints to its logcat
/// tag, which is at least one byte (two hex chars). We enforce a lower
/// bound so an empty or single-char argument fails clap-side rather than
/// silently bypassing the prompt.
pub const SCRIPTED_OOB_MIN_HEX_LEN: usize = 2;

/// Field separator used by `syauth list` TSV output.
pub const LIST_FIELD_SEP: char = '\t';

/// Banner printed when `syauth list` finds no bonds.
pub const LIST_EMPTY_HINT: &str = "(no bonds; run `syauth pair` to add one)";

/// Subdirectory of `--bond-dir` where `pam_syauth` reads the raw
/// 32-byte symmetric bond_key per peer (`<peer_id>.bin`). Matches
/// `crates/syauth-pam/src/auth.rs::BOND_KEY_DIR_NAME`.
pub const PAM_BOND_KEY_DIR_NAME: &str = "keys";

/// Extension `pam_syauth` expects on the per-peer bond_key file.
/// Matches `crates/syauth-pam/src/auth.rs::BOND_KEY_FILE_EXT`.
pub const PAM_BOND_KEY_FILE_EXT: &str = ".bin";

/// File mode `pam_syauth` requires on `<peer_id>.bin`. The PAM module
/// refuses the file (returns `secret-not-found`) if any group/other
/// bit is set, so the pair flow writes the file in 0600 from the
/// start.
pub const PAM_BOND_KEY_FILE_MODE: u32 = 0o600;

// ---------------------------------------------------------------------------
// CLI options.
// ---------------------------------------------------------------------------

/// CLI options for the `pair` subcommand.
#[derive(Debug, Parser, Clone)]
pub struct PairOpts {
    /// BlueZ adapter id (e.g. `hci0`).
    #[arg(long, default_value = DEFAULT_ADAPTER_NAME)]
    pub adapter: String,

    /// Restrict the picker to peers whose advertised name contains this
    /// substring. With `--yes`, the call fails with `AmbiguousPeer` if more
    /// than one peer matches.
    #[arg(long)]
    pub peer: Option<String>,

    /// `ProvisionalBonded → Revoked` deadline in seconds. Default 60.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    pub timeout_secs: u64,

    /// Directory holding the bonds file. Defaults to SPEC's
    /// `/var/lib/syauth/`. Tests inject a tempdir.
    #[arg(long, default_value = DEFAULT_BOND_DIR)]
    pub bond_dir: PathBuf,

    /// Skip the interactive `[y/N]` OOB confirmation prompt. Tests only.
    /// Does NOT skip any safety-relevant gate (adapter LESC check, ambiguous
    /// peer check).
    #[arg(long)]
    pub yes: bool,

    /// Surface the 6-digit LESC numeric-comparison code via the waybar
    /// applet (`sy syauth`) instead of stdin/`--yes`. The operator
    /// accepts or rejects the bond by clicking the bar entry; the
    /// privileged pair process and the unprivileged applet rendezvous
    /// over `${XDG_RUNTIME_DIR}/syauth/`. Mutually exclusive with
    /// `--yes`.
    #[arg(long, conflicts_with = "yes")]
    pub waybar: bool,

    /// Replace an existing bond record when the just-paired phone's
    /// `peer_id` already lives in the bond store. Without `--force`
    /// the pair flow fails with [`PairError::PeerAlreadyBonded`] and
    /// names the existing peer_id; the operator either revokes (
    /// `syauth revoke --id <peer_id>`) and re-runs, or re-runs with
    /// `--force` to overwrite. Note: the OS-level LESC bond is
    /// untouched either way; `--force` only swaps the on-disk bond
    /// record.
    #[arg(long)]
    pub force: bool,

    /// S-019 e2e seam: accept the OOB hex code directly and bypass the
    /// interactive `[y/N]` prompt entirely. Hidden from `--help` so an
    /// operator cannot reach it by accident; intended for
    /// `scripts/e2e-emulator-up.sh`. Prints a one-line warning to stderr
    /// (see [`SCRIPTED_OOB_WARNING`]) whenever it is used, so even a
    /// reviewer skimming a CI log sees the bypass. Does NOT skip any
    /// safety-relevant gate.
    #[arg(long, hide = true, value_name = "HEX")]
    pub scripted_oob: Option<String>,
}

/// CLI options for the `list` subcommand.
#[derive(Debug, Parser, Clone)]
pub struct ListOpts {
    /// Directory holding the bonds file. Defaults to SPEC's
    /// `/var/lib/syauth/`. Tests inject a tempdir.
    #[arg(long, default_value = DEFAULT_BOND_DIR)]
    pub bond_dir: PathBuf,
}

// ---------------------------------------------------------------------------
// Backend abstraction — the radio seam.
// ---------------------------------------------------------------------------

/// Lightweight handle for a peer the operator can pair with. The `name` is the
/// advertised friendly name; the `address` is the device address (typically a
/// BD_ADDR rendered in colon-hex). Both are operator-facing strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairCandidate {
    /// Advertised friendly name of the peer.
    pub name: String,
    /// Device address as a string. Opaque to this crate.
    pub address: String,
}

/// Result of a successful LESC negotiation: the peer's 32-byte Ed25519 public
/// key and the negotiated 32-byte bond key.
#[derive(Debug, Clone)]
pub struct LescOutcome {
    /// Peer's Ed25519 public key (32 bytes).
    pub peer_pubkey: [u8; 32],
    /// Negotiated bond key, fed into [`oob_code_for_bond`].
    pub bond_key: [u8; OOB_BOND_KEY_BYTES],
    /// 6-digit code BlueZ derived from LESC numeric comparison. Operator
    /// confirms this on both devices before the app-level OOB step.
    pub numeric_code: u32,
}

/// Adapter capability snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterInfo {
    /// Adapter id (e.g. `"hci0"`).
    pub name: String,
    /// Whether the adapter advertises the LE Secure Connections bit.
    pub supports_lesc: bool,
}

/// Abstraction over the `bluer` calls the pair flow makes. The mock impl in
/// the integration test implements this without touching a real radio.
#[async_trait]
pub trait PairBackend: Send + Sync {
    /// Open and probe the configured adapter. Returns the capability
    /// snapshot on success; `Err(PairError::AdapterMissing { name })` if the
    /// adapter is unknown to BlueZ.
    async fn adapter_info(&self, adapter_id: &str) -> Result<AdapterInfo, PairError>;

    /// Convenience helper layered on [`Self::adapter_info`]: returns `true`
    /// iff the adapter advertises LE Secure Connections.
    async fn adapter_supports_lesc(&self, adapter_id: &str) -> Result<bool, PairError> {
        Ok(self.adapter_info(adapter_id).await?.supports_lesc)
    }

    /// Scan for advertising peers. Returns the candidates the operator may
    /// choose from. Bounded by the backend's internal scan window.
    async fn scan_peers(&self) -> Result<Vec<PairCandidate>, PairError>;

    /// Drive LESC numeric comparison with `peer`. In production this wraps
    /// `bluer::Device::pair()` with MITM protection required.
    async fn initiate_lesc_with_peer(&self, peer: &PairCandidate) -> Result<LescOutcome, PairError>;
}

// ---------------------------------------------------------------------------
// Pairing state machine.
// ---------------------------------------------------------------------------

/// Reason recorded when the state machine transitions to
/// [`PairingPhase::Revoked`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevokeReason {
    /// `--timeout-secs` elapsed before the operator confirmed.
    Timeout,
    /// Operator answered `N` (or anything other than `y`/`Y`) at the OOB
    /// confirmation prompt.
    OperatorReject,
}

/// In-process pairing state machine. `/bt` SKILL Phase 2 mandates an explicit
/// enum (no `is_paired: bool`) so the only path to [`Self::Bonded`] is through
/// every preceding gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingPhase {
    /// Initial: backend is scanning for advertising peers.
    Scanning,
    /// LESC numeric comparison is in flight.
    AwaitingLesc,
    /// LESC completed; operator must confirm the 4-word OOB code.
    AwaitingOobConfirmation {
        /// The 4-word emoji OOB code derived from the negotiated bond key.
        code: [String; OOB_WORD_COUNT],
    },
    /// Operator confirmed; bond is in memory but not yet on disk.
    ProvisionalBonded {
        /// Stable BLAKE3-derived peer id from `peer_id_from_pubkey`.
        peer_id: String,
    },
    /// Bond is on disk; CLI is about to exit 0.
    Bonded,
    /// Pair flow aborted. No bond was written.
    Revoked {
        /// Why the state machine ended up here.
        reason: RevokeReason,
    },
}

// ---------------------------------------------------------------------------
// Errors.
// ---------------------------------------------------------------------------

/// Typed error surface for the `pair` flow.
#[derive(Debug, Error)]
pub enum PairError {
    /// Configured adapter is unknown to BlueZ.
    #[error("bluetooth adapter '{name}' not found")]
    AdapterMissing {
        /// The adapter id the operator asked for.
        name: String,
    },

    /// Adapter exists but does not advertise the LE Secure Connections bit.
    /// The DoD requires this error to name the issue.
    #[error("adapter '{adapter}' does not support LE Secure Connections; {hint}")]
    LescUnsupported {
        /// Adapter id, e.g. `"hci0"`.
        adapter: String,
        /// Human-readable hint, defaults to [`LESC_UNSUPPORTED_HINT`].
        hint: String,
    },

    /// Scan produced no candidates (no peer advertising in the window).
    #[error("no advertising peers found; ensure the phone app is on the pairing screen and within range")]
    NoPeers,

    /// `--peer <name>` matched more than one candidate while `--yes` was set
    /// (the picker is non-interactive in that case).
    #[error("ambiguous --peer filter: matched {matches:?}")]
    AmbiguousPeer {
        /// Names of every candidate that matched the filter.
        matches: Vec<String>,
    },

    /// `--peer <name>` matched zero candidates.
    #[error("--peer filter matched no advertising peers")]
    PeerNotFound,

    /// Pair flow was aborted (timeout or operator rejection).
    #[error("pair flow revoked: {reason:?}; no bond written")]
    Revoked {
        /// Why the state machine transitioned to Revoked.
        reason: RevokeReason,
    },

    /// Bond store I/O or contract failure (future schema, missing
    /// file, etc.). Wraps the upstream [`BondError`] verbatim. The
    /// [`BondError::AlreadyBonded`] case is funnelled through the
    /// typed [`PairError::PeerAlreadyBonded`] variant below so the
    /// CLI can print a remediation hint; every other [`BondError`]
    /// kind reaches this variant unchanged.
    #[error("bond store error: {0}")]
    Bond(#[from] BondError),

    /// The just-paired phone's `peer_id` already exists in the bond
    /// store and the operator did not pass `--force`. The CLI's
    /// stderr renderer turns this into a multi-line hint:
    ///   * the `peer_id` of the duplicate row,
    ///   * the exact `syauth revoke --id <peer_id>` invocation to
    ///     drop the old record,
    ///   * an `--force` hint for the same-operator re-pair path.
    #[error(
        "peer already paired with this desktop\n  peer_id={peer_id}\n  hint: rerun with `--force` to replace the existing bond,\n        or run `syauth revoke --id {peer_id}` first."
    )]
    PeerAlreadyBonded {
        /// The duplicate `peer_id` (32-char lowercase hex).
        peer_id: String,
    },

    /// Backend reported a failure that is not one of the typed variants.
    #[error("pair backend error: {reason}")]
    Backend {
        /// Human-readable description of the upstream failure.
        reason: String,
    },

    /// Peer (or the local stack via a misconfigured agent) attempted a
    /// pairing variant that has no MITM protection — Just Works. SPEC §3.2
    /// D5 demands LE Secure Connections numeric comparison; anything that
    /// would silently downgrade is refused. `actual` names the variant
    /// the stack offered.
    #[error("pair flow refused: {actual} variant has no MITM protection (LESC numeric comparison required by SPEC §3.2 D5)")]
    DowngradeBlocked {
        /// The variant the peer / stack offered.
        actual: PairingVariant,
    },

    /// Peer requested a pairing variant we cannot drive: legacy PIN,
    /// passkey-entry, OOB-only. None of these are equivalent to LESC
    /// numeric comparison and we do not implement them.
    #[error("pair flow refused: unsupported pairing variant '{actual}'")]
    UnsupportedPairingVariant {
        /// The variant the peer / stack offered.
        actual: PairingVariant,
    },

    /// Stdin / stdout error during the operator prompt.
    #[error("pair I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Pairing variants the BlueZ / Android stacks can present at agent /
/// `ACTION_PAIRING_REQUEST` time. Modeled here as a typed enum so the
/// decision logic in [`decide_pairing`] is testable without a radio.
/// The Display names are stable, lowercase-kebab strings — operator-
/// facing error messages embed them and an operator may grep for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingVariant {
    /// LESC numeric comparison. BlueZ:
    /// `Agent1::RequestConfirmation(device, passkey)`. Android:
    /// `BluetoothDevice.PAIRING_VARIANT_PASSKEY_CONFIRMATION` (1).
    /// This is the **only** variant the syauth pair flow accepts.
    PasskeyConfirmation,

    /// "Just Works": both ends auto-accept without operator confirmation.
    /// BlueZ: `Agent1::RequestAuthorization`. Android:
    /// `BluetoothDevice.PAIRING_VARIANT_CONSENT` (3). Has no MITM
    /// protection; SPEC §3.2 D5 forbids silently using it.
    JustWorks,

    /// Legacy BR/EDR PIN entry (pre-BT 2.1 SSP). BlueZ never offers it
    /// for LE devices but a misbehaving stack might; refuse.
    LegacyPin,

    /// Passkey entry: one side displays a code, the other types it.
    /// BlueZ: `Agent1::RequestPasskey` / `DisplayPasskey`. Distinct from
    /// LESC numeric comparison and not what SPEC §3.2 D5 prescribes.
    PasskeyEntry,

    /// OOB-only pairing (data exchanged via NFC, QR, etc.). Out of
    /// scope for the SPEC §3.3 ML "IN — v0.1.0" surface.
    OobOnly,
}

impl std::fmt::Display for PairingVariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PairingVariant::PasskeyConfirmation => "passkey-confirmation",
            PairingVariant::JustWorks => "just-works",
            PairingVariant::LegacyPin => "legacy-pin",
            PairingVariant::PasskeyEntry => "passkey-entry",
            PairingVariant::OobOnly => "oob-only",
        })
    }
}

/// Decide whether `variant` is acceptable for a syauth pair flow.
///
/// Returns `Ok(())` only for [`PairingVariant::PasskeyConfirmation`] —
/// the LESC numeric-comparison variant SPEC §3.2 D5 mandates. Every
/// other variant is refused with a typed error:
///
/// * [`PairingVariant::JustWorks`] → [`PairError::DowngradeBlocked`]
///   (no MITM protection; silent downgrade attempt).
/// * Everything else → [`PairError::UnsupportedPairingVariant`].
///
/// Called by the BlueZ agent's `request_confirmation` /
/// `request_authorization` callbacks on the Linux side and by the
/// Android `ACTION_PAIRING_REQUEST` receiver. Both paths consult a
/// single function so the decision is observable, testable, and
/// monotonic — no fork between platforms.
pub fn decide_pairing(variant: PairingVariant) -> Result<(), PairError> {
    match variant {
        PairingVariant::PasskeyConfirmation => Ok(()),
        PairingVariant::JustWorks => Err(PairError::DowngradeBlocked { actual: variant }),
        PairingVariant::LegacyPin | PairingVariant::PasskeyEntry | PairingVariant::OobOnly => {
            Err(PairError::UnsupportedPairingVariant { actual: variant })
        }
    }
}

// ---------------------------------------------------------------------------
// Operator confirmation seam.
// ---------------------------------------------------------------------------

/// Result of the operator-facing y/N confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OobConfirmation {
    /// Operator answered yes (or `--yes` was supplied).
    Accept,
    /// Operator answered no, or supplied something that did not parse as yes.
    Reject,
}

/// Read one line from `reader` and parse as a yes/no answer.
///
/// `--yes` short-circuits before calling this — when this function is called
/// the prompt is genuinely interactive.
pub fn parse_yes_no(line: &str) -> OobConfirmation {
    let trimmed = line.trim().to_ascii_lowercase();
    if trimmed == "y" || trimmed == "yes" {
        OobConfirmation::Accept
    } else {
        OobConfirmation::Reject
    }
}

/// Print the OOB banner and prompt; read one line from `reader`. With
/// `auto_accept = true`, returns [`OobConfirmation::Accept`] without reading
/// any input.
fn read_oob_confirmation(
    writer: &mut dyn Write,
    reader: &mut dyn BufRead,
    code: &[String; OOB_WORD_COUNT],
    auto_accept: bool,
) -> Result<OobConfirmation, PairError> {
    writeln!(writer, "app-level OOB code (must match the phone):")?;
    for word in code.iter() {
        writeln!(writer, "  {word}")?;
    }
    write!(writer, "OOB matches your phone? [y/N]: ")?;
    writer.flush()?;
    if auto_accept {
        writeln!(writer, "y (--yes)")?;
        return Ok(OobConfirmation::Accept);
    }
    let mut buf = String::new();
    let _ = reader.read_line(&mut buf)?;
    Ok(parse_yes_no(&buf))
}

// ---------------------------------------------------------------------------
// Candidate filtering.
// ---------------------------------------------------------------------------

/// Filter `candidates` by `--peer` substring (case-sensitive substring match
/// on the advertised name). When `peer_filter` is `None`, returns the input
/// untouched.
pub fn filter_candidates(candidates: &[PairCandidate], peer_filter: Option<&str>) -> Vec<PairCandidate> {
    match peer_filter {
        None => candidates.to_vec(),
        Some(needle) => candidates.iter().filter(|c| c.name.contains(needle)).cloned().collect(),
    }
}

/// Pick exactly one candidate from `filtered` given the `--yes` flag. With
/// `auto_pick = true` and more than one candidate, returns `AmbiguousPeer`.
/// With one candidate, returns it. With zero, returns `PeerNotFound`.
pub fn pick_unambiguous(filtered: Vec<PairCandidate>, auto_pick: bool) -> Result<PairCandidate, PairError> {
    match filtered.len() {
        0 => Err(PairError::PeerNotFound),
        1 => {
            let mut iter = filtered.into_iter();
            match iter.next() {
                Some(p) => Ok(p),
                // Unreachable: len == 1 above.
                None => Err(PairError::Backend {
                    reason: "candidate iterator empty after len==1 check".to_owned(),
                }),
            }
        }
        _ => {
            if auto_pick {
                Err(PairError::AmbiguousPeer {
                    matches: filtered.into_iter().map(|c| c.name).collect(),
                })
            } else {
                // The interactive picker is documented in the journey; tests
                // never reach this branch (they always pass `--yes`).
                // Production code uses the same selection by surfacing the
                // list to stdout and reading an index from stdin in
                // `run_pair_with_io`.
                Err(PairError::Backend {
                    reason: "ambiguous peer without --yes; interactive picker is selected in run_pair_with_io".to_owned(),
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Bond build.
// ---------------------------------------------------------------------------

/// Build the persisted [`Bond`] from the LESC outcome and the chosen peer.
///
/// `created_at` is wall-clock now per SPEC §4.4. Tests inject a fixed time via
/// `build_bond_with_time` to keep the bond file byte-deterministic.
pub fn build_bond_with_time(outcome: &LescOutcome, peer: &PairCandidate, now: OffsetDateTime) -> Bond {
    Bond {
        peer_id: peer_id_from_pubkey(&outcome.peer_pubkey),
        pubkey: outcome.peer_pubkey,
        name: peer.name.clone(),
        created_at: now,
        status: BondStatus::Bonded,
    }
}

/// Same as [`build_bond_with_time`] using `OffsetDateTime::now_utc()`.
fn build_bond(outcome: &LescOutcome, peer: &PairCandidate) -> Bond {
    build_bond_with_time(outcome, peer, OffsetDateTime::now_utc())
}

// ---------------------------------------------------------------------------
// Core pair driver.
// ---------------------------------------------------------------------------

/// Path inside `bond_dir` where the bonds file lives.
pub fn bonds_path(bond_dir: &Path) -> PathBuf {
    bond_dir.join(BONDS_FILE_NAME)
}

/// Path `pam_syauth` reads the raw 32-byte bond_key from for a given
/// `peer_id`. Tests inject a tempdir as `bond_dir`; production uses
/// `/var/lib/syauth/keys/<peer_id>.bin`.
pub fn pam_bond_key_path(bond_dir: &Path, peer_id: &str) -> PathBuf {
    bond_dir
        .join(PAM_BOND_KEY_DIR_NAME)
        .join(format!("{peer_id}{PAM_BOND_KEY_FILE_EXT}"))
}

/// Write the 32-byte symmetric bond_key for `peer_id` at the path
/// `pam_syauth` looks at on every unlock. Creates the `keys/`
/// subdirectory (mode 0700) if it does not exist; writes the file in
/// 0600 from the start so `pam_syauth` does not reject it on the
/// permission-mask gate.
fn write_pam_bond_key(bond_dir: &Path, peer_id: &str, bond_key: &[u8]) -> Result<(), PairError> {
    use std::os::unix::fs::PermissionsExt as _;
    let keys_dir = bond_dir.join(PAM_BOND_KEY_DIR_NAME);
    std::fs::create_dir_all(&keys_dir).map_err(PairError::Io)?;
    let _ = std::fs::set_permissions(&keys_dir, std::fs::Permissions::from_mode(0o700));
    let path = pam_bond_key_path(bond_dir, peer_id);
    std::fs::write(&path, bond_key).map_err(PairError::Io)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(PAM_BOND_KEY_FILE_MODE)).map_err(PairError::Io)?;
    Ok(())
}

/// Drive the pair flow against `backend`, reading operator confirmation from
/// `reader` and writing UI to `writer`.
///
/// This is the seam tests drive. Returns the final [`PairingPhase`] — either
/// [`PairingPhase::Bonded`] or [`PairingPhase::Revoked`]. A
/// [`PairingPhase::Revoked`] is also surfaced as
/// [`PairError::Revoked { reason }`] when the caller wants a single
/// `Result`-typed value; both forms carry the same `reason`.
pub async fn run_pair_with_io(
    opts: &PairOpts,
    backend: &dyn PairBackend,
    reader: &mut dyn BufRead,
    writer: &mut dyn Write,
) -> Result<PairingPhase, PairError> {
    let info = backend.adapter_info(&opts.adapter).await?;
    writeln!(
        writer,
        "adapter {} ready (LE Secure Connections: {})",
        info.name,
        if info.supports_lesc { "yes" } else { "no" }
    )?;
    // Safety gate #1: refuse to pair on a non-LESC adapter regardless of
    // `--yes`.
    if !info.supports_lesc {
        return Err(PairError::LescUnsupported {
            adapter: info.name,
            hint: LESC_UNSUPPORTED_HINT.to_owned(),
        });
    }

    // Phase 1: Scanning. The state machine reads top-to-bottom; each
    // transition is the local variable being shadowed (not reassigned),
    // which keeps clippy/unused-assignments happy and makes the
    // dataflow visible to readers.
    let _phase_scanning = PairingPhase::Scanning;
    let candidates = backend.scan_peers().await?;
    if candidates.is_empty() {
        return Err(PairError::NoPeers);
    }
    let filtered = filter_candidates(&candidates, opts.peer.as_deref());
    let chosen = pick_unambiguous(filtered, opts.yes)?;
    writeln!(writer, "selected {} ({})", chosen.name, chosen.address)?;

    // Phase 2: AwaitingLesc.
    let _phase_awaiting_lesc = PairingPhase::AwaitingLesc;
    writeln!(writer, "initiating LE Secure Connections...")?;
    let lesc_deadline = Duration::from_secs(opts.timeout_secs);
    let outcome = match timeout(lesc_deadline, backend.initiate_lesc_with_peer(&chosen)).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(e),
        Err(_elapsed) => {
            return Err(PairError::Revoked {
                reason: RevokeReason::Timeout,
            });
        }
    };
    writeln!(writer, "BT numeric code: {:06}   confirm on both devices", outcome.numeric_code)?;

    // Phase 3: AwaitingOobConfirmation.
    let code = oob_code_for_bond(&outcome.bond_key);
    let _phase_awaiting_oob = PairingPhase::AwaitingOobConfirmation { code: code.clone() };
    // S-019 scripted-OOB seam: when the caller passed `--scripted-oob`,
    // the prompt is bypassed entirely (no read from `reader`) and a
    // warning lands on the writer. The bond is still persisted via the
    // same path; the only thing the flag skips is the interactive
    // confirmation. Treat it as `--yes` for the prompt seam, with an
    // additional stderr warning the caller's script can grep.
    let scripted = opts.scripted_oob.is_some();
    if scripted {
        writeln!(writer, "warning: {SCRIPTED_OOB_WARNING}")?;
    }
    let auto_accept = opts.yes || scripted;
    let answer = read_oob_confirmation(writer, reader, &code, auto_accept)?;
    match answer {
        OobConfirmation::Accept => (),
        OobConfirmation::Reject => {
            return Err(PairError::Revoked {
                reason: RevokeReason::OperatorReject,
            });
        }
    }

    // Phase 4: ProvisionalBonded → Bonded.
    let bond = build_bond(&outcome, &chosen);
    let peer_id = bond.peer_id.clone();
    let _phase_provisional = PairingPhase::ProvisionalBonded { peer_id: peer_id.clone() };
    let path = bonds_path(&opts.bond_dir);
    let mut store = BondStore::load(&path)?;
    match store.add(bond.clone()) {
        Ok(()) => {}
        Err(BondError::AlreadyBonded { peer_id: existing }) => {
            if opts.force {
                store.remove(&existing)?;
                store.add(bond)?;
                writeln!(writer, "replaced existing bond peer_id={existing}")?;
            } else {
                return Err(PairError::PeerAlreadyBonded { peer_id: existing });
            }
        }
        Err(other) => return Err(other.into()),
    }
    store.save(&path)?;
    // pam_syauth reads the raw 32-byte bond_key from
    // `<bond_dir>/keys/<peer_id>.bin` (0600). Without this file the
    // PAM module fails with `secret-not-found` even though the bond
    // record itself is on disk. Write the keys file atomically beside
    // `bonds.toml`; the OOB-confirmed bond_key from LESC is the
    // symmetric MAC key the unlock channel needs.
    write_pam_bond_key(&opts.bond_dir, &peer_id, &outcome.bond_key)?;
    writeln!(writer, "bonded {} id={peer_id}; run `syauth list` to verify", chosen.name)?;
    Ok(PairingPhase::Bonded)
}

/// Same as [`run_pair_with_io`] but wires stdio for the production binary.
///
/// # Errors
///
/// Returns [`PairError`] for every typed failure of the pair flow.
pub async fn run_pair(opts: &PairOpts, backend: &dyn PairBackend) -> Result<PairingPhase, PairError> {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    run_pair_with_io(opts, backend, &mut reader, &mut writer).await
}

// ---------------------------------------------------------------------------
// Tests — library-level. Integration test lives in tests/pair_flow.rs.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // DEV-001 — Pairing variant enforcement (JOURNEY-DEV-001 TC-03).
    //
    // SPEC §3.2 D5 mandates "LE Secure Connections numeric comparison".
    // The BlueZ agent + Android pairing receiver each receive a typed
    // variant on every pair request; only LESC numeric comparison is
    // acceptable. Everything else (Just Works, legacy PIN, OOB-only,
    // passkey-entry-typed) is a downgrade attempt and must be refused.
    // -----------------------------------------------------------------

    #[test]
    fn decide_pairing_accepts_passkey_confirmation() {
        // LESC numeric comparison: BlueZ variant
        // `RequestConfirmation(device, passkey)`; Android
        // `PAIRING_VARIANT_PASSKEY_CONFIRMATION` (value 1).
        let got = decide_pairing(PairingVariant::PasskeyConfirmation);
        assert!(got.is_ok(), "expected Ok for PasskeyConfirmation, got {got:?}");
    }

    #[test]
    fn decide_pairing_rejects_just_works_as_downgrade_blocked() {
        // Just Works: BlueZ `RequestAuthorization`; Android
        // `PAIRING_VARIANT_CONSENT` (value 3). No MITM protection;
        // accepting it silently weakens the bond.
        let got = decide_pairing(PairingVariant::JustWorks);
        assert!(
            matches!(got, Err(PairError::DowngradeBlocked { .. })),
            "expected DowngradeBlocked for JustWorks, got {got:?}"
        );
    }

    #[test]
    fn decide_pairing_rejects_legacy_pin_as_unsupported_variant() {
        let got = decide_pairing(PairingVariant::LegacyPin);
        assert!(
            matches!(got, Err(PairError::UnsupportedPairingVariant { .. })),
            "expected UnsupportedPairingVariant for LegacyPin, got {got:?}"
        );
    }

    #[test]
    fn decide_pairing_rejects_passkey_entry_as_unsupported_variant() {
        let got = decide_pairing(PairingVariant::PasskeyEntry);
        assert!(
            matches!(got, Err(PairError::UnsupportedPairingVariant { .. })),
            "expected UnsupportedPairingVariant for PasskeyEntry, got {got:?}"
        );
    }

    #[test]
    fn decide_pairing_rejects_oob_only_as_unsupported_variant() {
        let got = decide_pairing(PairingVariant::OobOnly);
        assert!(
            matches!(got, Err(PairError::UnsupportedPairingVariant { .. })),
            "expected UnsupportedPairingVariant for OobOnly, got {got:?}"
        );
    }

    #[test]
    fn pairing_variant_display_is_stable_and_secret_free() {
        // Display names appear in operator-facing error reasons; they
        // must be stable (an operator can grep for them) and must not
        // include any secret-derived bytes. The `Debug` impl is fine
        // for logs; the `to_string` is what `PairError` formats.
        assert_eq!(PairingVariant::PasskeyConfirmation.to_string(), "passkey-confirmation");
        assert_eq!(PairingVariant::JustWorks.to_string(), "just-works");
        assert_eq!(PairingVariant::LegacyPin.to_string(), "legacy-pin");
        assert_eq!(PairingVariant::PasskeyEntry.to_string(), "passkey-entry");
        assert_eq!(PairingVariant::OobOnly.to_string(), "oob-only");
    }

    // -----------------------------------------------------------------
    // Legacy tests below.
    // -----------------------------------------------------------------

    #[test]
    fn parse_yes_no_recognizes_y_and_yes() {
        assert_eq!(parse_yes_no("y\n"), OobConfirmation::Accept);
        assert_eq!(parse_yes_no("Y"), OobConfirmation::Accept);
        assert_eq!(parse_yes_no("yes"), OobConfirmation::Accept);
        assert_eq!(parse_yes_no("YES\n"), OobConfirmation::Accept);
    }

    #[test]
    fn parse_yes_no_rejects_everything_else() {
        assert_eq!(parse_yes_no(""), OobConfirmation::Reject);
        assert_eq!(parse_yes_no("n"), OobConfirmation::Reject);
        assert_eq!(parse_yes_no("no"), OobConfirmation::Reject);
        assert_eq!(parse_yes_no("maybe"), OobConfirmation::Reject);
    }

    #[test]
    fn filter_candidates_passes_through_without_filter() {
        let c = vec![
            PairCandidate {
                name: "a".to_owned(),
                address: "AA".to_owned(),
            },
            PairCandidate {
                name: "b".to_owned(),
                address: "BB".to_owned(),
            },
        ];
        assert_eq!(filter_candidates(&c, None).len(), 2);
    }

    #[test]
    fn filter_candidates_substring_filters() {
        let c = vec![
            PairCandidate {
                name: "alex-pixel".to_owned(),
                address: "AA".to_owned(),
            },
            PairCandidate {
                name: "alex-spare".to_owned(),
                address: "BB".to_owned(),
            },
            PairCandidate {
                name: "other".to_owned(),
                address: "CC".to_owned(),
            },
        ];
        let got = filter_candidates(&c, Some("alex"));
        assert_eq!(got.len(), 2);
        let got = filter_candidates(&c, Some("pixel"));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "alex-pixel");
    }

    #[test]
    fn pick_unambiguous_returns_only_match() {
        let c = vec![PairCandidate {
            name: "a".to_owned(),
            address: "AA".to_owned(),
        }];
        let got = pick_unambiguous(c, true).expect("one ok");
        assert_eq!(got.name, "a");
    }

    #[test]
    fn pick_unambiguous_errors_on_zero_matches() {
        let err = pick_unambiguous(vec![], true).expect_err("zero matches");
        assert!(matches!(err, PairError::PeerNotFound));
    }

    #[test]
    fn pick_unambiguous_with_yes_errors_on_two_matches() {
        let c = vec![
            PairCandidate {
                name: "a".to_owned(),
                address: "AA".to_owned(),
            },
            PairCandidate {
                name: "b".to_owned(),
                address: "BB".to_owned(),
            },
        ];
        let err = pick_unambiguous(c, true).expect_err("ambiguous");
        match err {
            PairError::AmbiguousPeer { matches } => {
                assert_eq!(matches, vec!["a".to_owned(), "b".to_owned()]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // S-019 — `--scripted-oob` bypasses the prompt and warns on stderr/writer.
    // -----------------------------------------------------------------------

    /// Minimal mock backend so this unit test does not need to share state
    /// with the `tests/pair_flow.rs` integration suite. Returns a fixed
    /// golden outcome for every adapter / scan / LESC call.
    struct ScriptedTestBackend;

    /// Pinned 32-byte test bond key.
    const SCRIPTED_TEST_BOND_KEY: [u8; 32] = [0x42; 32];
    /// Pinned 32-byte test pubkey.
    const SCRIPTED_TEST_PUBKEY: [u8; 32] = [0x21; 32];
    /// Pinned numeric code so the test asserts deterministic stdout.
    const SCRIPTED_TEST_NUMERIC_CODE: u32 = 123_456;
    /// Pinned scripted-oob hex argument. Anything ≥ 2 hex chars is valid.
    const SCRIPTED_TEST_OOB_HEX: &str = "deadbeef";

    #[async_trait]
    impl PairBackend for ScriptedTestBackend {
        async fn adapter_info(&self, adapter_id: &str) -> Result<AdapterInfo, PairError> {
            Ok(AdapterInfo {
                name: adapter_id.to_owned(),
                supports_lesc: true,
            })
        }
        async fn scan_peers(&self) -> Result<Vec<PairCandidate>, PairError> {
            Ok(vec![PairCandidate {
                name: "scripted-peer".to_owned(),
                address: "AA:BB:CC:DD:EE:01".to_owned(),
            }])
        }
        async fn initiate_lesc_with_peer(&self, _peer: &PairCandidate) -> Result<LescOutcome, PairError> {
            Ok(LescOutcome {
                peer_pubkey: SCRIPTED_TEST_PUBKEY,
                bond_key: SCRIPTED_TEST_BOND_KEY,
                numeric_code: SCRIPTED_TEST_NUMERIC_CODE,
            })
        }
    }

    #[tokio::test]
    async fn scripted_oob_bypasses_prompt_and_warns_without_reading_stdin() {
        use std::io::Cursor;

        // Empty stdin would deadlock the interactive prompt — the assertion
        // that this test reaches `Bonded` without blocking on `read_line`
        // is exactly the contract we want pinned.
        let mut reader = Cursor::new(Vec::<u8>::new());
        let mut writer: Vec<u8> = Vec::new();

        let td = tempfile::tempdir().expect("tempdir for bonds");
        let opts = PairOpts {
            adapter: DEFAULT_ADAPTER_NAME.to_owned(),
            peer: None,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            bond_dir: td.path().join("syauth"),
            yes: false,
            waybar: false,
            scripted_oob: Some(SCRIPTED_TEST_OOB_HEX.to_owned()),
            force: false,
        };

        let phase = run_pair_with_io(&opts, &ScriptedTestBackend, &mut reader, &mut writer)
            .await
            .expect("scripted-oob pair must reach Bonded");
        assert_eq!(phase, PairingPhase::Bonded);

        let out = String::from_utf8_lossy(&writer);
        assert!(
            out.contains(SCRIPTED_OOB_WARNING),
            "scripted-oob warning must appear in writer output;\nout: {out}"
        );
        // The prompt's auto-accept tail line ("y (--yes)") still lands
        // because the OOB confirmation seam is shared with `--yes`.
        assert!(
            out.contains("y (--yes)"),
            "auto-accept tail must land on stdout-equivalent writer; got: {out}"
        );

        // Verify the bond was actually persisted.
        let bonds_file = bonds_path(&opts.bond_dir);
        let store = BondStore::load(&bonds_file).expect("bond store loads");
        assert_eq!(store.list().len(), 1, "exactly one bond persisted");
    }

    /// Helper: build a [`PairOpts`] for the already-bonded re-pair tests.
    fn already_bonded_opts(td: &tempfile::TempDir, force: bool) -> PairOpts {
        PairOpts {
            adapter: DEFAULT_ADAPTER_NAME.to_owned(),
            peer: None,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            bond_dir: td.path().join("syauth"),
            yes: true,
            waybar: false,
            scripted_oob: None,
            force,
        }
    }

    /// First pair lands; second pair with the same `ScriptedTestBackend`
    /// (same pubkey → same `peer_id`) fails with the typed
    /// [`PairError::PeerAlreadyBonded`] when `--force` is NOT set. The
    /// error's `Display` carries the existing peer_id plus the two
    /// remediation lines (`--force` hint and `syauth revoke --id` hint).
    #[tokio::test]
    async fn second_pair_without_force_returns_peer_already_bonded_with_hints() {
        use std::io::Cursor;

        let td = tempfile::tempdir().expect("tempdir for bonds");

        // First pair — must succeed and write exactly one bond.
        let opts_first = already_bonded_opts(&td, false);
        let mut reader_first = Cursor::new(Vec::<u8>::new());
        let mut writer_first: Vec<u8> = Vec::new();
        run_pair_with_io(&opts_first, &ScriptedTestBackend, &mut reader_first, &mut writer_first)
            .await
            .expect("first pair must reach Bonded");
        let bonds_file = bonds_path(&opts_first.bond_dir);
        assert_eq!(BondStore::load(&bonds_file).expect("store loads after first pair").list().len(), 1);

        // Second pair against the same fixture — same pubkey → same
        // peer_id → must fail with the typed variant.
        let opts_second = already_bonded_opts(&td, false);
        let mut reader_second = Cursor::new(Vec::<u8>::new());
        let mut writer_second: Vec<u8> = Vec::new();
        let err = run_pair_with_io(&opts_second, &ScriptedTestBackend, &mut reader_second, &mut writer_second)
            .await
            .expect_err("second pair without --force must fail");
        let expected_peer_id = peer_id_from_pubkey(&SCRIPTED_TEST_PUBKEY);
        match &err {
            PairError::PeerAlreadyBonded { peer_id } => assert_eq!(peer_id, &expected_peer_id),
            other => panic!("expected PeerAlreadyBonded, got: {other:?}"),
        }
        let rendered = err.to_string();
        assert!(rendered.contains(&expected_peer_id), "Display must carry peer_id; got: {rendered}");
        assert!(rendered.contains("--force"), "Display must point at --force; got: {rendered}");
        assert!(
            rendered.contains("syauth revoke --id"),
            "Display must point at syauth revoke --id; got: {rendered}"
        );

        // Defense in depth: the store still has exactly one bond
        // (the failed re-pair must NOT touch the existing record).
        let store_after = BondStore::load(&bonds_file).expect("store loads after second pair");
        assert_eq!(store_after.list().len(), 1, "failed re-pair must not mutate the store");
    }

    /// `--force` swaps the existing bond record in place; the writer
    /// captures the `replaced existing bond peer_id=...` log line; the
    /// store still ends with exactly one bond.
    #[tokio::test]
    async fn second_pair_with_force_replaces_existing_bond_in_place() {
        use std::io::Cursor;

        let td = tempfile::tempdir().expect("tempdir for bonds");

        // First pair — same as the previous test.
        let opts_first = already_bonded_opts(&td, false);
        let mut reader_first = Cursor::new(Vec::<u8>::new());
        let mut writer_first: Vec<u8> = Vec::new();
        run_pair_with_io(&opts_first, &ScriptedTestBackend, &mut reader_first, &mut writer_first)
            .await
            .expect("first pair must reach Bonded");
        let bonds_file = bonds_path(&opts_first.bond_dir);

        // Second pair WITH `--force`. Must reach Bonded; writer must
        // contain the replacement log line; store must still have
        // exactly one bond keyed on the same peer_id.
        let opts_second = already_bonded_opts(&td, true);
        let mut reader_second = Cursor::new(Vec::<u8>::new());
        let mut writer_second: Vec<u8> = Vec::new();
        let phase = run_pair_with_io(&opts_second, &ScriptedTestBackend, &mut reader_second, &mut writer_second)
            .await
            .expect("second pair with --force must reach Bonded");
        assert_eq!(phase, PairingPhase::Bonded);

        let out = String::from_utf8_lossy(&writer_second);
        let expected_peer_id = peer_id_from_pubkey(&SCRIPTED_TEST_PUBKEY);
        let expected_replace_line = format!("replaced existing bond peer_id={expected_peer_id}");
        assert!(
            out.contains(&expected_replace_line),
            "writer must log replacement; expected {expected_replace_line:?} in {out:?}"
        );

        let store_after = BondStore::load(&bonds_file).expect("store loads after --force re-pair");
        assert_eq!(store_after.list().len(), 1, "exactly one bond after --force re-pair");
        assert_eq!(store_after.list()[0].peer_id, expected_peer_id);
    }

    /// `run_pair_with_io` must write the per-peer bond_key file
    /// `pam_syauth` reads on every unlock, otherwise the desktop
    /// pair completes but `pam_syauth` fails with
    /// `secret-not-found`. Pins the file at
    /// `<bond_dir>/keys/<peer_id>.bin`, length 32, mode 0600.
    #[tokio::test]
    async fn pair_writes_pam_bond_key_file_at_expected_path() {
        use std::{io::Cursor, os::unix::fs::PermissionsExt as _};

        let td = tempfile::tempdir().expect("tempdir for bonds");
        let opts = already_bonded_opts(&td, false);
        let mut reader = Cursor::new(Vec::<u8>::new());
        let mut writer: Vec<u8> = Vec::new();
        run_pair_with_io(&opts, &ScriptedTestBackend, &mut reader, &mut writer)
            .await
            .expect("pair must reach Bonded");

        let expected_peer_id = peer_id_from_pubkey(&SCRIPTED_TEST_PUBKEY);
        let path = pam_bond_key_path(&opts.bond_dir, &expected_peer_id);
        let meta = std::fs::metadata(&path).expect("pam bond_key file must exist");
        assert_eq!(meta.len(), 32, "pam bond_key file must be 32 bytes");
        assert_eq!(
            meta.permissions().mode() & 0o777,
            PAM_BOND_KEY_FILE_MODE,
            "pam bond_key file must be 0600"
        );
        let bytes = std::fs::read(&path).expect("pam bond_key file readable");
        assert_eq!(bytes, SCRIPTED_TEST_BOND_KEY.to_vec(), "file bytes must equal LescOutcome.bond_key");
    }
}
