//! `syauth-cli` — `list` subcommand.
//!
//! Thin reader of `BondStore::load(bond_dir).list()`, printing TSV:
//! `id\tname\tstatus\tcreated_at`.
//!
//! Per the S-011 DoD: "syauth list shows the new peer immediately after
//! pairing completes." S-012 will extend this with `--json`, `--revoked-only`,
//! etc.; the v1 surface lives here so the S-011 integration test can drive
//! both subcommands back-to-back in the same `--bond-dir` tempdir.
//!
//! Roadmap: specs/syauth/ROADMAP.md item S-011.
//! Journey: specs/journeys/JOURNEY-S-011-pairing-desktop.md

use std::io::{self, Write};

use syauth_core::{BondStatus, BondStore, bond::format_rfc3339};

use crate::pair::{LIST_EMPTY_HINT, LIST_FIELD_SEP, ListOpts, PairError, bonds_path};

/// Render a `BondStatus` as a short token suitable for the TSV `status`
/// column. `Bonded` ⇒ `bonded`, `Revoked` ⇒ `revoked:<reason>`.
fn render_status(status: &BondStatus) -> String {
    match status {
        BondStatus::Bonded => "bonded".to_owned(),
        BondStatus::Revoked { reason } => format!("revoked:{reason}"),
    }
}

/// Render the entire `list()` view to `writer` per the contract above.
pub fn render_list_to(writer: &mut dyn Write, store: &BondStore) -> Result<(), PairError> {
    let bonds = store.list();
    if bonds.is_empty() {
        writeln!(writer, "{LIST_EMPTY_HINT}")?;
        return Ok(());
    }
    for b in bonds {
        let when = format_rfc3339(&b.created_at).unwrap_or_else(|_| "?".to_owned());
        writeln!(
            writer,
            "{id}{sep}{name}{sep}{status}{sep}{created}",
            id = b.peer_id,
            name = b.name,
            status = render_status(&b.status),
            created = when,
            sep = LIST_FIELD_SEP,
        )?;
    }
    Ok(())
}

/// Drive `syauth list` against `opts.bond_dir`.
///
/// # Errors
///
/// Returns [`PairError`] when the bond file is malformed or unreadable.
/// A missing bond file is NOT an error: an empty store is loaded and the
/// `(no bonds; ...)` hint is printed.
pub fn run_list(opts: &ListOpts) -> Result<(), PairError> {
    let path = bonds_path(&opts.bond_dir);
    let store = BondStore::load(&path)?;
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    render_list_to(&mut writer, &store)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use syauth_core::{Bond, BondStatus, BondStore, peer_id_from_pubkey};
    use time::macros::datetime;

    use super::*;

    const FIXED_PUBKEY: [u8; 32] = [0x07; 32];

    #[test]
    fn render_list_to_emits_empty_hint() {
        let store = BondStore::empty();
        let mut buf: Vec<u8> = Vec::new();
        let mut cur = Cursor::new(&mut buf);
        render_list_to(&mut cur, &store).expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains(LIST_EMPTY_HINT));
    }

    #[test]
    fn render_list_to_emits_tsv_row_per_bond() {
        let mut store = BondStore::empty();
        let bond = Bond {
            peer_id: peer_id_from_pubkey(&FIXED_PUBKEY),
            pubkey: FIXED_PUBKEY,
            name: "alex-pixel".to_owned(),
            created_at: datetime!(2026-05-15 12:00:00 UTC),
            status: BondStatus::Bonded,
        };
        store.add(bond).expect("add");
        let mut buf: Vec<u8> = Vec::new();
        let mut cur = Cursor::new(&mut buf);
        render_list_to(&mut cur, &store).expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("alex-pixel"), "name in output: {s}");
        assert!(s.contains("bonded"), "status in output: {s}");
        assert_eq!(s.matches(LIST_FIELD_SEP).count(), 3, "three tab separators: {s}");
    }

    #[test]
    fn render_status_handles_revoked() {
        let s = render_status(&BondStatus::Revoked {
            reason: "phone-lost".to_owned(),
        });
        assert_eq!(s, "revoked:phone-lost");
    }
}
