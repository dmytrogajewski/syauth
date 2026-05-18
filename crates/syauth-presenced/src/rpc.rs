//! Typed RPC wire format for the PAM ↔ daemon Unix socket.
//!
//! SPEC anchor: `specs/unlock-proximity/SPEC.md` §3 Decisions row
//! "PAM ↔ daemon transport" — `${XDG_RUNTIME_DIR}/syauth/auth.sock`
//! carries length-prefixed CBOR-encoded typed messages. The 4-byte
//! big-endian length prefix matches the existing UniFFI frame style
//! the mobile crate uses.
//!
//! Roadmap row: `specs/unlock-proximity/ROADMAP.md` Step S-002.
//! Journey: `specs/journeys/JOURNEY-S-002-cbor-unix-socket-rpc-stub.md`.
//!
//! S-002 ships the wire format, the encode/decode helpers, and a stub
//! responder. The real challenge state machine arrives in S-006; the
//! `Reload` RPC body is wired in S-005; the `Status` RPC body is
//! wired in S-017. The `kind` discriminator on every variant keeps
//! the byte layout stable as new variants are added.

use std::{
    io::{Read, Write},
    time::SystemTime,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

/// Maximum size in bytes of a single CBOR-encoded RPC frame (excluding
/// the 4-byte length prefix). 64 KiB is three orders of magnitude
/// above any legitimate request the SPEC defines today
/// (`ChallengeRequest` is < 64 bytes after CBOR encoding; a
/// `StatusResponse` for the 100-peer-limit case is < 4 KiB), so any
/// larger frame is a sign of a malformed or hostile client. Anchored
/// in SPEC §7 T-Daemon-DoS.
pub const MAX_FRAME_LEN: usize = 64 * 1024;

/// Width of the big-endian length prefix that precedes every CBOR
/// payload on the wire. Matches the existing UniFFI frame style the
/// mobile crate uses, per SPEC §3 scope item #5.
pub const LENGTH_PREFIX_BYTES: usize = 4;

/// RPC request kinds the PAM module (and `syauth status` in S-017)
/// can send to the daemon. The `kind` field on the wire selects the
/// variant on decode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Request {
    /// PAM-driven challenge. Daemon issues a fresh nonce to the
    /// phone, awaits the signed response, replies to the PAM module
    /// with the verified outcome. S-002 ships only the wire shape;
    /// S-006 wires the real challenge state machine.
    Challenge {
        /// Bond identifier for the peer this challenge targets.
        peer_id: String,
        /// 16-byte nonce the PAM caller wants the phone to sign.
        /// `serde_bytes` keeps the CBOR payload compact (one
        /// byte-string item, not an array of integers).
        #[serde(with = "serde_bytes")]
        nonce: Vec<u8>,
    },
    /// Operator-driven re-load of the bonds.toml + keys directory.
    /// S-005 implements the side effect; S-002 just sends/decodes.
    Reload,
    /// Operator-driven liveness probe. S-017 fills in the per-peer
    /// metrics; S-002 ships the wire shape.
    Status,
}

/// RPC response kinds the daemon sends back on the same connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Response {
    /// Outcome of a `Request::Challenge`. S-002 always emits
    /// `ok=false, signature=None, reason="not-implemented"`; S-006
    /// fills in real signatures.
    Challenge {
        /// `true` only when the phone signed and the daemon
        /// verified.
        ok: bool,
        /// Ed25519 signature over the challenge nonce (present iff
        /// `ok=true`). `None` for the S-002 stub.
        #[serde(with = "serde_bytes")]
        signature: Option<Vec<u8>>,
        /// Short, machine-readable reason — `"not-implemented"`,
        /// `"offline"`, `"denied"`, `"replay"`, etc. Keeps the PAM
        /// module's error mapping table greppable.
        reason: String,
    },
    /// Outcome of `Request::Reload`. S-002 always emits `ok=true`;
    /// S-005 fills in failure paths.
    Reload {
        /// `true` if the reload happened. `false` if the bonds file
        /// could not be read.
        ok: bool,
    },
    /// Outcome of `Request::Status`. S-002 ships the wire shape with
    /// an empty `peers` list; S-017 fills in the per-peer columns.
    Status {
        /// Per-peer liveness rows.
        peers: Vec<PeerStatus>,
        /// Daemon boot wall-clock time (UTC). Serialized as the
        /// CBOR-canonical "seconds since Unix epoch as a 64-bit
        /// unsigned integer" via the `started_at_epoch_seconds`
        /// helper module so the wire format does not embed a
        /// platform-specific representation.
        #[serde(with = "epoch_seconds")]
        started_at: SystemTime,
    },
}

