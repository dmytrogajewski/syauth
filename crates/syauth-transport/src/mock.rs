//! In-process mock implementation of [`crate::BtPeer`] / [`crate::Session`].
//!
//! Drives the six SPEC §4.3 → [`MockScenario`] mappings documented in
//! `specs/journeys/JOURNEY-S-007-transport-trait.md`:
//!
//! | Variant                    | Behavior                                                                                                                       |
//! |----------------------------|-------------------------------------------------------------------------------------------------------------------------------|
//! | [`MockScenario::Golden`]   | `recv_frame` returns the most recently sent frame with its payload XORed by [`GOLDEN_PAYLOAD_XOR_MASK`].                       |
//! | [`MockScenario::Offline`]  | [`crate::BtPeer::connect`] returns [`crate::TransportError::Unreachable`] immediately.                                         |
//! | [`MockScenario::Slow`]     | `recv_frame` waits `delay` before reading; the caller's `timeout` must trip first.                                             |
//! | [`MockScenario::Reordered`]| The first two sent frames come back in reverse order.                                                                          |
//! | [`MockScenario::Replay`]   | A sent frame is delivered `duplicate_count + 1` times by successive `recv_frame` calls.                                        |
//! | [`MockScenario::WrongVersion`] | The first byte of the sent frame is mutated to `injected_version` before being echoed back.                              |
//!
//! All scheduling uses `tokio::time::sleep` — never `std::thread::sleep` — so
//! `tokio::time::pause()` can drive deterministic tests in future. All
//! parameters are module-level `const`s rather than literals (per
//! `AGENTS.md` micro-TDD rules).

use std::{collections::VecDeque, sync::Arc, time::Duration};

use async_trait::async_trait;
use syauth_core::{Frame, MIN_FRAME_LEN, SYAUTH_WIRE_VERSION_V1, VERSION_OFFSET};
use tokio::{
    sync::Mutex,
    time::{sleep, timeout},
};

use crate::{BtPeer, Session, error::TransportError};

// ---------------------------------------------------------------------------
// Named constants — every magic number a test would otherwise hand-type.
// ---------------------------------------------------------------------------

/// Capacity of the internal `tokio::sync::mpsc`-shaped buffer used by
/// [`MockBtPeer`]. Bounded so the mock never grows an unbounded queue if a
/// test forgets to drain it.
pub const MOCK_CHAN_CAP: usize = 16;

/// Default delay used by [`MockScenario::Slow`] when the caller does not
/// supply one. Tuned to be comfortably longer than [`SHORT_CALLER_TIMEOUT`]
/// times [`TIMEOUT_BUDGET_MULT`] so the timeout-vs-delay distinction is
/// observable on noisy CI runners.
pub const SLOW_DEFAULT_DELAY: Duration = Duration::from_millis(200);

/// Wire-format version byte injected by [`MockScenario::WrongVersion`] when
/// the caller does not supply one. Chosen as `0x02` so the failure mode is
/// "future version", which is what the production stack will most likely see
/// during a phased rollout.
pub const WRONG_VERSION_DEFAULT: u8 = 0x02;

/// Default number of *extra* duplicate frames emitted by
/// [`MockScenario::Replay`] when the caller does not supply a count.
/// `1` means the upper layer observes the frame twice in total.
pub const REPLAY_DEFAULT_DUPLICATES: u8 = 1;

/// XOR mask applied to the request payload by [`MockScenario::Golden`] before
/// echoing it back. Non-zero so a test asserting on the response payload kills
/// the "echo without transformation" mutant.
pub const GOLDEN_PAYLOAD_XOR_MASK: u8 = 0x5A;

/// Maximum number of in-flight requests that [`MockScenario::Reordered`] may
/// buffer before emitting them in swapped order. The test matrix only needs
/// two, but we leave a small headroom in case S-009 adds a three-frame
/// scenario.
pub const REORDERED_BUFFER_DEPTH: usize = 4;

