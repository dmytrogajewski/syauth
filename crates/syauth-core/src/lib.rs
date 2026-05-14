//! `syauth-core` — the shared protocol core consumed by the PAM module
//! (`syauth-pam`), the transport (`syauth-transport`), the CLI
//! (`syauth-cli`), and the Android companion (`syauth-mobile`, via UniFFI).
//!
//! Layered as roadmap items land:
//!
//! - **S-002** — v1 wire-format [`Frame`] encoder / decoder.
//! - **S-003** — sliding LRU + TTL replay nonce cache.
//! - **S-004** — Ed25519 signing + BLAKE3-keyed-hash MAC (this commit).
//! - **S-005** — bond store TOML schema.
//! - S-006 — kernel-keyring / libsecret abstraction.
//!
//! See `specs/syauth/SPEC.md` for the protocol design,
//! `specs/journeys/JOURNEY-S-002-protocol-framing.md` for the framing
//! rationale, `specs/journeys/JOURNEY-S-003-replay-defense.md` for the replay
//! cache rationale, `specs/journeys/JOURNEY-S-004-crypto-primitives.md` for
//! the signing/MAC rationale, and
//! `specs/journeys/JOURNEY-S-005-bond-store.md` for the bond-store rationale.

#![deny(missing_docs)]
#![deny(unsafe_code)]

pub mod bond;
pub mod frame;
pub mod mac;
pub mod replay;
pub mod sign;

pub use bond::{
    BOND_DIR_MODE, BOND_FILE_MODE, BOND_SCHEMA_VERSION_LATEST, Bond, BondError, BondStatus, BondStore, PEER_ID_BLAKE3_BYTES,
    peer_id_from_pubkey,
};
pub use frame::{
    Frame, FrameError, HEADER_LEN, MAX_FRAME_LEN, MAX_PAYLOAD_LEN, MIN_FRAME_LEN, NONCE_LEN, NONCE_OFFSET, PAYLOAD_OFFSET,
    SYAUTH_WIRE_VERSION_V1, TAG_LEN, VERSION_LEN, VERSION_OFFSET,
};
pub use mac::{BOND_KEY_BYTES, MAC_TAG_LEN, compute_tag, verify_tag};
pub use replay::{Acceptance, DEFAULT_REPLAY_CAP, DEFAULT_REPLAY_TTL, ReplayCache};
pub use sign::{SIGNATURE_LEN, SIGNED_MESSAGE_PREFIX_LEN, Signature, SigningKey, VerifyError, VerifyingKey, sign_frame, verify_frame};