/// Per-peer status row, used inside `Response::Status`. S-002 ships
/// the wire shape; S-017 populates the fields from the orchestrator.
///
/// Field semantics anchor to SPEC §3 scope item #24 ("time since last
/// challenge, time since last connect by each peer"). Granularity is
/// milliseconds so a `syauth status --watch` redraw at 1 Hz can show
/// sub-second freshness on the row that just landed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerStatus {
    /// Bond identifier (matches `Request::Challenge::peer_id`).
    pub peer_id: String,
    /// Milliseconds since the daemon last issued a challenge for
    /// this peer (any outcome). `None` if no challenge has been
    /// attempted since daemon start.
    pub last_challenge_ms_ago: Option<u64>,
    /// Milliseconds since the daemon last acquired this peer's
    /// challenge slot (per-peer `Semaphore(1)` permit grant; the
    /// closest proxy the daemon owns to a "connect" event — the
    /// daemon does not track GATT link-up at this layer). `None`
    /// if the slot has not been acquired since daemon start.
    pub last_connect_ms_ago: Option<u64>,
    /// Current rotating session UUID the daemon advertises for
    /// this peer for the wall-clock minute the snapshot was taken
    /// in. Derived via `session_uuid_for(bond_key, minute_index)`.
    pub current_session_uuid: uuid::Uuid,
    /// Count of in-flight challenges. Bounded to `{0, 1}` by SPEC
    /// §3 scope item #7 (per-peer `Semaphore(1)`).
    pub in_flight_challenges: u32,
}

/// Errors the framing layer can surface to callers. All variants
/// carry enough context for the PAM module's error-mapping table.
#[derive(Debug, Error)]
pub enum FrameError {
    /// Underlying socket / fd I/O failed mid-frame.
    #[error("frame I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// CBOR decoder rejected the payload.
    #[error("CBOR decode failed: {0}")]
    Decode(String),
    /// CBOR encoder failed (effectively impossible for the typed
    /// enums; surfaced as a typed variant for symmetry with `Decode`).
    #[error("CBOR encode failed: {0}")]
    Encode(String),
    /// The length-prefix announced a payload larger than
    /// `MAX_FRAME_LEN`. Almost certainly a malformed or hostile
    /// client.
    #[error("frame too large: {len} bytes > {max} bytes max")]
    TooLarge {
        /// Announced length in bytes.
        len: u32,
        /// Configured cap (`MAX_FRAME_LEN`).
        max: usize,
    },
}

/// Encode `value` to CBOR + prepend a 4-byte big-endian length
/// prefix. The returned buffer is ready for a single
/// `AsyncWriteExt::write_all` call.
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, FrameError> {
    let mut payload = Vec::new();
    ciborium::ser::into_writer(value, &mut payload).map_err(|err| FrameError::Encode(err.to_string()))?;
    if payload.len() > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge {
            len: u32::try_from(payload.len()).unwrap_or(u32::MAX),
            max: MAX_FRAME_LEN,
        });
    }
    // `payload.len()` is `<= MAX_FRAME_LEN` which fits in a `u32`.
    let len_bytes = u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge {
        len: u32::MAX,
        max: MAX_FRAME_LEN,
    })?;
    let mut framed = Vec::with_capacity(LENGTH_PREFIX_BYTES + payload.len());
    framed.extend_from_slice(&len_bytes.to_be_bytes());
    framed.extend_from_slice(&payload);
    Ok(framed)
}

/// Decode a single CBOR frame from `bytes` (the payload only — the
/// caller has already stripped the length prefix).
pub fn decode_frame<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, FrameError> {
    if bytes.len() > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge {
            len: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
            max: MAX_FRAME_LEN,
        });
    }
    ciborium::de::from_reader(bytes).map_err(|err| FrameError::Decode(err.to_string()))
}

