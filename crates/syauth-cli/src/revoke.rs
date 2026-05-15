//! `syauth-cli` — `revoke` subcommand.
//!
//! Day-2 verb that flips a bond's [`syauth_core::BondStatus`] to
//! [`syauth_core::BondStatus::Revoked`] with an operator-supplied reason.
//! The bond record is **never** deleted — the PAM module needs the
//! pubkey + reason to refuse subsequent unlock attempts and to surface
//! the reason in `syauth list`.
//!
//! Idempotency contract: re-running `syauth revoke --id <same>` is a
//! no-op (exit 0). [`syauth_core::BondStore::mark_revoked`] already
//! returns `Ok(())` for an already-revoked bond and never overwrites the
//! original reason; this module surfaces the existing reason on stdout
//! so the operator sees what they previously recorded.
//!
//! Roadmap: specs/syauth/ROADMAP.md item S-012.
//! Journey: specs/journeys/JOURNEY-S-012-day2-cli.md

use std::{
    io::{self, Write},
    path::PathBuf,
};

use clap::Parser;
use syauth_core::{BondStatus, BondStore};
use thiserror::Error;

use crate::pair::{DEFAULT_BOND_DIR, bonds_path};

/// Default reason recorded when `--reason` is not supplied. Non-empty so
/// the audit trail in `bonds.toml` always names *something* — the
/// operator can grep the file later and find every revoke they performed.
pub const DEFAULT_REVOKE_REASON: &str = "manual: syauth revoke";

/// CLI options for the `revoke` subcommand.
///
/// `--id` is the required long-form (no positional) per the journey's
/// "long-form everywhere" decision. `--reason` defaults to
/// [`DEFAULT_REVOKE_REASON`] so the operator-facing audit trail is
/// always populated.
#[derive(Debug, Parser, Clone)]
pub struct RevokeOpts {
    /// Bond peer id (32-char lowercase hex from
    /// [`syauth_core::peer_id_from_pubkey`]). Required.
    #[arg(long)]
    pub id: String,

    /// Directory holding the bonds file. Defaults to SPEC's
    /// `/var/lib/syauth/`. Tests inject a tempdir.
    #[arg(long, default_value = DEFAULT_BOND_DIR)]
    pub bond_dir: PathBuf,

    /// Operator-supplied reason for the revocation; written to the
    /// bond's status field. Defaults to [`DEFAULT_REVOKE_REASON`].
    #[arg(long, default_value = DEFAULT_REVOKE_REASON)]
    pub reason: String,
}

/// Typed errors produced by [`run_revoke`].
#[derive(Debug, Error)]
pub enum RevokeError {
    /// `--id` was not present in the bond store. Operator likely
    /// mistyped the id; the message embeds the looked-up id verbatim so
    /// they can compare against `syauth list` output.
    #[error("no bond with id={id} (run `syauth list` to see known ids)")]
    UnknownId {
        /// The id that was looked up.
        id: String,
    },

    /// Bond store I/O or contract failure. Wraps [`syauth_core::BondError`].
    #[error("bond store error: {0}")]
    Bond(#[from] syauth_core::BondError),

    /// Stdio I/O error.
    #[error("revoke i/o error: {0}")]
    Io(#[from] io::Error),
}

/// Outcome of a successful `revoke` invocation, surfaced to the
/// dispatcher so it can pick the right stdout message without re-reading
/// the bond.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevokeOutcome {
    /// Bond was previously bonded; this call flipped it to `Revoked`
    /// with the supplied `reason`.
    Revoked {
        /// The bond's peer id.
        id: String,
        /// The reason recorded on disk.
        reason: String,
    },
    /// Bond was already revoked; the second call was a no-op and the
    /// existing reason was preserved verbatim.
    AlreadyRevoked {
        /// The bond's peer id.
        id: String,
        /// The reason as it appears in the store (unchanged).
        existing_reason: String,
    },
}

/// Render the [`RevokeOutcome`] to `writer`.
///
/// Two cases, matching the journey's "operator sees what they recorded":
///
/// - `Revoked` → `revoked bond <id>: <reason>`
/// - `AlreadyRevoked` → `bond <id> already revoked: <reason>`
///
/// Both go to stdout; both exit 0.
pub fn render_outcome_to(writer: &mut dyn Write, outcome: &RevokeOutcome) -> Result<(), RevokeError> {
    match outcome {
        RevokeOutcome::Revoked { id, reason } => {
            writeln!(writer, "revoked bond {id}: {reason}")?;
        }
        RevokeOutcome::AlreadyRevoked { id, existing_reason } => {
            writeln!(writer, "bond {id} already revoked: {existing_reason}")?;
        }
    }
    Ok(())
}

/// Core, side-effect-bearing implementation of `syauth revoke` against
/// `store` plus a `writer` for the operator-facing line.
///
/// This is the seam tests drive. The thin wrapper [`run_revoke`] wires
/// up stdio and the on-disk [`BondStore`].
///
/// # Errors
///
/// Returns [`RevokeError::UnknownId`] when `id` is not present.
///
/// # Side effects
///
/// On a successful revoke the caller is responsible for `store.save(...)`.
/// On an already-revoked id the function returns
/// [`RevokeOutcome::AlreadyRevoked`] without mutating `store`, so the
/// caller can skip the save (no bytes change) and still exit 0.
pub fn apply_revoke(store: &mut BondStore, id: &str, reason: &str) -> Result<RevokeOutcome, RevokeError> {
    let snapshot = lookup_status(store, id)?;
    match snapshot {
        BondStatus::Revoked { reason: existing_reason } => Ok(RevokeOutcome::AlreadyRevoked {
            id: id.to_owned(),
            existing_reason,
        }),
        BondStatus::Bonded => {
            store.mark_revoked(id, reason)?;
            Ok(RevokeOutcome::Revoked {
                id: id.to_owned(),
                reason: reason.to_owned(),
            })
        }
    }
}

/// Find the bond with `id` and return its current [`BondStatus`] (by
/// value). Returns [`RevokeError::UnknownId`] if not present.
///
/// Pulled out so [`apply_revoke`] can branch on the pre-state without
/// holding a borrow into `store` while calling `mark_revoked`.
fn lookup_status(store: &BondStore, id: &str) -> Result<BondStatus, RevokeError> {
    for b in store.list() {
        if b.peer_id == id {
            return Ok(b.status.clone());
        }
    }
    Err(RevokeError::UnknownId { id: id.to_owned() })
}

/// Drive `syauth revoke` end-to-end: load the bond store, mark the
/// bond revoked (or detect already-revoked), save atomically, print the
/// operator-facing line.
///
/// # Errors
///
/// Returns [`RevokeError`] for every typed failure (unknown id, I/O,
/// bond store contract violation).
pub fn run_revoke(opts: &RevokeOpts) -> Result<(), RevokeError> {
    let path = bonds_path(&opts.bond_dir);
    let mut store = BondStore::load(&path)?;
    let outcome = apply_revoke(&mut store, &opts.id, &opts.reason)?;
    if matches!(outcome, RevokeOutcome::Revoked { .. }) {
        store.save(&path)?;
    }
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    render_outcome_to(&mut writer, &outcome)
}

// ---------------------------------------------------------------------------
// Unit tests — library-level. Integration test lives in tests/cli.rs.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use syauth_core::{Bond, BondStatus, BondStore, peer_id_from_pubkey};
    use time::macros::datetime;