/// Caller-side `recv_frame` timeout used by the golden roundtrip test.
pub const GOLDEN_RECV_TIMEOUT: Duration = Duration::from_millis(100);

/// Wall-clock budget for one full `MockScenario::Golden` roundtrip
/// (connect + send + recv). 100 ms is two orders of magnitude above the
/// in-process channel latency observed locally; well below the SPEC §4.2
/// 2-second unlock-path budget.
pub const GOLDEN_ROUNDTRIP_BUDGET: Duration = Duration::from_millis(100);

/// Short caller-side timeout used by the negative-path tests. Picked so it is
/// strictly less than [`SLOW_DEFAULT_DELAY`] divided by
/// [`TIMEOUT_BUDGET_MULT`], so a heavily loaded CI runner still observes the
/// caller's timeout firing before the mock's delay would have.
pub const SHORT_CALLER_TIMEOUT: Duration = Duration::from_millis(30);

/// Multiplier applied to a caller-side timeout to derive the wall-clock
/// upper bound the `Slow` test asserts against. `6` leaves headroom for
/// scheduler jitter while still being strictly less than
/// `SLOW_DEFAULT_DELAY / SHORT_CALLER_TIMEOUT` (= 200/30 ≈ 6.67), so a passing
/// test really does prove the caller's timeout fired.
pub const TIMEOUT_BUDGET_MULT: u32 = 6;

/// Alias for the slow delay used in the negative-path test, kept as a separate
/// `const` so the public test surface can be re-exported without leaking the
/// `SLOW_DEFAULT_DELAY` name to consumers who only want the test budget.
pub const MOCK_SLOW_DELAY: Duration = SLOW_DEFAULT_DELAY;

// ---------------------------------------------------------------------------
// MockScenario — public, per-test configuration.
// ---------------------------------------------------------------------------

/// Per-test behavior knob for [`MockBtPeer`].
///
/// Each variant maps to a row of the SPEC §4.3 e2e matrix; see
/// `specs/journeys/JOURNEY-S-007-transport-trait.md` for the full mapping.
#[derive(Debug, Clone)]
pub enum MockScenario {
    /// Happy path: `recv_frame` returns the most recently sent frame with its
    /// payload XORed by [`GOLDEN_PAYLOAD_XOR_MASK`]. Roundtrip completes in
    /// well under [`GOLDEN_ROUNDTRIP_BUDGET`].
    Golden,

    /// Peer is off the air: [`BtPeer::connect`] returns
    /// [`TransportError::Unreachable`] immediately.
    Offline,

    /// Peer responds, but only after `delay`. The caller's `recv_frame`
    /// `timeout` is expected to fire first, producing
    /// [`TransportError::Timeout`].
    Slow {
        /// Delay applied inside `recv_frame` before reading from the buffer.
        delay: Duration,
    },

    /// Peer returns the first two sent frames in reverse order.
    Reordered,

    /// Peer delivers the same frame `duplicate_count + 1` times in successive
    /// `recv_frame` calls.
    Replay {
        /// Number of *extra* duplicate emissions. `1` means the upper layer
        /// observes the frame twice in total.
        duplicate_count: u8,
    },

    /// Peer mutates the first byte (the wire-format version) of every echoed
    /// frame to `injected_version`. Triggers
    /// [`TransportError::BadFrame`] with the underlying
    /// `FrameError::BadVersion` at `recv_frame` time.
    WrongVersion {
        /// Version byte to inject in place of the v1 marker.
        injected_version: u8,
    },
}

// ---------------------------------------------------------------------------
// MockBtPeer — the trait implementation.
// ---------------------------------------------------------------------------

/// In-process [`BtPeer`] for tests.
///
/// Construct via [`MockBtPeer::expect`]; that is the only public constructor,
/// so a test cannot accidentally bypass the scenario configuration.
pub struct MockBtPeer {
    scenario: MockScenario,
}

impl MockBtPeer {
    /// Build a mock peer that will exhibit `scenario` behavior on every
    /// session opened against it.
    #[must_use]
    pub fn expect(scenario: MockScenario) -> Self {
        Self { scenario }
    }
}