/// Read exactly one length-prefixed CBOR frame from `reader` and
/// decode it into `T`. Reads at most `MAX_FRAME_LEN + LENGTH_PREFIX_BYTES`
/// bytes total — no allocation is performed before the length cap is
/// checked.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<T, FrameError>
where
    R: AsyncRead + Unpin,
    T: for<'de> Deserialize<'de>,
{
    let mut len_buf = [0u8; LENGTH_PREFIX_BYTES];
    reader.read_exact(&mut len_buf).await?;
    let announced_len = u32::from_be_bytes(len_buf);
    let len_usize = usize::try_from(announced_len).map_err(|_| FrameError::TooLarge {
        len: announced_len,
        max: MAX_FRAME_LEN,
    })?;
    if len_usize > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge {
            len: announced_len,
            max: MAX_FRAME_LEN,
        });
    }
    let mut payload = vec![0u8; len_usize];
    reader.read_exact(&mut payload).await?;
    decode_frame(&payload)
}

/// Encode `value` and write it (length prefix + CBOR payload) to
/// `writer` in a single buffered call.
pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let framed = encode_frame(value)?;
    writer.write_all(&framed).await?;
    writer.flush().await?;
    Ok(())
}

/// Blocking counterpart of [`read_frame`]. The `pam_syauth` cdylib
/// runs inside a non-async `sudo` process and cannot afford to
/// link a tokio runtime, so the PAM Unix-socket client reads
/// length-prefixed CBOR frames via plain `std::io::Read`. The wire
/// format is identical to the tokio path — daemon (tokio) and PAM
/// (blocking) share the same encoding via [`decode_frame`].
pub fn read_frame_blocking<R, T>(reader: &mut R) -> Result<T, FrameError>
where
    R: Read,
    T: for<'de> Deserialize<'de>,
{
    let mut len_buf = [0u8; LENGTH_PREFIX_BYTES];
    reader.read_exact(&mut len_buf)?;
    let announced_len = u32::from_be_bytes(len_buf);
    let len_usize = usize::try_from(announced_len).map_err(|_| FrameError::TooLarge {
        len: announced_len,
        max: MAX_FRAME_LEN,
    })?;
    if len_usize > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge {
            len: announced_len,
            max: MAX_FRAME_LEN,
        });
    }
    let mut payload = vec![0u8; len_usize];
    reader.read_exact(&mut payload)?;
    decode_frame(&payload)
}

/// Blocking counterpart of [`write_frame`]. See
/// [`read_frame_blocking`] for the rationale.
pub fn write_frame_blocking<W, T>(writer: &mut W, value: &T) -> Result<(), FrameError>
where
    W: Write,
    T: Serialize,
{
    let framed = encode_frame(value)?;
    writer.write_all(&framed)?;
    writer.flush()?;
    Ok(())
}

/// `SystemTime` ↔ epoch-seconds helpers. The CBOR wire format embeds
/// the timestamp as a 64-bit unsigned integer so non-Rust consumers
/// (the mobile crate's UniFFI surface, the future `syauth-presenced-ctl`
/// admin tool) decode it without depending on serde-with's
/// timestamp helpers.
mod epoch_seconds {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Serialize a `SystemTime` as the number of whole seconds since
    /// the Unix epoch. Times before the epoch encode as `0` — the
    /// daemon's startup time is always after the epoch in practice,
    /// so this branch is unreachable on the happy path and exists
    /// only so the function is total.
    pub(super) fn serialize<S: Serializer>(value: &SystemTime, serializer: S) -> Result<S::Ok, S::Error> {
        let secs = value.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_secs();
        secs.serialize(serializer)
    }

    /// Deserialize the epoch-seconds back to a `SystemTime`.
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SystemTime, D::Error> {
        let secs = u64::deserialize(deserializer)?;
        Ok(UNIX_EPOCH + Duration::from_secs(secs))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    const TEST_NONCE: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
    ];

