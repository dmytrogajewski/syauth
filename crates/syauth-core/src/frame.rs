//! syauth v1 wire-format frame.
//!
//! Frame layout (SPEC §3.3 / §4.2):
//!
//! ```text
//!   0                   1                   2                   3
//!   0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
//!  +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//!  | ver (1) |                  nonce (16 octets)                   ...
//!  +---------+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//!  | ... nonce continued ...                                          |
//!  +---------------------------------------------------------------+
//!  |                    payload (0..=4096 octets)                   ...
//!  +---------------------------------------------------------------+
//!  |                       tag (16 octets)                         ...
//!  +---------------------------------------------------------------+
//! ```
//!
//! No multi-byte integers cross the wire in v1, so endianness is not
//! observable. Any future header field added in a v2 frame MUST be big-endian
//! (see the doc comment on [`MAX_PAYLOAD_LEN`]).
//!
//! The tag is a placeholder all-zero `[u8; TAG_LEN]` until S-004 lands the
//! BLAKE3-keyed-hash MAC.

use thiserror::Error;

/// On-wire protocol version recognized by this build. SPEC §4.5 mandates
/// that unknown versions are rejected explicitly rather than silently
/// upgraded; see [`FrameError::BadVersion`].
pub const SYAUTH_WIRE_VERSION_V1: u8 = 1;

/// Length in bytes of the version field.
pub const VERSION_LEN: usize = 1;

/// Length in bytes of the per-frame nonce. Matches SPEC §3.3 and the 16-byte
/// nonce used by the replay cache in S-003.
pub const NONCE_LEN: usize = 16;

/// Length in bytes of the integrity tag. Matches the 16-byte BLAKE3-keyed-hash
/// output we will land in S-004; in S-002 the tag is a placeholder.
pub const TAG_LEN: usize = 16;

/// Offset of the version byte from the start of the frame.
pub const VERSION_OFFSET: usize = 0;

/// Offset of the first nonce byte from the start of the frame.
pub const NONCE_OFFSET: usize = VERSION_OFFSET + VERSION_LEN;

/// Offset of the first payload byte from the start of the frame.
pub const PAYLOAD_OFFSET: usize = NONCE_OFFSET + NONCE_LEN;

/// Combined length of the fixed-position header fields (version + nonce).
pub const HEADER_LEN: usize = VERSION_LEN + NONCE_LEN;

/// Minimum legal frame length on the wire — the header plus the trailing tag,
/// with an empty payload.
pub const MIN_FRAME_LEN: usize = HEADER_LEN + TAG_LEN;

/// Hard cap on the payload bytes the parser will accept.
///
/// Chosen as the smallest power of two that comfortably exceeds the largest
/// expected v1 payload (an Ed25519 signature, 64 B, plus 16 B nonce and
/// modest framing overhead), while bounding the decoder's heap appetite to
/// a single page on Linux x86_64. SPEC §4.6 calls out a 4-MTU BLE GATT
/// exchange (~520 B writable per fragment) as the worst case the transport
/// reassembles — well under this cap.
///
/// Any future multi-byte field added in a v2 frame MUST be encoded as
/// big-endian.
pub const MAX_PAYLOAD_LEN: usize = 4096;

/// Maximum legal frame length on the wire.
pub const MAX_FRAME_LEN: usize = MIN_FRAME_LEN + MAX_PAYLOAD_LEN;

/// A v1 syauth protocol frame, parsed and validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Wire-format version byte. Must equal [`SYAUTH_WIRE_VERSION_V1`].
    pub version: u8,
    /// Per-frame 16-byte nonce, used by the replay cache (S-003).
    pub nonce: [u8; NONCE_LEN],
    /// Payload bytes. Bounded by [`MAX_PAYLOAD_LEN`].
    pub payload: Vec<u8>,
    /// Integrity tag. Placeholder all-zero in S-002; computed in S-004.
    pub tag: [u8; TAG_LEN],
}

/// Errors produced by [`Frame::encode`] and [`Frame::decode`].
///
/// Variants exhaustively partition the failure space the decoder cares about,
/// which is why the `cargo fuzz` target in
/// `crates/syauth-core/fuzz/fuzz_targets/frame_parse.rs` can confidently
/// assert that every parse either succeeds or yields one of these errors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FrameError {
    /// Buffer is shorter than the minimum legal frame
    /// ([`MIN_FRAME_LEN`] bytes).
    ///
    /// `needed` is the minimum byte count the parser required; `got` is the
    /// caller's actual byte count. Both are reported so log lines and
    /// integration tests can name the deficit numerically.
    #[error("frame too short: needed {needed} bytes, got {got}")]
    TooShort {
        /// Number of bytes the parser required at this stage.
        needed: usize,
        /// Number of bytes the caller actually supplied.
        got: usize,
    },

    /// Version byte does not equal [`SYAUTH_WIRE_VERSION_V1`].
    ///
    /// Carries the offending byte so the operator can see at a glance whether
    /// the peer is speaking a future version they expected to deploy, or
    /// garbage from a misaligned reader.
    #[error("unsupported wire-format version: 0x{0:02x}")]
    BadVersion(u8),

    /// Payload section exceeds [`MAX_PAYLOAD_LEN`] bytes — either at encode
    /// time (caller constructed an oversized [`Frame`]) or at decode time
    /// (peer sent more bytes between the header and what would have been a
    /// trailing tag than the parser is willing to allocate).
    #[error("frame payload exceeds maximum length")]
    BadLength,
}

