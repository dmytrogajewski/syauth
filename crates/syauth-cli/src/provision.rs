//! `syauth-cli` — `provision-test` subcommand: pre-seeded test bond.
//!
//! GAP: DEV-001 — this whole module substitutes a plaintext-TOML +
//! adb-push handshake for the SPEC §3.2 D5 LESC numeric-comparison
//! pairing flow + §3.3 ML 6-digit OOB confirmation. The subcommand
//! exists only until S-011 (CLI `pair`) and S-016 (Android pairing
//! screen) lose their `StubPairBackend` and ship real LESC; on
//! closure of DEV-001 this entire file is removed (or gated behind a
//! `--features=demo` flag that is OFF in every release build). See
//! `docs/known-gaps.md` row DEV-001.
//!
//! What gets generated:
//!
//! 1. A 32-byte BLAKE3 MAC seed (`bond_key`) — shared secret used by the
//!    PAM module to MAC-tag challenge frames and verify response MAC
//!    tags. Both ends must agree.
//! 2. An Ed25519 keypair representing the **phone's identity** —
//!    private half goes to the phone, public half goes on the desktop's
//!    bond record so the PAM module can `verify_frame` signed responses.
//!
//! What gets persisted on the desktop:
//!
//! - `Bond` record with `peer_id`, `pubkey`, `name`, `created_at`,
//!   `status: Bonded` → atomically written to
//!   `<bond_dir>/bonds.toml` via [`BondStore::save`].
//! - `bond_key` written to `<bond_dir>/keys/<peer_id>.bin` with mode
//!   0600. The PAM module's `load_bond_key` reads this file directly
//!   when no in-process test keystore is installed (the kernel-keyring
//!   wiring is a v0.2 task; the file-backed fallback is the minimum
//!   needed to demo a working unlock).
//!
//! What gets emitted to the operator:
//!
//! A `syauth-provision.toml` package containing the bond_key,
//! phone-private-key seed, and the bond metadata. The operator
//! transports this file to the phone (e.g., `adb push
//! syauth-provision.toml /sdcard/Download/`). The phone reads it on
//! first launch and persists its half.
//!
//! ## Security note
//!
//! The provision package contains a private key in plaintext. It is
//! safe to transport over a USB cable (`adb push`) under the
//! operator's physical control; it must NOT be sent over a network or
//! shared storage. The package's `[security]` block documents this in
//! the file header so a future reader notices.

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use clap::Parser;
use rand::{RngCore, rngs::OsRng};
use serde::Serialize;
use syauth_core::{BOND_KEY_BYTES, Bond, BondStatus, BondStore, SigningKey, bond::PUBKEY_LEN, peer_id_from_pubkey};
use thiserror::Error;
use time::OffsetDateTime;

use crate::pair::{BONDS_FILE_NAME, DEFAULT_BOND_DIR};

/// Mode bits for the per-peer bond_key file. The PAM module runs as
/// `root` under `pam_sm_authenticate` so 0600 is sufficient; group/other
/// must NOT read this byte.
pub const BOND_KEY_FILE_MODE: u32 = 0o600;

/// Mode bits for the `keys/` directory. 0700 mirrors the SPEC §4.4
/// `/var/lib/syauth/` parent.
pub const KEYS_DIR_MODE: u32 = 0o700;

/// Subdir of `<bond_dir>` that holds per-peer bond_key files.
pub const KEYS_DIR_NAME: &str = "keys";

/// File the provision subcommand writes for the operator to ship to the
/// phone. Always written next to where the operator runs the command;
/// the `--output` flag overrides.
pub const DEFAULT_PROVISION_FILE_NAME: &str = "syauth-provision.toml";

/// Schema version baked into the provision package. The phone's
/// consumer checks this and refuses any mismatch loudly.
pub const PROVISION_SCHEMA_VERSION: u32 = 1;

/// Human-readable footer banner emitted to stdout after a successful
/// provision. Pinned as a constant so a future regression test can
/// grep for it without hard-coding the string twice.
pub const PROVISION_SUCCESS_BANNER: &str = "==> provision-test complete";

/// Required length bounds for the host-name argument. The CLI expects
/// a short hostname-like string, not a free-form arbitrary label.
/// Empty names are rejected.
pub const HOST_NAME_MIN_LEN: usize = 1;
pub const HOST_NAME_MAX_LEN: usize = 64;

/// CLI options for `syauth provision-test`.
#[derive(Debug, Parser, Clone)]
pub struct ProvisionOpts {
    /// Display name for the phone in `syauth list` and on the phone's
    /// "Approve unlock for ..." prompt.
    #[arg(long)]
    pub name: String,

    /// Directory where `bonds.toml` and `keys/` live. Defaults to
    /// `/var/lib/syauth`. Created with mode 0700 if missing (requires
    /// root in production; tests pass a tempdir).
    #[arg(long, default_value = DEFAULT_BOND_DIR)]
    pub bond_dir: PathBuf,

