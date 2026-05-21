// Journey: specs/journeys/JOURNEY-S-004-session-uuid-rotation.md
// Roadmap row: specs/unlock-proximity/ROADMAP.md Step S-004.
//
// Integration tests for the per-minute session-UUID rotation:
//   TC-03 — rotates_at_minute_boundary: under paused tokio time,
//           advance through three simulated minutes and assert
//           exactly four `set_session_uuids` calls (one on
//           construction + one per tick) with `session_uuid_for`
//           output.
//   TC-04 — syslog_emits_rotation_line: install an in-test
//           recorder layer and assert the SPEC §3 #22 audit-line
//           shape (`rotated id=...`, `minute=...`, `uuid=...`)
//           on the `ROTATION_LOG_TARGET` target.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};

use syauth_core::{Bond, BondStatus, bond::PUBKEY_LEN, peer_id_from_pubkey};
use syauth_presenced::{Orchestrator, ROTATION_LOG_TARGET, SECONDS_PER_MINUTE, SHORT_UUID_HEX_LEN};
use syauth_transport::{BOND_KEY_BYTES, FakePeripheral, Peripheral, session_uuid_for};
use time::OffsetDateTime;
use tokio::sync::oneshot;
use tracing::{
    Event, Metadata, Subscriber,
    field::{Field, Visit},
    subscriber::Interest,
};
use tracing_subscriber::{Layer, Registry, layer::SubscriberExt as _};
use uuid::Uuid;

/// Deterministic bond_key fixture so the rotation tests do not depend
/// on a real LESC handshake.
const BOND_KEY: [u8; BOND_KEY_BYTES] = [0xAA; BOND_KEY_BYTES];

/// Deterministic pubkey fixture so the `Bond::peer_id` field has a
/// stable value the audit-line assertion can pattern-match against.
const PUBKEY: [u8; PUBKEY_LEN] = [0xCD; PUBKEY_LEN];

/// Number of simulated minute ticks the rotation test advances
/// through, on top of the construction-time publish.
const TICKS: u64 = 3;

/// Build a `Bond` fixture without going through the pair flow.
fn fixture_bond() -> Bond {
    Bond {
        peer_id: peer_id_from_pubkey(&PUBKEY),
        pubkey: PUBKEY,
        name: "test-peer".to_owned(),
        created_at: OffsetDateTime::UNIX_EPOCH,
        status: BondStatus::Bonded,
    }
}