#[async_trait]
impl BtPeer for MockBtPeer {
    async fn connect(&self, _timeout: Duration) -> Result<Box<dyn Session>, TransportError> {
        match &self.scenario {
            MockScenario::Offline => Err(TransportError::Unreachable),
            other => Ok(Box::new(MockSession::new(other.clone()))),
        }
    }
}

// ---------------------------------------------------------------------------
// MockSession — internal Session impl.
// ---------------------------------------------------------------------------

/// Internal per-connection state for [`MockBtPeer`].
///
/// `inbox` holds frames that `recv_frame` will return next. Frames are pushed
/// at `send_frame` time, transformed according to the scenario, and popped at
/// `recv_frame` time. Wrapping in `Arc<Mutex<…>>` is necessary because
/// `send_frame` and `recv_frame` are `async fn`s with `&mut self`, and the
/// scenarios that retain past frames (`Replay`, `Reordered`) need shared
/// state across method calls.
struct MockSession {
    scenario: MockScenario,
    inbox: Arc<Mutex<VecDeque<Frame>>>,
    /// `Reordered` requires us to hold the first sent frame until the second
    /// arrives, then enqueue them in swap order. This counter remembers how
    /// many frames have been sent in this session so far.
    sent_count: Arc<Mutex<u32>>,
    /// `Reordered` pending-buffer for the first frame.
    reorder_pending: Arc<Mutex<Option<Frame>>>,
}

impl MockSession {
    fn new(scenario: MockScenario) -> Self {
        Self {
            scenario,
            inbox: Arc::new(Mutex::new(VecDeque::with_capacity(MOCK_CHAN_CAP))),
            sent_count: Arc::new(Mutex::new(0)),
            reorder_pending: Arc::new(Mutex::new(None)),
        }
    }
}

/// Return a copy of `frame` with its payload XORed against `mask`.
fn xor_payload(frame: &Frame, mask: u8) -> Frame {
    let payload = frame.payload.iter().map(|b| b ^ mask).collect();
    Frame {
        version: frame.version,
        nonce: frame.nonce,
        payload,
        tag: frame.tag,
    }
}

#[async_trait]
impl Session for MockSession {
    async fn send_frame(&mut self, frame: &Frame) -> Result<(), TransportError> {
        match &self.scenario {
            MockScenario::Golden | MockScenario::Slow { .. } | MockScenario::WrongVersion { .. } => {
                let mut inbox = self.inbox.lock().await;
                if inbox.len() >= MOCK_CHAN_CAP {
                    return Err(TransportError::Closed);
                }
                inbox.push_back(xor_payload(frame, GOLDEN_PAYLOAD_XOR_MASK));
                Ok(())
            }
            MockScenario::Replay { duplicate_count } => {
                let mut inbox = self.inbox.lock().await;
                let total: usize = usize::from(*duplicate_count).saturating_add(1);
                for _ in 0..total {
                    if inbox.len() >= MOCK_CHAN_CAP {
                        return Err(TransportError::Closed);
                    }
                    inbox.push_back(frame.clone());
                }
                Ok(())
            }
            MockScenario::Reordered => {
                let mut sent_count = self.sent_count.lock().await;
                *sent_count = sent_count.saturating_add(1);
                let current = *sent_count;
                drop(sent_count);
                let mut pending = self.reorder_pending.lock().await;
                if current == 1 {
                    *pending = Some(frame.clone());
                    Ok(())
                } else {
                    let buffered = pending.take();
                    drop(pending);
                    let mut inbox = self.inbox.lock().await;
                    inbox.push_back(frame.clone());
                    if let Some(first) = buffered {
                        if inbox.len() >= REORDERED_BUFFER_DEPTH {
                            return Err(TransportError::Closed);
                        }
                        inbox.push_back(first);
                    }
                    Ok(())
                }
            }
            MockScenario::Offline => Err(TransportError::Unreachable),
        }
    }

