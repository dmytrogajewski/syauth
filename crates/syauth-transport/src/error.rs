//! Typed error surface for the `syauth-transport` crate.
//!
//! Every variant exists because at least one row of the SPEC §4.3 e2e matrix
//! (or its `/bt` skill expansion in `.agents/skills/bt/SKILL.md` Phase 4)
//! distinguishes that failure mode at the transport boundary:
//!
//! - [`TransportError::Timeout`]   ← `bt.unlock.timeout`
//! - [`TransportError::Unreachable`] ← `bt.unlock.unreachable`
//! - [`TransportError::Closed`]    ← peer hung up mid-roundtrip
//! - [`TransportError::BadFrame`]  ← framing rejected by `syauth-core`
//! - [`TransportError::WrongVersion`] ← `bt.unlock.version_rejected`
//! - [`TransportError::Replay`]    ← `bt.unlock.nonce_reused`
//!
//! `BadFrame` carries the upstream [`syauth_core::FrameError`] verbatim so
//! callers and tests can assert on the structural variant (`BadVersion`,
//! `TooShort`, `BadLength`) without substring-matching strings.

use syauth_core::FrameError;
use thiserror::Error;

/// Errors produced by the [`crate::BtPeer`] and [`crate::Session`] traits.
///
/// The variants exhaustively partition the failure modes that the upper layer
/// needs to distinguish to pick a PAM return code: `Timeout` and `Unreachable`
/// both map to `PAM_AUTHINFO_UNAVAIL`, while `Closed`, `BadFrame`,
/// `WrongVersion`, and `Replay` all map to `PAM_AUTH_ERR`. Keeping them
/// separate at the transport layer lets `tracing` spans (`bt.unlock.*`) name
/// the specific failure without string-matching.
#[derive(Debug, PartialEq, Eq, Error)]
pub enum TransportError {
    /// The caller's `timeout` expired before the operation completed.
    /// Maps to `PAM_AUTHINFO_UNAVAIL` in the upper layer.
    #[error("transport timeout")]
    Timeout,

    /// The peer cannot be reached at all (radio off, peer not advertising,
    /// adapter not initialised). Distinct from [`TransportError::Timeout`]
    /// because the upper layer reports `bt.unlock.unreachable` rather than
    /// `bt.unlock.timeout` to syslog.
    #[error("transport unreachable")]
    Unreachable,

    /// A previously-established session was closed by the peer mid-roundtrip.
    #[error("transport session closed")]
    Closed,

    /// A wire-format frame could not be parsed by `syauth-core`. The wrapped
    /// [`FrameError`] is the structural variant (`TooShort`, `BadVersion`,
    /// `BadLength`) so callers can match on it.
    #[error("bad frame: {0}")]
    BadFrame(#[from] FrameError),

    /// Peer advertised or sent a frame stamped with an unsupported wire-format
    /// version. The byte is preserved so logs can name the offending value.
    /// `BadFrame(FrameError::BadVersion(_))` is the structural form; this
    /// variant exists for transports (e.g. `bluer` in S-010) that learn the
    /// peer's version from an advertised characteristic *before* a frame is
    /// ever exchanged.
    #[error("peer advertised unsupported wire-format version 0x{0:02x}")]
    WrongVersion(u8),

    /// The transport detected a replayed frame (same nonce twice in one
    /// session window). Distinct from the upper-layer replay cache (S-003);
    /// transports may also flag in-session duplicates the moment they see
    /// them.
    #[error("replayed frame")]
    Replay,
}