/// One-time install of a permissive global tracing dispatcher so
/// the callsite cache is seeded with `Interest::sometimes()` before
/// any test runs. Without this, a parallel test that fires
/// `tracing::info!` against `NoSubscriber` may poison the global
/// callsite cache with `Interest::never()`, preventing later tests
/// that install a recorder via `set_default` from capturing the
/// callsite.
fn install_permissive_global_dispatcher() {
    use std::sync::OnceLock;
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let subscriber = Registry::default().with(TestRecorder::default());
        // `set_global_default` returns `Err` if a global was already
        // installed (e.g. by the lifecycle_smoke harness); that is
        // fine — the other dispatcher is also permissive enough.
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

/// TC-03 verbatim from the roadmap row S-004:
/// under paused tokio time, advance three simulated minutes and
/// assert exactly one `set_session_uuids` call per simulated minute
/// (plus the construction-time publish), each carrying
/// `session_uuid_for(bond_key, minute)`.
#[tokio::test(start_paused = true)]
async fn rotates_at_minute_boundary() {
    install_permissive_global_dispatcher();
    let fake: Arc<FakePeripheral> = FakePeripheral::new();
    let peripheral: Arc<dyn Peripheral + Send + Sync> = fake.clone();
    let bond = fixture_bond();
    // Under `start_paused`, `Instant::now()` returns the test's
    // virtual clock origin; the first tick fires when the virtual
    // clock advances past `start`.
    let start = tokio::time::Instant::now() + Duration::from_secs(SECONDS_PER_MINUTE);
    let orchestrator = Arc::new(Orchestrator::new(peripheral, bond, BOND_KEY, start));
    let (tx, rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(orchestrator.run(rx));

    // Yield once so the spawned task hits its first `set_session_uuids`
    // call (the construction-time publish) before we start advancing.
    tokio::task::yield_now().await;
    for _ in 0..TICKS {
        tokio::time::advance(Duration::from_secs(SECONDS_PER_MINUTE)).await;
        tokio::task::yield_now().await;
    }

    let _ = tx.send(());
    let _ = handle.await;

    let calls = fake.session_uuid_calls();
    // 1 construction-time publish + TICKS minute ticks.
    let expected_count: usize = 1 + usize::try_from(TICKS).expect("TICKS fits in usize");
    assert_eq!(
        calls.len(),
        expected_count,
        "expected {expected_count} set_session_uuids calls (1 construction + {TICKS} ticks), got {}",
        calls.len()
    );
    // Each recorded UUID set is a 2-element set: the bonded peer's
    // `session_uuid_for(bond_key, minute)` PLUS the always-present
    // pair-mode UUID derived from the zero bond key for the same
    // minute. Deriving the exact minute index from the test's
    // virtual clock requires reading `SystemTime::now()`, which the
    // paused tokio clock does not control, so we assert the values
    // belong to the expected sequence around the wall-clock instant
    // the test ran.
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_secs();
    let now_minute = i64::try_from(now_secs / SECONDS_PER_MINUTE).expect("minute fits in i64");
    let zero_bond = [0u8; syauth_transport::BOND_KEY_BYTES];
    let allowed: HashSet<Uuid> = (now_minute - 2..=now_minute + 2)
        .flat_map(|m| {
            [
                Uuid::from_bytes(session_uuid_for(&BOND_KEY, m)),
                Uuid::from_bytes(session_uuid_for(&zero_bond, m)),
            ]
        })
        .collect();
    for (i, recorded) in calls.iter().enumerate() {
        assert_eq!(recorded.len(), 2, "call #{i} should be a 2-element UUID set (peer + pair), got {recorded:?}");
        for uuid in recorded {
            assert!(
                allowed.contains(uuid),
                "call #{i} UUID {uuid} is not in the expected session_uuid_for window"
            );
        }
    }
}

/// TC-04 verbatim from the roadmap row S-004: the rotation audit
/// line shape is the SPEC §3 #22 contract
/// (`rotated id=<peer> minute=<N> uuid=<short>`). The test installs
/// a tiny in-test recorder layer (less than 30 LOC) so the
/// assertion does not depend on stdout / stderr capture. The
/// orchestrator future is driven IN-LINE (no `tokio::spawn`) so the
/// recorder's thread-local default subscriber is in scope for every
/// emitted event.
#[tokio::test(start_paused = true)]
async fn syslog_emits_rotation_line() {
    install_permissive_global_dispatcher();
    let recorder = TestRecorder::default();
    let subscriber = Registry::default().with(recorder.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let fake: Arc<FakePeripheral> = FakePeripheral::new();
    let peripheral: Arc<dyn Peripheral + Send + Sync> = fake.clone();
    let bond = fixture_bond();
    let start = tokio::time::Instant::now() + Duration::from_secs(SECONDS_PER_MINUTE);
    let orchestrator = Arc::new(Orchestrator::new(peripheral, bond.clone(), BOND_KEY, start));
    let (tx, rx) = oneshot::channel::<()>();
    let mut run_fut = Box::pin(orchestrator.run(rx));

    // Drive the orchestrator's future cooperatively — one minute
    // tick, then shutdown — without `tokio::spawn`, so the
    // recorder's thread-local default subscriber is the active
    // subscriber for every `tracing::info!` the orchestrator emits.
    tokio::select! {
        _ = &mut run_fut => {},
        _ = tokio::time::sleep(Duration::from_millis(1)) => {},
    }
    tokio::time::advance(Duration::from_secs(SECONDS_PER_MINUTE)).await;
    tokio::select! {
        _ = &mut run_fut => {},
        _ = tokio::time::sleep(Duration::from_millis(1)) => {},
    }
    let _ = tx.send(());
    let _ = run_fut.await;

    let events = recorder.snapshot();
    let matches: Vec<&RecordedEvent> = events
        .iter()
        .filter(|e| e.target == ROTATION_LOG_TARGET && e.message.starts_with("rotated id="))
        .collect();
    let fake_calls = fake.session_uuid_calls();
    assert!(
        !matches.is_empty(),
        "expected at least one rotation audit line on target {ROTATION_LOG_TARGET}, got events: {events:?}; fake had {} set_session_uuid calls",
        fake_calls.len()
    );
    let line = &matches[0].message;
    let peer_segment = format!("id={}", bond.peer_id);
    assert!(line.contains(&peer_segment), "audit line missing peer segment: {line}");
    assert!(line.contains("minute="), "audit line missing minute= segment: {line}");
    assert!(line.contains("uuid="), "audit line missing uuid= segment: {line}");
    // The short-hex render is exactly SHORT_UUID_HEX_LEN chars after
    // the literal `uuid=`.
    let after_uuid = line.split("uuid=").nth(1).expect("uuid= segment present");
    assert!(
        after_uuid.len() >= SHORT_UUID_HEX_LEN,
        "audit line short-uuid too short: {after_uuid:?}"
    );
}

/// One recorded `tracing` event with its target + rendered message.
#[derive(Debug, Clone)]
struct RecordedEvent {
    target: String,
    message: String,
}

/// In-test recorder layer that captures every `tracing::Event` into
/// a shared `Vec<RecordedEvent>`. Scoped to one test via
/// `tracing::subscriber::set_default`.
#[derive(Clone, Default)]
struct TestRecorder {
    events: Arc<Mutex<Vec<RecordedEvent>>>,
}

impl TestRecorder {
    fn snapshot(&self) -> Vec<RecordedEvent> {
        match self.events.lock() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl<S: Subscriber> Layer<S> for TestRecorder {
    fn register_callsite(&self, _metadata: &'static Metadata<'static>) -> Interest {
        // Force re-evaluation per event so a parallel test that ran
        // first under `NoSubscriber` does not pollute the global
        // callsite cache with `Interest::never()`.
        Interest::sometimes()
    }

    fn enabled(&self, _metadata: &Metadata<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) -> bool {
        true
    }

    fn on_event(&self, event: &Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let target = event.metadata().target().to_owned();
        let rec = RecordedEvent {
            target,
            message: visitor.message,
        };
        if let Ok(mut g) = self.events.lock() {
            g.push(rec);
        }
    }
}

/// Visitor that extracts the rendered `message` field from a
/// `tracing::Event`.
#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
            if self.message.starts_with('"') && self.message.ends_with('"') && self.message.len() >= 2 {
                self.message = self.message[1..self.message.len() - 1].to_owned();
            }
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_owned();
        }
    }
}