    async fn recv_frame(&mut self, caller_timeout: Duration) -> Result<Frame, TransportError> {
        let read = self.scenario_read();
        match timeout(caller_timeout, read).await {
            Ok(result) => result,
            Err(_) => Err(TransportError::Timeout),
        }
    }
}

impl MockSession {
    /// Build the future that produces the next inbound frame for this
    /// scenario. Wrapped in [`tokio::time::timeout`] by `recv_frame`.
    async fn scenario_read(&self) -> Result<Frame, TransportError> {
        if let MockScenario::Slow { delay } = &self.scenario {
            sleep(*delay).await;
        }
        let mut inbox = self.inbox.lock().await;
        let Some(mut frame) = inbox.pop_front() else {
            // No data buffered yet — return Timeout immediately. The outer
            // `tokio::time::timeout` would do the same thing, but doing it
            // here keeps the trait's contract honest even if a caller passes
            // `Duration::ZERO`.
            return Err(TransportError::Timeout);
        };
        if let MockScenario::WrongVersion { injected_version } = &self.scenario
            && frame.payload.len() <= syauth_core::MAX_PAYLOAD_LEN
        {
            // Mutate the first byte of the wire-format frame by re-encoding,
            // patching, then re-decoding — that exercises the real
            // `Frame::decode` path and produces the canonical
            // `FrameError::BadVersion`. We do this through the encoder so the
            // mutation behaves identically to a real on-the-wire flip.
            let mut buf = Vec::with_capacity(MIN_FRAME_LEN + frame.payload.len());
            frame.version = SYAUTH_WIRE_VERSION_V1;
            frame.encode(&mut buf)?;
            buf[VERSION_OFFSET] = *injected_version;
            let decoded = Frame::decode(&buf);
            return decoded.map_err(TransportError::from);
        }
        Ok(frame)
    }
}

