//! `syauth-core` — the shared protocol core consumed by the PAM module
//! (`syauth-pam`), the transport (`syauth-transport`), the CLI
//! (`syauth-cli`), and the Android companion (`syauth-mobile`, via UniFFI).
//!
//! Layered as roadmap items land:
//!
//! - **S-002** — v1 wire-format [`Frame`] encoder / decoder (this commit).
//! - S-003 — replay nonce cache.
//! - S-004 — Ed25519 signing + BLAKE3-keyed-hash MAC.
//! - S-005 — bond store TOML schema.
//! - S-006 — kernel-keyring / libsecret abstraction.
//!
//! See `specs/syauth/SPEC.md` for the protocol design and
//! `specs/journeys/JOURNEY-S-002-protocol-framing.md` for the framing
//! rationale.

#![deny(missing_docs)]

pub mod frame;

pub use frame::{
    Frame, FrameError, HEADER_LEN, MAX_FRAME_LEN, MAX_PAYLOAD_LEN, MIN_FRAME_LEN, NONCE_LEN, NONCE_OFFSET, PAYLOAD_OFFSET,
    SYAUTH_WIRE_VERSION_V1, TAG_LEN, VERSION_LEN, VERSION_OFFSET,
};
