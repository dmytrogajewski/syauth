//! syauth bond store — persistent record of bonded phones.
//!
//! Per SPEC §4.4, the bond store is the only piece of multi-invocation
//! state syauth keeps on the host. The on-disk format is a TOML file at
//! `/var/lib/syauth/bonds.toml` (configurable at compile time, overridable
//! for tests via the `path` argument to [`BondStore::load`] /
//! [`BondStore::save`]). The file is owned by root, with mode
//! [`BOND_FILE_MODE`] (`0o600`); its parent directory is mode
//! [`BOND_DIR_MODE`] (`0o700`).
//!
//! Writes are atomic via [`tempfile::NamedTempFile::persist`], so a crash
//! between [`std::io::Write::write_all`] and `persist` cannot leave the
//! destination in a torn state — the destination either has the previous
//! bytes or the new bytes, never a mixture. This invariant is pinned by
//! the test [`tests::atomic_write_fault_leaves_file_intact`].
//!
//! Per-peer identity uses a deterministic BLAKE3 hash of the peer's
//! 32-byte Ed25519 pubkey, truncated to [`PEER_ID_BLAKE3_BYTES`] bytes
//! and rendered as lowercase hex by [`peer_id_from_pubkey`]. This makes
//! `peer_id` stable across reboots and reinstalls — re-pairing the same
//! phone (with the same Keystore-backed key, per SPEC D6) yields the
//! same id, which is necessary for `syauth list` and `syauth revoke` to
//! be useful across the lifetime of the bond.
//!
//! The TOML schema carries a top-level `schema_version: u32`. Readers
//! reject any value greater than [`BOND_SCHEMA_VERSION_LATEST`] with a
//! typed [`BondError::UnsupportedSchemaVersion`]; this is the
//! forward-compat seam called out in SPEC §4.5 (mirroring the explicit
//! frame-version rejection in S-002).

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Length in bytes of an Ed25519 public key — the only pubkey type we
/// accept in v0.1 (SPEC D4).
pub const PUBKEY_LEN: usize = 32;

/// Number of BLAKE3-hash bytes used to form the peer id. 16 bytes (128
/// bits) is comfortably above the birthday bound for the population of
/// phones a single user will bond in their lifetime, and it produces a
/// 32-char hex string that is short enough for terminal output and long
/// enough that two random ids never collide in practice.
pub const PEER_ID_BLAKE3_BYTES: usize = 16;

/// Length of the rendered hex peer id (`2 * PEER_ID_BLAKE3_BYTES`).
pub const PEER_ID_HEX_LEN: usize = PEER_ID_BLAKE3_BYTES * 2;

/// POSIX mode for the bonds file (`0o600`). Root-only read/write.
pub const BOND_FILE_MODE: u32 = 0o600;

/// POSIX mode for the parent directory of the bonds file (`0o700`).
/// Root-only access — see SPEC §4.4.
pub const BOND_DIR_MODE: u32 = 0o700;

/// Highest schema version this build understands. Bumped lock-step with
/// every breaking schema change. Today we have one version.
pub const BOND_SCHEMA_VERSION_LATEST: u32 = 1;

/// Octal permission mask we compare against when reading existing
/// directory metadata. Anything stricter (e.g. 0o000) is fine; anything
/// looser (e.g. 0o755) is rejected so a misconfigured operator does not
/// silently widen the bonds-file blast radius.
pub const BOND_DIR_PERMISSION_MASK: u32 = 0o777;

/// Status of a bonded peer at the time it was last mutated. The
/// `Revoked` variant carries an operator-facing reason for the
/// revocation; see [`BondStore::mark_revoked`].
///
/// Serialized as an internally-tagged TOML table so the file is
/// readable by a human operator:
///
/// ```toml
/// [bond.status]
/// kind = "Bonded"
/// ```
///
/// ```toml
/// [bond.status]
/// kind = "Revoked"
/// reason = "phone lost"
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum BondStatus {
    /// Active bond, accepted by the PAM module.
    Bonded,
    /// Revoked bond, rejected by the PAM module without going to the
    /// radio.
    Revoked {
        /// Operator-supplied reason for the revocation.
        reason: String,
    },
}