    use super::*;

    const FIXED_PUBKEY: [u8; 32] = [0x07; 32];
    const ALT_PUBKEY: [u8; 32] = [0x08; 32];
    const REASON: &str = "phone-lost";

    fn seed_bonded(name: &str, pubkey: [u8; 32]) -> Bond {
        Bond {
            peer_id: peer_id_from_pubkey(&pubkey),
            pubkey,
            name: name.to_owned(),
            created_at: datetime!(2026-05-15 12:00:00 UTC),
            status: BondStatus::Bonded,
        }
    }

    #[test]
    fn apply_revoke_marks_bonded_as_revoked() {
        let mut store = BondStore::empty();
        let bond = seed_bonded("alex", FIXED_PUBKEY);
        let id = bond.peer_id.clone();
        store.add(bond).expect("add");
        let outcome = apply_revoke(&mut store, &id, REASON).expect("revoke");
        match outcome {
            RevokeOutcome::Revoked { id: got, reason } => {
                assert_eq!(got, id);
                assert_eq!(reason, REASON);
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
        match &store.list()[0].status {
            BondStatus::Revoked { reason } => assert_eq!(reason, REASON),
            other => panic!("expected Revoked, got {other:?}"),
        }
    }

    #[test]
    fn apply_revoke_on_already_revoked_returns_existing_reason() {
        let mut store = BondStore::empty();
        let bond = seed_bonded("alex", FIXED_PUBKEY);
        let id = bond.peer_id.clone();
        store.add(bond).expect("add");
        let _ = apply_revoke(&mut store, &id, "first").expect("first revoke");
        let outcome = apply_revoke(&mut store, &id, "second").expect("idempotent");
        match outcome {
            RevokeOutcome::AlreadyRevoked { id: got, existing_reason } => {
                assert_eq!(got, id);
                assert_eq!(existing_reason, "first", "second reason must not overwrite the first");
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[test]
    fn apply_revoke_unknown_id_returns_unknown_id_error() {
        let mut store = BondStore::empty();
        store.add(seed_bonded("alex", FIXED_PUBKEY)).expect("add");
        let err = apply_revoke(&mut store, "deadbeef", REASON).expect_err("unknown");
        match err {
            RevokeError::UnknownId { id } => assert_eq!(id, "deadbeef"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn apply_revoke_does_not_disturb_other_bonds() {
        let mut store = BondStore::empty();
        let target = seed_bonded("alex", FIXED_PUBKEY);
        let other = seed_bonded("kim", ALT_PUBKEY);
        let id = target.peer_id.clone();
        let other_id = other.peer_id.clone();
        store.add(target).expect("add target");
        store.add(other).expect("add other");
        apply_revoke(&mut store, &id, REASON).expect("revoke");
        let by_id: std::collections::HashMap<_, _> = store.list().iter().map(|b| (b.peer_id.clone(), b.status.clone())).collect();
        assert!(matches!(by_id.get(&id), Some(BondStatus::Revoked { .. })));
        assert!(matches!(by_id.get(&other_id), Some(BondStatus::Bonded)));
    }

    #[test]
    fn render_outcome_to_revoked_includes_id_and_reason() {
        let mut buf: Vec<u8> = Vec::new();
        let mut cur = Cursor::new(&mut buf);
        render_outcome_to(
            &mut cur,
            &RevokeOutcome::Revoked {
                id: "abc".to_owned(),
                reason: REASON.to_owned(),
            },
        )
        .expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("abc"));
        assert!(s.contains(REASON));
        assert!(s.contains("revoked"));
    }

    #[test]
    fn render_outcome_to_already_revoked_names_existing_reason() {
        let mut buf: Vec<u8> = Vec::new();
        let mut cur = Cursor::new(&mut buf);
        render_outcome_to(
            &mut cur,
            &RevokeOutcome::AlreadyRevoked {
                id: "abc".to_owned(),
                existing_reason: "prev".to_owned(),
            },
        )
        .expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("already revoked"));
        assert!(s.contains("prev"));
    }
}