    fn round_trip_request(value: &Request) -> Request {
        let encoded = encode_frame(value).expect("encode succeeds");
        // Strip the 4-byte length prefix to drive `decode_frame`
        // directly (the higher-level `read_frame` over a tokio
        // stream is exercised by the integration smoke test).
        assert!(encoded.len() > LENGTH_PREFIX_BYTES);
        let payload = &encoded[LENGTH_PREFIX_BYTES..];
        decode_frame::<Request>(payload).expect("decode succeeds")
    }

    fn round_trip_response(value: &Response) -> Response {
        let encoded = encode_frame(value).expect("encode succeeds");
        assert!(encoded.len() > LENGTH_PREFIX_BYTES);
        let payload = &encoded[LENGTH_PREFIX_BYTES..];
        decode_frame::<Response>(payload).expect("decode succeeds")
    }

    #[test]
    fn request_challenge_roundtrips() {
        let original = Request::Challenge {
            peer_id: "test-peer".to_string(),
            nonce: TEST_NONCE.to_vec(),
        };
        assert_eq!(round_trip_request(&original), original);
    }

    #[test]
    fn request_reload_roundtrips() {
        let original = Request::Reload;
        assert_eq!(round_trip_request(&original), original);
    }

    #[test]
    fn request_status_roundtrips() {
        let original = Request::Status;
        assert_eq!(round_trip_request(&original), original);
    }

    #[test]
    fn response_challenge_roundtrips_with_no_signature() {
        let original = Response::Challenge {
            ok: false,
            signature: None,
            reason: "not-implemented".to_string(),
        };
        assert_eq!(round_trip_response(&original), original);
    }

    #[test]
    fn response_challenge_roundtrips_with_signature() {
        let original = Response::Challenge {
            ok: true,
            signature: Some(vec![0xaa; 64]),
            reason: "ok".to_string(),
        };
        assert_eq!(round_trip_response(&original), original);
    }

    #[test]
    fn response_reload_roundtrips() {
        let original = Response::Reload { ok: true };
        assert_eq!(round_trip_response(&original), original);
    }

    #[test]
    fn response_status_roundtrips() {
        let original = Response::Status {
            peers: vec![PeerStatus {
                peer_id: "peer-a".to_string(),
                last_challenge_ms_ago: Some(42_000),
                last_connect_ms_ago: None,
                current_session_uuid: uuid::Uuid::from_bytes([0xab; 16]),
                in_flight_challenges: 1,
            }],
            started_at: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        };
        assert_eq!(round_trip_response(&original), original);
    }

    #[test]
    fn frame_carries_length_prefix() {
        let value = Request::Reload;
        let encoded = encode_frame(&value).expect("encode succeeds");
        assert!(encoded.len() >= LENGTH_PREFIX_BYTES);
        let announced = u32::from_be_bytes(encoded[..LENGTH_PREFIX_BYTES].try_into().expect("prefix slice"));
        let payload_len = u32::try_from(encoded.len() - LENGTH_PREFIX_BYTES).expect("payload len fits");
        assert_eq!(announced, payload_len);
    }

    /// The blocking frame helpers (`read_frame_blocking` /
    /// `write_frame_blocking`) MUST produce bytes the async
    /// `read_frame` decoder accepts, and vice versa — the daemon
    /// (tokio) and PAM (blocking) must agree on the wire format.
    #[test]
    fn blocking_helpers_roundtrip_via_in_memory_pipe() {
        let original = Request::Challenge {
            peer_id: "peer-a".to_string(),
            nonce: TEST_NONCE.to_vec(),
        };
        let mut buf: Vec<u8> = Vec::new();
        write_frame_blocking(&mut buf, &original).expect("write_frame_blocking");
        let mut cursor = std::io::Cursor::new(buf);
        let decoded: Request = read_frame_blocking(&mut cursor).expect("read_frame_blocking");
        assert_eq!(decoded, original);
    }

    #[test]
    fn decode_rejects_frame_larger_than_max() {
        let oversized = vec![0u8; MAX_FRAME_LEN + 1];
        match decode_frame::<Request>(&oversized) {
            Err(FrameError::TooLarge { len, max }) => {
                assert_eq!(len as usize, MAX_FRAME_LEN + 1);
                assert_eq!(max, MAX_FRAME_LEN);
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }
}