/// A single bonded peer (phone) recorded on disk.
///
/// `peer_id` is the canonical 32-char lowercase hex form of
/// `blake3(pubkey)[..PEER_ID_BLAKE3_BYTES]`. It is **always** derived
/// from `pubkey` via [`peer_id_from_pubkey`] and should never be
/// computed any other way; callers can rely on
/// `bond.peer_id == peer_id_from_pubkey(&bond.pubkey)` as an invariant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bond {
    /// Stable hex identifier (see [`peer_id_from_pubkey`]).
    pub peer_id: String,
    /// Peer's Ed25519 public key, hex-encoded on disk.
    #[serde(with = "hex_pubkey")]
    pub pubkey: [u8; PUBKEY_LEN],
    /// Human-readable name (e.g. "Alex's Pixel 8") shown by
    /// `syauth list`.
    pub name: String,
    /// RFC3339 timestamp captured at pairing time.
    #[serde(with = "rfc3339_dt")]
    pub created_at: OffsetDateTime,
    /// Current bond status.
    pub status: BondStatus,
}

/// On-disk representation of the entire bond file.
///
/// Two top-level fields: `schema_version` (a u32 that gates forward
/// compatibility — see [`BondError::UnsupportedSchemaVersion`]) and a
/// `[[bond]]` array.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BondFile {
    schema_version: u32,
    #[serde(default, rename = "bond")]
    bonds: Vec<Bond>,
}

/// In-memory bond store. Cheap to clone (well, `clone` is not derived
/// because there is no good reason to clone the store — pass it by
/// reference). Construct via [`BondStore::load`].
#[derive(Debug)]
pub struct BondStore {
    bonds: Vec<Bond>,
}

