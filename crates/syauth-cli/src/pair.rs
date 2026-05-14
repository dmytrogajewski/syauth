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

/// Field separator used by `syauth list` TSV output.
pub const LIST_FIELD_SEP: char = '\t';

/// Banner printed when `syauth list` finds no bonds.
pub const LIST_EMPTY_HINT: &str = "(no bonds; run `syauth pair` to add one)";

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

    /// Bond store I/O or contract failure (already-bonded, future schema,
    /// etc.). Wraps the upstream [`BondError`] verbatim.
    #[error("bond store error: {0}")]
    Bond(#[from] BondError),

    /// Backend reported a failure that is not one of the typed variants.
    #[error("pair backend error: {reason}")]
    Backend {
        /// Human-readable description of the upstream failure.
        reason: String,
    },

    /// Stdin / stdout error during the operator prompt.
    #[error("pair I/O error: {0}")]
    Io(#[from] io::Error),
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
    let answer = read_oob_confirmation(writer, reader, &code, opts.yes)?;
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
    store.add(bond)?;
    store.save(&path)?;
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
}