    /// Where to write the provision package the phone consumes.
    /// Defaults to `./syauth-provision.toml`.
    #[arg(long)]
    pub output: Option<PathBuf>,
}

/// Typed errors that surface as a nonzero exit code on the binary.
#[derive(Debug, Error)]
pub enum ProvisionError {
    #[error("host name must be {HOST_NAME_MIN_LEN}..={HOST_NAME_MAX_LEN} bytes, got {got}")]
    InvalidName { got: usize },

    #[error("failed to create directory {path}: {source}")]
    Mkdir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("bond store error: {0}")]
    BondStore(#[from] syauth_core::BondError),

    #[error("serialize provision package: {0}")]
    Serialize(#[from] toml::ser::Error),
}

/// What the subcommand returns when it succeeds. Useful for tests +
/// for the binary's `println!` so the operator sees the peer_id and
/// the absolute path to the provision file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionOutcome {
    pub peer_id: String,
    pub bond_path: PathBuf,
    pub bond_key_path: PathBuf,
    pub provision_path: PathBuf,
}

/// Provision package — the structure of the TOML file the operator
/// adb-pushes to the phone.
#[derive(Debug, Serialize)]
struct ProvisionPackage<'a> {
    schema_version: u32,
    host_name: &'a str,
    peer_id: &'a str,
    bond_key_hex: String,
    phone_signing_key_hex: String,
    phone_pubkey_hex: String,
    created_at: String,
    /// Documentation block: spells out the security constraint inside
    /// the file so a future reader notices even if the operator
    /// emailed it around by mistake.
    _security_note: &'a str,
}

/// Run the provision flow. All randomness comes from [`OsRng`].
pub fn run_provision_test(opts: &ProvisionOpts) -> Result<ProvisionOutcome, ProvisionError> {
    validate_name(&opts.name)?;

    // 1. Generate phone identity Ed25519 keypair.
    let mut sk_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut sk_bytes);
    let phone_signing_key = SigningKey::from_bytes(&sk_bytes);
    let phone_pubkey: [u8; PUBKEY_LEN] = phone_signing_key.verifying_key().to_bytes();

    // 2. Generate the shared bond_key.
    let mut bond_key = [0u8; BOND_KEY_BYTES];
    OsRng.fill_bytes(&mut bond_key);

    // 3. Derive peer_id (BLAKE3 over the pubkey).
    let peer_id = peer_id_from_pubkey(&phone_pubkey);

    // 4. Persist desktop half.
    ensure_dir(&opts.bond_dir, KEYS_DIR_MODE)?;
    let keys_dir = opts.bond_dir.join(KEYS_DIR_NAME);
    ensure_dir(&keys_dir, KEYS_DIR_MODE)?;
    let bonds_path = opts.bond_dir.join(BONDS_FILE_NAME);
    let mut store = match BondStore::load(&bonds_path) {
        Ok(s) => s,
        Err(syauth_core::BondError::Io { .. }) => BondStore::empty(),
        Err(other) => return Err(ProvisionError::BondStore(other)),
    };
    let created_at = OffsetDateTime::now_utc();
    let bond = Bond {
        peer_id: peer_id.clone(),
        pubkey: phone_pubkey,
        name: opts.name.clone(),
        created_at,
        status: BondStatus::Bonded,
    };
    store.add(bond)?;
    store.save(&bonds_path)?;

    let bond_key_path = keys_dir.join(format!("{peer_id}.bin"));
    write_bond_key_file(&bond_key_path, &bond_key)?;

    // 5. Emit provision package.
    let provision_path = match &opts.output {
        Some(p) => p.clone(),
        None => PathBuf::from(DEFAULT_PROVISION_FILE_NAME),
    };
    let pkg = ProvisionPackage {
        schema_version: PROVISION_SCHEMA_VERSION,
        host_name: &opts.name,
        peer_id: &peer_id,
        bond_key_hex: hex::encode(bond_key),
        phone_signing_key_hex: hex::encode(sk_bytes),
        phone_pubkey_hex: hex::encode(phone_pubkey),
        created_at: created_at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| String::from("0000-01-01T00:00:00Z")),
        _security_note: "Contains a plaintext Ed25519 private key. Transport ONLY over a trusted USB cable (`adb push`). Never email or store in shared storage.",
    };
    let toml_body = toml::to_string_pretty(&pkg)?;
    write_provision_file(&provision_path, &toml_body)?;

    Ok(ProvisionOutcome {
        peer_id,
        bond_path: bonds_path,
        bond_key_path,
        provision_path,
    })
}

fn validate_name(name: &str) -> Result<(), ProvisionError> {
    let n = name.len();
    if !(HOST_NAME_MIN_LEN..=HOST_NAME_MAX_LEN).contains(&n) {
        return Err(ProvisionError::InvalidName { got: n });
    }
    Ok(())
}