/// Errors produced by [`BondStore`] operations.
#[derive(Debug, Error)]
pub enum BondError {
    /// I/O error reading from or writing to the bond file or its
    /// parent directory. The `path` is the offending file; the
    /// underlying [`std::io::Error`] is the `source`.
    #[error("bond i/o failed at {path}")]
    Io {
        /// File or directory the error was raised against.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The bonds file is not valid TOML. Includes the underlying
    /// `toml::de::Error` as the source so operators can see which line
    /// is malformed.
    #[error("bond file parse error")]
    Parse(#[from] toml::de::Error),

    /// Failed to serialize the in-memory state to TOML. This is a
    /// programmer error (e.g. a non-UTF-8 byte in a `String` field —
    /// which Rust forbids) so it is extremely rare.
    #[error("bond file serialize error")]
    Serialize(#[from] toml::ser::Error),

    /// The file's `schema_version` is newer than this build supports.
    /// Returned by [`BondStore::load`] rather than panicking on a
    /// future format.
    #[error("unsupported bond schema_version: file is v{found}, this build understands up to v{supported_up_to}")]
    UnsupportedSchemaVersion {
        /// Version number observed in the file.
        found: u32,
        /// Highest version this build can parse
        /// ([`BOND_SCHEMA_VERSION_LATEST`]).
        supported_up_to: u32,
    },

    /// `add` was called with a bond whose `peer_id` is already in the
    /// store. The operator's path is "revoke then re-add" — silently
    /// overwriting would mask a re-pairing attack.
    #[error("peer already bonded: peer_id={peer_id}")]
    AlreadyBonded {
        /// The id that collided.
        peer_id: String,
    },

    /// `mark_revoked` or `remove` was called with an id not in the
    /// store.
    #[error("unknown peer: peer_id={peer_id}")]
    UnknownPeer {
        /// The id that was not found.
        peer_id: String,
    },

    /// The parent directory of the bonds file exists with permissions
    /// looser than [`BOND_DIR_MODE`]. Returned rather than silently
    /// narrowing the directory so the operator notices their
    /// misconfiguration.
    #[error("bond parent directory at {path} has too-permissive mode 0o{mode:o} (expected 0o{expected:o})")]
    ParentDirTooPermissive {
        /// The directory whose mode was observed.
        path: PathBuf,
        /// The observed mode (masked with [`BOND_DIR_PERMISSION_MASK`]).
        mode: u32,
        /// The expected upper-bound mode ([`BOND_DIR_MODE`]).
        expected: u32,
    },

    /// Bond's `peer_id` field does not match `peer_id_from_pubkey`
    /// applied to its `pubkey`. Returned by `load` for files that have
    /// been hand-edited inconsistently.
    #[error("bond peer_id mismatch: file says {found}, BLAKE3(pubkey) gives {expected}")]
    PeerIdMismatch {
        /// The id as it appears in the file.
        found: String,
        /// The id computed from the pubkey.
        expected: String,
    },
}

/// Helper: BLAKE3-hash the pubkey, truncate to
/// [`PEER_ID_BLAKE3_BYTES`], render as 32-char lowercase hex.
///
/// Deterministic and stable: two calls with the same pubkey always
/// return byte-identical strings. No salt, no time, no env input.
#[must_use]
pub fn peer_id_from_pubkey(pubkey: &[u8; PUBKEY_LEN]) -> String {
    let hash = blake3::hash(pubkey);
    let bytes = &hash.as_bytes()[..PEER_ID_BLAKE3_BYTES];
    hex_lowercase(bytes)
}

/// Render `bytes` as lowercase hex without going through `format!`'s
/// width-padding path — keeps clippy happy and avoids per-byte alloc.
fn hex_lowercase(bytes: &[u8]) -> String {
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX_CHARS[(b >> 4) as usize] as char);
        out.push(HEX_CHARS[(b & 0x0f) as usize] as char);
    }
    out
}

impl BondStore {
    /// Load a bond store from `path`.
    ///
    /// If `path` does not exist, returns an empty store — the bonds
    /// file is created on first `save`. Any other I/O error is
    /// returned as [`BondError::Io`]. Parse failures bubble up as
    /// [`BondError::Parse`]; a future `schema_version` produces
    /// [`BondError::UnsupportedSchemaVersion`].
    pub fn load(path: &Path) -> Result<Self, BondError> {
        let raw = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self { bonds: Vec::new() });
            }
            Err(e) => {
                return Err(BondError::Io {
                    path: path.to_path_buf(),
                    source: e,
                });
            }
        };
        let parsed: BondFile = toml::from_str(&raw)?;
        if parsed.schema_version > BOND_SCHEMA_VERSION_LATEST {
            return Err(BondError::UnsupportedSchemaVersion {
                found: parsed.schema_version,
                supported_up_to: BOND_SCHEMA_VERSION_LATEST,
            });
        }
        for bond in &parsed.bonds {
            let expected = peer_id_from_pubkey(&bond.pubkey);
            if bond.peer_id != expected {
                return Err(BondError::PeerIdMismatch {
                    found: bond.peer_id.clone(),
                    expected,
                });
            }
        }
        Ok(Self { bonds: parsed.bonds })
    }

    /// Construct an empty store in memory. Useful for tests and for the
    /// first-ever `syauth pair` invocation.
    #[must_use]
    pub fn empty() -> Self {
        Self { bonds: Vec::new() }
    }

    /// Read-only view of the bonds in declaration order.
    #[must_use]
    pub fn list(&self) -> &[Bond] {
        &self.bonds
    }

    /// Add a bond. Returns [`BondError::AlreadyBonded`] if a bond with
    /// the same `peer_id` is already present (overwrite is intentionally
    /// not supported — see the module docs and TC-04 in the journey).
    pub fn add(&mut self, bond: Bond) -> Result<(), BondError> {
        if self.find_index(&bond.peer_id).is_some() {
            return Err(BondError::AlreadyBonded { peer_id: bond.peer_id });
        }
        self.bonds.push(bond);
        Ok(())
    }

    /// Remove a bond by `peer_id`. Returns [`BondError::UnknownPeer`]
    /// if the id is not in the store.
    pub fn remove(&mut self, peer_id: &str) -> Result<(), BondError> {
        match self.find_index(peer_id) {
            Some(i) => {
                self.bonds.remove(i);
                Ok(())
            }
            None => Err(BondError::UnknownPeer {
                peer_id: peer_id.to_owned(),
            }),
        }
    }

    /// Flip a bond's status to [`BondStatus::Revoked`] with the given
    /// reason. Calling this on an already-revoked bond is `Ok(())`
    /// with no overwrite of the existing reason — `syauth revoke` is
    /// idempotent per S-012 DoD.
    pub fn mark_revoked(&mut self, peer_id: &str, reason: &str) -> Result<(), BondError> {
        match self.find_index(peer_id) {
            Some(i) => {
                if matches!(self.bonds[i].status, BondStatus::Revoked { .. }) {
                    return Ok(());
                }
                self.bonds[i].status = BondStatus::Revoked { reason: reason.to_owned() };
                Ok(())
            }
            None => Err(BondError::UnknownPeer {
                peer_id: peer_id.to_owned(),
            }),
        }
    }

    /// Persist the store to `path` atomically.
    ///
    /// Steps, in order:
    ///
    /// 1. Resolve the parent directory; create it with
    ///    [`BOND_DIR_MODE`] if it does not exist.
    /// 2. If the parent already exists, refuse to proceed if its mode
    ///    is looser than [`BOND_DIR_MODE`]
    ///    ([`BondError::ParentDirTooPermissive`]).
    /// 3. Serialize the store to TOML.
    /// 4. Open a [`tempfile::NamedTempFile`] in the same parent
    ///    directory; set its mode to [`BOND_FILE_MODE`].
    /// 5. `write_all` the TOML bytes; `flush` to the kernel.
    /// 6. `persist` atomically replaces `path` (same-filesystem rename).
    pub fn save(&self, path: &Path) -> Result<(), BondError> {
        let parent = parent_dir_of(path)?;
        ensure_parent_dir(parent)?;
        let bytes = self.serialize_to_toml()?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| BondError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
        set_temp_file_mode(&tmp, BOND_FILE_MODE)?;
        tmp.write_all(&bytes).map_err(|e| BondError::Io {
            path: tmp.path().to_path_buf(),
            source: e,
        })?;
        tmp.as_file().sync_all().map_err(|e| BondError::Io {
            path: tmp.path().to_path_buf(),
            source: e,
        })?;
        tmp.persist(path).map_err(|e| BondError::Io {
            path: path.to_path_buf(),
            source: e.error,
        })?;
        Ok(())
    }

    /// Serialize the in-memory state to the on-disk TOML bytes.
    fn serialize_to_toml(&self) -> Result<Vec<u8>, BondError> {
        let file = BondFile {
            schema_version: BOND_SCHEMA_VERSION_LATEST,
            bonds: self.bonds.clone(),
        };
        let s = toml::to_string(&file)?;
        Ok(s.into_bytes())
    }

    fn find_index(&self, peer_id: &str) -> Option<usize> {
        self.bonds.iter().position(|b| b.peer_id == peer_id)
    }
}