// ---------------------------------------------------------------------------
// Tests — one per MockScenario variant (DoD requirement).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // Journey: specs/journeys/JOURNEY-S-007-transport-trait.md

    use std::time::Instant as StdInstant;

    use syauth_core::{Frame, FrameError, NONCE_LEN, SYAUTH_WIRE_VERSION_V1, TAG_LEN};

    use super::*;
    use crate::{BtPeer, TransportError};

    /// Build a well-formed v1 frame with the given payload.
    fn frame_with_payload(payload: Vec<u8>) -> Frame {
        Frame {
            version: SYAUTH_WIRE_VERSION_V1,
            nonce: [0x11; NONCE_LEN],
            payload,
            tag: [0x33; TAG_LEN],
        }
    }

    #[tokio::test]
    async fn golden_roundtrip_decodes_xor_echo_within_budget() {
        let peer = MockBtPeer::expect(MockScenario::Golden);
        let started = StdInstant::now();
        let mut session = peer.connect(GOLDEN_RECV_TIMEOUT).await.expect("connect");
        let request = frame_with_payload(vec![0xAA, 0xBB, 0xCC, 0xDD]);
        session.send_frame(&request).await.expect("send");
        let response = session.recv_frame(GOLDEN_RECV_TIMEOUT).await.expect("recv");
        let elapsed = started.elapsed();
        assert_eq!(response.nonce, request.nonce);
        assert_eq!(response.tag, request.tag);
        let want_payload: Vec<u8> = request.payload.iter().map(|b| b ^ GOLDEN_PAYLOAD_XOR_MASK).collect();
        assert_eq!(response.payload, want_payload);
        assert!(
            elapsed < GOLDEN_ROUNDTRIP_BUDGET,
            "golden roundtrip exceeded budget: {:?} >= {:?}",
            elapsed,
            GOLDEN_ROUNDTRIP_BUDGET
        );
    }

    #[tokio::test]
    async fn offline_scenario_connect_returns_unreachable() {
        let peer = MockBtPeer::expect(MockScenario::Offline);
        match peer.connect(GOLDEN_RECV_TIMEOUT).await {
            Err(err) => assert_eq!(err, TransportError::Unreachable),
            Ok(_) => panic!("offline must fail connect"),
        }
    }

    #[tokio::test]
    async fn slow_scenario_recv_times_out_before_delay_elapses() {
        let peer = MockBtPeer::expect(MockScenario::Slow { delay: MOCK_SLOW_DELAY });
        let mut session = peer.connect(GOLDEN_RECV_TIMEOUT).await.expect("connect");
        let request = frame_with_payload(vec![0x01, 0x02]);
        session.send_frame(&request).await.expect("send");
        let started = StdInstant::now();
        let err = session.recv_frame(SHORT_CALLER_TIMEOUT).await.expect_err("slow must time out");
        let elapsed = started.elapsed();
        assert_eq!(err, TransportError::Timeout);
        let upper_bound = SHORT_CALLER_TIMEOUT * TIMEOUT_BUDGET_MULT;
        assert!(
            elapsed < upper_bound,
            "elapsed {:?} >= upper_bound {:?} (caller timeout did not fire)",
            elapsed,
            upper_bound
        );
        // Sanity: the upper bound must be strictly less than the mock's delay,
        // otherwise this test cannot distinguish caller-timeout from
        // delay-completed.
        assert!(upper_bound < MOCK_SLOW_DELAY, "test misconfigured: upper_bound >= MOCK_SLOW_DELAY");
    }

    #[tokio::test]
    async fn reordered_scenario_emits_second_frame_first() {
        let peer = MockBtPeer::expect(MockScenario::Reordered);
        let mut session = peer.connect(GOLDEN_RECV_TIMEOUT).await.expect("connect");
        let mut first = frame_with_payload(vec![0x01]);
        first.nonce[0] = 0xAA;
        let mut second = frame_with_payload(vec![0x02]);
        second.nonce[0] = 0xBB;
        session.send_frame(&first).await.expect("send first");
        session.send_frame(&second).await.expect("send second");
        let got_first = session.recv_frame(GOLDEN_RECV_TIMEOUT).await.expect("recv 1");
        let got_second = session.recv_frame(GOLDEN_RECV_TIMEOUT).await.expect("recv 2");
        assert_eq!(got_first.nonce[0], 0xBB, "first inbound must be the second sent frame");
        assert_eq!(got_second.nonce[0], 0xAA, "second inbound must be the first sent frame");
    }

    #[tokio::test]
    async fn replay_scenario_emits_duplicate_frame() {
        let peer = MockBtPeer::expect(MockScenario::Replay {
            duplicate_count: REPLAY_DEFAULT_DUPLICATES,
        });
        let mut session = peer.connect(GOLDEN_RECV_TIMEOUT).await.expect("connect");
        let request = frame_with_payload(vec![0xDE, 0xAD, 0xBE, 0xEF]);
        session.send_frame(&request).await.expect("send");
        let first = session.recv_frame(GOLDEN_RECV_TIMEOUT).await.expect("recv 1");
        let second = session.recv_frame(GOLDEN_RECV_TIMEOUT).await.expect("recv 2");
        assert_eq!(first, second, "replay must deliver identical frames");
        assert_eq!(first, request, "replay must echo the request verbatim");
    }

    #[tokio::test]
    async fn wrong_version_scenario_returns_bad_frame_with_injected_version() {
        let peer = MockBtPeer::expect(MockScenario::WrongVersion {
            injected_version: WRONG_VERSION_DEFAULT,
        });
        let mut session = peer.connect(GOLDEN_RECV_TIMEOUT).await.expect("connect");
        let request = frame_with_payload(vec![0x99]);
        session.send_frame(&request).await.expect("send");
        let err = session
            .recv_frame(GOLDEN_RECV_TIMEOUT)
            .await
            .expect_err("wrong-version must reject");
        assert_eq!(err, TransportError::BadFrame(FrameError::BadVersion(WRONG_VERSION_DEFAULT)));
    }
}