fn ensure_dir(path: &Path, mode: u32) -> Result<(), ProvisionError> {
    if !path.exists() {
        fs::create_dir_all(path).map_err(|source| ProvisionError::Mkdir {
            path: path.to_owned(),
            source,
        })?;
    }
    let perms = fs::Permissions::from_mode(mode);
    fs::set_permissions(path, perms).map_err(|source| ProvisionError::Write {
        path: path.to_owned(),
        source,
    })?;
    Ok(())
}

fn write_bond_key_file(path: &Path, bond_key: &[u8; BOND_KEY_BYTES]) -> Result<(), ProvisionError> {
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(BOND_KEY_FILE_MODE)
        .open(path)
        .map_err(|source| ProvisionError::Write {
            path: path.to_owned(),
            source,
        })?;
    f.write_all(bond_key).map_err(|source| ProvisionError::Write {
        path: path.to_owned(),
        source,
    })?;
    // Belt-and-braces: enforce mode in case the file already existed
    // with looser perms.
    let perms = fs::Permissions::from_mode(BOND_KEY_FILE_MODE);
    fs::set_permissions(path, perms).map_err(|source| ProvisionError::Write {
        path: path.to_owned(),
        source,
    })?;
    Ok(())
}

fn write_provision_file(path: &Path, body: &str) -> Result<(), ProvisionError> {
    let mut f = File::create(path).map_err(|source| ProvisionError::Write {
        path: path.to_owned(),
        source,
    })?;
    f.write_all(body.as_bytes()).map_err(|source| ProvisionError::Write {
        path: path.to_owned(),
        source,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn opts(td: &TempDir, name: &str) -> ProvisionOpts {
        ProvisionOpts {
            name: name.to_owned(),
            bond_dir: td.path().to_owned(),
            output: Some(td.path().join("provision.toml")),
        }
    }

    #[test]
    fn happy_path_writes_bond_keyfile_and_package() {
        let td = TempDir::new().unwrap();
        let outcome = run_provision_test(&opts(&td, "fedora")).unwrap();

        // Bond store written.
        assert!(outcome.bond_path.exists());
        let store = BondStore::load(&outcome.bond_path).unwrap();
        let bond = store.list().iter().next().unwrap();
        assert_eq!(bond.name, "fedora");
        assert_eq!(bond.status, BondStatus::Bonded);
        assert_eq!(bond.peer_id, outcome.peer_id);

        // Bond key file written + mode 0600.
        assert!(outcome.bond_key_path.exists());
        let mode = fs::metadata(&outcome.bond_key_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, BOND_KEY_FILE_MODE);
        let bytes = fs::read(&outcome.bond_key_path).unwrap();
        assert_eq!(bytes.len(), BOND_KEY_BYTES);

        // Provision file written + parseable.
        assert!(outcome.provision_path.exists());
        let text = fs::read_to_string(&outcome.provision_path).unwrap();
        assert!(text.contains("schema_version = 1"));
        assert!(text.contains("host_name = \"fedora\""));
        assert!(text.contains(&format!("peer_id = \"{}\"", outcome.peer_id)));
    }

    #[test]
    fn rejects_empty_name() {
        let td = TempDir::new().unwrap();
        let err = run_provision_test(&opts(&td, "")).unwrap_err();
        match err {
            ProvisionError::InvalidName { got } => assert_eq!(got, 0),
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn second_run_is_idempotent_for_distinct_peers() {
        let td = TempDir::new().unwrap();
        let a = run_provision_test(&opts(&td, "fedora-a")).unwrap();
        let b = run_provision_test(&opts(&td, "fedora-b")).unwrap();
        assert_ne!(a.peer_id, b.peer_id);
        let store = BondStore::load(&a.bond_path).unwrap();
        assert_eq!(store.list().iter().count(), 2);
    }

    #[test]
    fn package_round_trips_through_provision_struct_for_phone_consumer() {
        let td = TempDir::new().unwrap();
        let outcome = run_provision_test(&opts(&td, "fedora")).unwrap();
        let text = fs::read_to_string(&outcome.provision_path).unwrap();

        // Phone-side will parse this same file; verify the hex-encoded
        // fields are valid lowercase hex of the right length.
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        let tbl = parsed.as_table().unwrap();
        for key in ["bond_key_hex", "phone_signing_key_hex", "phone_pubkey_hex"] {
            let v = tbl.get(key).and_then(|v| v.as_str()).unwrap();
            assert_eq!(v.len(), 64, "field {key} should be 32 bytes hex-encoded");
            assert!(
                v.chars()
                    .all(|c| c.is_ascii_hexdigit() && (!c.is_ascii_alphabetic() || c.is_lowercase())),
                "{key} must be lowercase hex"
            );
        }
    }
}