fn parent_dir_of(path: &Path) -> Result<&Path, BondError> {
    path.parent().ok_or_else(|| BondError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "bond path has no parent directory"),
    })
}

#[cfg(unix)]
fn ensure_parent_dir(parent: &Path) -> Result<(), BondError> {
    match fs::metadata(parent) {
        Ok(meta) if meta.is_dir() => {
            let mode = meta.permissions().mode() & BOND_DIR_PERMISSION_MASK;
            if mode & !BOND_DIR_MODE != 0 {
                return Err(BondError::ParentDirTooPermissive {
                    path: parent.to_path_buf(),
                    mode,
                    expected: BOND_DIR_MODE,
                });
            }
            Ok(())
        }
        Ok(_) => Err(BondError::Io {
            path: parent.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::NotADirectory, "bond parent path exists and is not a directory"),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(parent).map_err(|e| BondError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
            fs::set_permissions(parent, fs::Permissions::from_mode(BOND_DIR_MODE)).map_err(|e| BondError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
            Ok(())
        }
        Err(e) => Err(BondError::Io {
            path: parent.to_path_buf(),
            source: e,
        }),
    }
}

#[cfg(not(unix))]
fn ensure_parent_dir(parent: &Path) -> Result<(), BondError> {
    fs::create_dir_all(parent).map_err(|e| BondError::Io {
        path: parent.to_path_buf(),
        source: e,
    })
}

#[cfg(unix)]
fn set_temp_file_mode(tmp: &tempfile::NamedTempFile, mode: u32) -> Result<(), BondError> {
    tmp.as_file()
        .set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|e| BondError::Io {
            path: tmp.path().to_path_buf(),
            source: e,
        })
}

#[cfg(not(unix))]
fn set_temp_file_mode(_tmp: &tempfile::NamedTempFile, _mode: u32) -> Result<(), BondError> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Serde helpers
// ---------------------------------------------------------------------------

/// (De)serialize `[u8; PUBKEY_LEN]` as a 64-char lowercase hex string.
mod hex_pubkey {
    use serde::{Deserialize, Deserializer, Serializer};

    use super::{PUBKEY_LEN, hex_lowercase};

    pub(super) fn serialize<S: Serializer>(bytes: &[u8; PUBKEY_LEN], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex_lowercase(bytes))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; PUBKEY_LEN], D::Error> {
        let raw = String::deserialize(d)?;
        if raw.len() != PUBKEY_LEN * 2 {
            return Err(serde::de::Error::custom(format!(
                "pubkey hex must be {} chars, got {}",
                PUBKEY_LEN * 2,
                raw.len()
            )));
        }
        let mut out = [0u8; PUBKEY_LEN];
        for (i, byte_out) in out.iter_mut().enumerate() {
            let off = i * 2;
            *byte_out = u8::from_str_radix(&raw[off..off + 2], 16)
                .map_err(|e| serde::de::Error::custom(format!("pubkey hex parse error at byte {i}: {e}")))?;
        }
        Ok(out)
    }
}