impl Frame {
    /// Append this frame's wire-format encoding to `buf`.
    ///
    /// On error, `buf` is left untouched (idempotent failure). The only
    /// possible error is [`FrameError::BadLength`], raised when
    /// `self.payload.len() > MAX_PAYLOAD_LEN`.
    pub fn encode(&self, buf: &mut Vec<u8>) -> Result<(), FrameError> {
        if self.payload.len() > MAX_PAYLOAD_LEN {
            return Err(FrameError::BadLength);
        }
        buf.reserve(HEADER_LEN + self.payload.len() + TAG_LEN);
        buf.push(self.version);
        buf.extend_from_slice(&self.nonce);
        buf.extend_from_slice(&self.payload);
        buf.extend_from_slice(&self.tag);
        Ok(())
    }

    /// Parse a wire-format frame from `input`.
    ///
    /// Returns a [`FrameError`] describing the first failure observed:
    ///
    /// - `TooShort` if `input.len() < MIN_FRAME_LEN`.
    /// - `BadVersion(b)` if `input[0]` is not [`SYAUTH_WIRE_VERSION_V1`].
    ///   The version check runs after the length check so that a one-byte
    ///   input does not produce a misleading version error.
    /// - `BadLength` if the implied payload length
    ///   (`input.len() - MIN_FRAME_LEN`) exceeds [`MAX_PAYLOAD_LEN`].
    pub fn decode(input: &[u8]) -> Result<Self, FrameError> {
        if input.len() < MIN_FRAME_LEN {
            return Err(FrameError::TooShort {
                needed: MIN_FRAME_LEN,
                got: input.len(),
            });
        }
        let version = input[VERSION_OFFSET];
        if version != SYAUTH_WIRE_VERSION_V1 {
            return Err(FrameError::BadVersion(version));
        }
        let payload_len = input.len() - MIN_FRAME_LEN;
        if payload_len > MAX_PAYLOAD_LEN {
            return Err(FrameError::BadLength);
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&input[NONCE_OFFSET..NONCE_OFFSET + NONCE_LEN]);
        let payload_end = PAYLOAD_OFFSET + payload_len;
        let payload = input[PAYLOAD_OFFSET..payload_end].to_vec();
        let mut tag = [0u8; TAG_LEN];
        tag.copy_from_slice(&input[payload_end..payload_end + TAG_LEN]);
        Ok(Frame {
            version,
            nonce,
            payload,
            tag,
        })
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Helper: build a well-formed v1 frame with the given payload length.
    fn frame_with_payload(len: usize) -> Frame {
        Frame {
            version: SYAUTH_WIRE_VERSION_V1,
            nonce: [0x11; NONCE_LEN],
            payload: vec![0x22; len],
            tag: [0x33; TAG_LEN],
        }
    }

    #[test]
    fn golden_encode_matches_byte_layout() {
        let frame = Frame {
            version: SYAUTH_WIRE_VERSION_V1,
            nonce: [0x01; NONCE_LEN],
            payload: vec![0xAA; 4],
            tag: [0xBB; TAG_LEN],
        };
        let mut buf = Vec::new();
        frame.encode(&mut buf).expect("encode");
        let mut expected = Vec::new();
        expected.push(SYAUTH_WIRE_VERSION_V1);
        expected.extend_from_slice(&[0x01; NONCE_LEN]);
        expected.extend_from_slice(&[0xAA; 4]);
        expected.extend_from_slice(&[0xBB; TAG_LEN]);
        assert_eq!(buf, expected);
        assert_eq!(buf.len(), HEADER_LEN + 4 + TAG_LEN);
    }

    #[test]
    fn encode_appends_to_existing_buffer() {
        let prefix: &[u8] = b"sentinel-";
        let frame = frame_with_payload(0);
        let mut buf: Vec<u8> = prefix.to_vec();
        frame.encode(&mut buf).expect("encode");
        assert_eq!(&buf[..prefix.len()], prefix);
        assert_eq!(buf.len(), prefix.len() + MIN_FRAME_LEN);
    }

    #[test]
    fn encode_rejects_oversized_payload_and_leaves_buf_untouched() {
        let frame = frame_with_payload(MAX_PAYLOAD_LEN + 1);
        let mut buf = vec![0xEE, 0xFF];
        let before = buf.clone();
        let err = frame.encode(&mut buf).expect_err("must reject oversize");
        assert_eq!(err, FrameError::BadLength);
        assert_eq!(buf, before);
    }

    #[test]
    fn encode_accepts_max_payload() {
        let frame = frame_with_payload(MAX_PAYLOAD_LEN);
        let mut buf = Vec::new();
        frame.encode(&mut buf).expect("encode at MAX_PAYLOAD_LEN");
        assert_eq!(buf.len(), MAX_FRAME_LEN);
    }

    #[test]
    fn decode_rejects_short_input() {
        let bytes = vec![0u8; MIN_FRAME_LEN - 1];
        let err = Frame::decode(&bytes).expect_err("short input");
        assert_eq!(
            err,
            FrameError::TooShort {
                needed: MIN_FRAME_LEN,
                got: MIN_FRAME_LEN - 1,
            }
        );
    }

    #[test]
    fn decode_rejects_empty_input() {
        let err = Frame::decode(&[]).expect_err("empty input");
        assert_eq!(
            err,
            FrameError::TooShort {
                needed: MIN_FRAME_LEN,
                got: 0,
            }
        );
    }

    #[test]
    fn decode_rejects_wrong_version() {
        let bad_version: u8 = 0x02;
        let mut bytes = vec![0u8; MIN_FRAME_LEN];
        bytes[VERSION_OFFSET] = bad_version;
        let err = Frame::decode(&bytes).expect_err("bad version");
        assert_eq!(err, FrameError::BadVersion(bad_version));
    }

    #[test]
    fn decode_rejects_zero_version() {
        let bytes = vec![0u8; MIN_FRAME_LEN];
        let err = Frame::decode(&bytes).expect_err("zero version");
        assert_eq!(err, FrameError::BadVersion(0));
    }

    #[test]
    fn decode_rejects_oversized_payload() {
        let mut bytes = vec![0u8; MIN_FRAME_LEN + MAX_PAYLOAD_LEN + 1];
        bytes[VERSION_OFFSET] = SYAUTH_WIRE_VERSION_V1;
        let err = Frame::decode(&bytes).expect_err("oversize");
        assert_eq!(err, FrameError::BadLength);
    }

    #[test]
    fn decode_accepts_minimum_frame() {
        let mut bytes = vec![0u8; MIN_FRAME_LEN];
        bytes[VERSION_OFFSET] = SYAUTH_WIRE_VERSION_V1;
        let frame = Frame::decode(&bytes).expect("min frame decodes");
        assert_eq!(frame.version, SYAUTH_WIRE_VERSION_V1);
        assert_eq!(frame.nonce, [0u8; NONCE_LEN]);
        assert!(frame.payload.is_empty());
        assert_eq!(frame.tag, [0u8; TAG_LEN]);
    }

    #[test]
    fn decode_accepts_maximum_frame() {
        let frame = frame_with_payload(MAX_PAYLOAD_LEN);
        let mut buf = Vec::new();
        frame.encode(&mut buf).expect("encode max");
        let parsed = Frame::decode(&buf).expect("decode max");
        assert_eq!(parsed, frame);
    }

    #[test]
    fn roundtrip_specific_example() {
        let frame = Frame {
            version: SYAUTH_WIRE_VERSION_V1,
            nonce: [0x42; NONCE_LEN],
            payload: (0u8..=200).collect(),
            tag: [0x99; TAG_LEN],
        };
        let mut buf = Vec::new();
        frame.encode(&mut buf).expect("encode");
        let parsed = Frame::decode(&buf).expect("decode");
        assert_eq!(parsed, frame);
    }

    proptest! {
        #[test]
        fn roundtrip_property(
            nonce in proptest::array::uniform16(any::<u8>()),
            payload in proptest::collection::vec(any::<u8>(), 0..=MAX_PAYLOAD_LEN),
            tag in proptest::array::uniform16(any::<u8>()),
        ) {
            let frame = Frame {
                version: SYAUTH_WIRE_VERSION_V1,
                nonce,
                payload,
                tag,
            };
            let mut buf = Vec::new();
            frame.encode(&mut buf).expect("encode well-formed frame");
            let parsed = Frame::decode(&buf).expect("decode well-formed frame");
            prop_assert_eq!(parsed, frame);
        }

        #[test]
        fn decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..MAX_FRAME_LEN + 32)) {
            let _ = Frame::decode(&bytes);
        }

        #[test]
        fn decode_rejects_any_nonzero_version(
            version in (2u8..=u8::MAX),
            payload_len in 0usize..=64,
        ) {
            let mut bytes = vec![0u8; MIN_FRAME_LEN + payload_len];
            bytes[VERSION_OFFSET] = version;
            let err = Frame::decode(&bytes).expect_err("bad version");
            prop_assert_eq!(err, FrameError::BadVersion(version));
        }
    }
}