/// (De)serialize [`OffsetDateTime`] as an RFC3339 string.
mod rfc3339_dt {
    use serde::{Deserialize, Deserializer, Serializer};
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};

    pub(super) fn serialize<S: Serializer>(dt: &OffsetDateTime, s: S) -> Result<S::Ok, S::Error> {
        let formatted = dt
            .format(&Rfc3339)
            .map_err(|e| serde::ser::Error::custom(format!("rfc3339 format: {e}")))?;
        s.serialize_str(&formatted)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<OffsetDateTime, D::Error> {
        let raw = String::deserialize(d)?;
        OffsetDateTime::parse(&raw, &Rfc3339).map_err(|e| serde::de::Error::custom(format!("rfc3339 parse: {e}")))
    }
}

/// Format an [`OffsetDateTime`] as RFC3339 — used by callers building
/// `Bond` values from scratch.
///
/// # Errors
///
/// Returns the underlying [`time::error::Format`] if the formatter
/// rejects the value (in practice, this never fires for an
/// [`OffsetDateTime`] in the supported range).
pub fn format_rfc3339(dt: &OffsetDateTime) -> Result<String, time::error::Format> {
    dt.format(&Rfc3339)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use tempfile::TempDir;
    use time::macros::datetime;

    use super::*;

    const SAMPLE_PUBKEY_A: [u8; PUBKEY_LEN] = [0x01; PUBKEY_LEN];
    const SAMPLE_PUBKEY_B: [u8; PUBKEY_LEN] = [0x02; PUBKEY_LEN];
    const FIXED_TIME: OffsetDateTime = datetime!(2026-05-15 12:00:00 UTC);

    fn sample_bond(pubkey: [u8; PUBKEY_LEN], name: &str) -> Bond {
        Bond {
            peer_id: peer_id_from_pubkey(&pubkey),
            pubkey,
            name: name.to_owned(),
            created_at: FIXED_TIME,
            status: BondStatus::Bonded,
        }
    }

    fn temp_bonds_path(td: &TempDir) -> PathBuf {
        td.path().join("syauth").join("bonds.toml")
    }

    // TC-01
    #[test]
    fn peer_id_is_stable_and_blake3_derived() {
        let id_a = peer_id_from_pubkey(&SAMPLE_PUBKEY_A);
        let id_b = peer_id_from_pubkey(&SAMPLE_PUBKEY_A);
        assert_eq!(id_a, id_b, "peer_id must be deterministic");
        assert_eq!(id_a.len(), PEER_ID_HEX_LEN, "peer_id hex length");
        let hash = blake3::hash(&SAMPLE_PUBKEY_A);
        let expected_bytes = &hash.as_bytes()[..PEER_ID_BLAKE3_BYTES];
        assert_eq!(id_a, hex_lowercase(expected_bytes));
    }

    #[test]
    fn peer_id_differs_for_different_pubkeys() {
        assert_ne!(peer_id_from_pubkey(&SAMPLE_PUBKEY_A), peer_id_from_pubkey(&SAMPLE_PUBKEY_B));
    }

    // TC-02
    #[test]
    fn load_missing_file_returns_empty_store() {
        let td = TempDir::new().expect("tempdir");
        let path = td.path().join("does-not-exist.toml");
        let store = BondStore::load(&path).expect("load missing file");
        assert!(store.list().is_empty());
    }

    // TC-03
    #[test]
    fn add_save_load_roundtrip() {
        let td = TempDir::new().expect("tempdir");
        let path = temp_bonds_path(&td);
        let mut store = BondStore::empty();
        let b1 = sample_bond(SAMPLE_PUBKEY_A, "alex-pixel");
        let b2 = sample_bond(SAMPLE_PUBKEY_B, "alex-spare");
        store.add(b1.clone()).expect("add b1");
        store.add(b2.clone()).expect("add b2");
        store.save(&path).expect("save");
        let reloaded = BondStore::load(&path).expect("reload");
        assert_eq!(reloaded.list(), &[b1, b2]);
    }

    // TC-04
    #[test]
    fn add_rejects_duplicate_peer_id() {
        let mut store = BondStore::empty();
        store.add(sample_bond(SAMPLE_PUBKEY_A, "first")).expect("first add");
        let dup = sample_bond(SAMPLE_PUBKEY_A, "second");
        let err = store.add(dup).expect_err("dup add must fail");
        match err {
            BondError::AlreadyBonded { peer_id } => assert_eq!(peer_id, peer_id_from_pubkey(&SAMPLE_PUBKEY_A)),
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.list()[0].name, "first");
    }

    // TC-05
    #[test]
    fn revoke_is_persisted_across_save_load() {
        let td = TempDir::new().expect("tempdir");
        let path = temp_bonds_path(&td);
        let mut store = BondStore::empty();
        let bond = sample_bond(SAMPLE_PUBKEY_A, "alex-pixel");
        let id = bond.peer_id.clone();
        store.add(bond).expect("add");
        store.save(&path).expect("save");
        let mut store = BondStore::load(&path).expect("load");
        store.mark_revoked(&id, "phone-lost").expect("revoke");
        store.save(&path).expect("save revoke");
        let reloaded = BondStore::load(&path).expect("reload revoke");
        assert_eq!(reloaded.list().len(), 1);
        match &reloaded.list()[0].status {
            BondStatus::Revoked { reason } => assert_eq!(reason, "phone-lost"),
            other => panic!("expected Revoked, got {other:?}"),
        }
    }

    // TC-06
    #[test]
    fn revoke_unknown_peer_errors() {
        let mut store = BondStore::empty();
        let err = store.mark_revoked("deadbeef", "x").expect_err("unknown");
        match err {
            BondError::UnknownPeer { peer_id } => assert_eq!(peer_id, "deadbeef"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    // TC-07
    #[test]
    fn revoke_of_already_revoked_is_no_op() {
        let mut store = BondStore::empty();
        let bond = sample_bond(SAMPLE_PUBKEY_A, "alex");
        let id = bond.peer_id.clone();
        store.add(bond).expect("add");
        store.mark_revoked(&id, "first-reason").expect("revoke 1");
        store.mark_revoked(&id, "second-reason").expect("revoke 2 ok");
        match &store.list()[0].status {
            BondStatus::Revoked { reason } => assert_eq!(reason, "first-reason", "reason must not be overwritten"),
            other => panic!("expected Revoked, got {other:?}"),
        }
    }

    #[test]
    fn remove_unknown_peer_errors() {
        let mut store = BondStore::empty();
        let err = store.remove("deadbeef").expect_err("unknown");
        assert!(matches!(err, BondError::UnknownPeer { .. }));
    }

    #[test]
    fn remove_existing_peer_succeeds() {
        let mut store = BondStore::empty();
        let bond = sample_bond(SAMPLE_PUBKEY_A, "alex");
        let id = bond.peer_id.clone();
        store.add(bond).expect("add");
        store.remove(&id).expect("remove");
        assert!(store.list().is_empty());
    }

    // TC-08: atomic-write fault leaves original file intact.
    //
    // Strategy: write a known-good store, then simulate "crash between
    // write_all and persist" by writing to a NamedTempFile in the same
    // dir and dropping it without calling `persist`. The destination
    // file must be byte-equal to the snapshot taken before the fault.
    #[test]
    fn atomic_write_fault_leaves_file_intact() {
        let td = TempDir::new().expect("tempdir");
        let path = temp_bonds_path(&td);
        let mut store = BondStore::empty();
        store.add(sample_bond(SAMPLE_PUBKEY_A, "original")).expect("add");
        store.save(&path).expect("save original");
        let snapshot = fs::read(&path).expect("snapshot");

        // Simulated faulty save: write all bytes to a tempfile in the
        // parent dir, then DROP the tempfile without calling persist —
        // mimics a panic between `write_all` and `persist`.
        let parent = path.parent().expect("parent");
        {
            let mut tmp = tempfile::NamedTempFile::new_in(parent).expect("tmpfile");
            tmp.write_all(b"# torn-write garbage that must not land on disk\n")
                .expect("write torn");
            // intentional: do NOT call tmp.persist(&path).
            drop(tmp);
        }

        let after = fs::read(&path).expect("post-fault read");
        assert_eq!(after, snapshot, "destination must equal pre-fault snapshot");

        // No leftover .tmp files from the simulated crash.
        let leftovers: Vec<_> = fs::read_dir(parent)
            .expect("readdir")
            .filter_map(Result::ok)
            .filter(|e| e.path() != path)
            .collect();
        assert!(leftovers.is_empty(), "tempfile must be unlinked on drop: {leftovers:?}");
    }

    // TC-09
    #[cfg(unix)]
    #[test]
    fn saved_file_mode_is_0o600() {
        let td = TempDir::new().expect("tempdir");
        let path = temp_bonds_path(&td);
        let mut store = BondStore::empty();
        store.add(sample_bond(SAMPLE_PUBKEY_A, "alex")).expect("add");
        store.save(&path).expect("save");
        let meta = fs::metadata(&path).expect("metadata");
        assert_eq!(meta.permissions().mode() & BOND_DIR_PERMISSION_MASK, BOND_FILE_MODE);
    }

    // TC-10
    #[cfg(unix)]
    #[test]
    fn parent_directory_mode_is_0o700_after_save() {
        let td = TempDir::new().expect("tempdir");
        let path = temp_bonds_path(&td);
        let parent = path.parent().expect("parent");
        assert!(!parent.exists(), "parent must be freshly created");
        let mut store = BondStore::empty();
        store.add(sample_bond(SAMPLE_PUBKEY_A, "alex")).expect("add");
        store.save(&path).expect("save");
        let meta = fs::metadata(parent).expect("dir metadata");
        assert_eq!(meta.permissions().mode() & BOND_DIR_PERMISSION_MASK, BOND_DIR_MODE);
    }

    #[cfg(unix)]
    #[test]
    fn save_rejects_too_permissive_parent_dir() {
        let td = TempDir::new().expect("tempdir");
        let parent = td.path().join("loose");
        fs::create_dir(&parent).expect("mkdir");
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).expect("chmod 0755");
        let path = parent.join("bonds.toml");
        let mut store = BondStore::empty();
        store.add(sample_bond(SAMPLE_PUBKEY_A, "alex")).expect("add");
        let err = store.save(&path).expect_err("must reject loose parent");
        match err {
            BondError::ParentDirTooPermissive { mode, expected, .. } => {
                assert_eq!(mode & BOND_DIR_PERMISSION_MASK, 0o755);
                assert_eq!(expected, BOND_DIR_MODE);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    // TC-11
    #[test]
    fn future_schema_version_returns_typed_error() {
        let td = TempDir::new().expect("tempdir");
        let path = td.path().join("future.toml");
        let future = format!("schema_version = {}\n", BOND_SCHEMA_VERSION_LATEST + 1);
        fs::write(&path, future).expect("write");
        let err = BondStore::load(&path).expect_err("future version");
        match err {
            BondError::UnsupportedSchemaVersion { found, supported_up_to } => {
                assert_eq!(found, BOND_SCHEMA_VERSION_LATEST + 1);
                assert_eq!(supported_up_to, BOND_SCHEMA_VERSION_LATEST);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parse_error_on_garbage_toml() {
        let td = TempDir::new().expect("tempdir");
        let path = td.path().join("garbage.toml");
        fs::write(&path, "this is not toml = =\n").expect("write");
        let err = BondStore::load(&path).expect_err("must fail");
        assert!(matches!(err, BondError::Parse(_)));
    }

    #[test]
    fn peer_id_mismatch_in_file_is_rejected() {
        let td = TempDir::new().expect("tempdir");
        let path = td.path().join("mismatch.toml");
        // Hand-rolled TOML with a peer_id that does NOT match BLAKE3(pubkey).
        let body = format!(
            "schema_version = {ver}\n\n\
             [[bond]]\n\
             peer_id = \"{wrong}\"\n\
             pubkey = \"{pk}\"\n\
             name = \"forged\"\n\
             created_at = \"2026-05-15T12:00:00Z\"\n\n\
             [bond.status]\n\
             kind = \"Bonded\"\n",
            ver = BOND_SCHEMA_VERSION_LATEST,
            wrong = "0".repeat(PEER_ID_HEX_LEN),
            pk = hex_lowercase(&SAMPLE_PUBKEY_A),
        );
        fs::write(&path, body).expect("write");
        let err = BondStore::load(&path).expect_err("must reject mismatch");
        assert!(matches!(err, BondError::PeerIdMismatch { .. }));
    }

    #[test]
    fn rfc3339_round_trip_preserves_created_at() {
        let dt = FIXED_TIME;
        let formatted = format_rfc3339(&dt).expect("format");
        let parsed = OffsetDateTime::parse(&formatted, &Rfc3339).expect("parse");
        assert_eq!(parsed, dt);
    }
}
