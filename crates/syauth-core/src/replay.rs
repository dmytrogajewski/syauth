//! Sliding LRU + TTL nonce cache used by `pam_sm_authenticate` to reject
//! replayed responses inside a single boot session.
//!
//! Background
//! ----------
//!
//! SPEC §4.2 sets the cache's shape: 64 entries, 10 s TTL, in-memory only. SPEC §6
//! T-002 names this cache as the mitigation for response-frame replay attacks.
//! Replay is one of several defenses — the upstream signature + tag checks are
//! authoritative — so the cache is sized to bound memory rather than to provide
//! a permanent blacklist.
//!
//! Why time is injected
//! --------------------
//!
//! The cache deliberately does **not** call `std::time::Instant::now()`. The
//! caller passes `now: Instant` on every `observe` call. This makes the cache
//! deterministic under tests (no `thread::sleep` past TTL) and keeps it
//! reusable from non-wall-clock contexts (replay drivers, fuzzers).
//!
//! Algorithmic shape
//! -----------------
//!
//! Backing store is a `VecDeque<(nonce, inserted_at)>` because (a) the cap is
//! small (64), so O(cap) linear scans are cheap, (b) front-eviction is O(1) on
//! a `VecDeque`, and (c) we avoid adding a third-party LRU crate. The
//! algorithm in `observe`:
//!
//! 1. Pop every front entry whose age `now - inserted_at >= ttl` (TTL sweep).
//! 2. If any surviving entry has a matching nonce, return `Replayed`. The
//!    matched entry's `inserted_at` is **not** refreshed — otherwise an
//!    attacker could keep a captured nonce alive forever by spamming replays.
//! 3. Otherwise push `(nonce, now)` to the back, then evict the front while
//!    `len > cap` (LRU sweep). Return `Fresh`.
//!
//! Degenerate inputs are tolerated: `cap == 0` retains nothing (every
//! observation is `Fresh`), `Duration::ZERO` expires entries instantly.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crate::frame::NONCE_LEN;

/// SPEC §4.2 default replay-cache capacity. Sized to comfortably exceed the
/// expected per-PAM-call response volume (one signature exchange) while
/// bounding memory at a few hundred bytes.
pub const DEFAULT_REPLAY_CAP: usize = 64;

/// SPEC §4.2 default replay-cache TTL. Chosen as 10 s — five-times the 2.0 s
/// unlock deadline (SPEC §4.2 NFR), so a slow BLE retry sequence cannot expire
/// out from under us, but short enough that the cache never holds a meaningful
/// fraction of any one session's worth of nonces.
pub const DEFAULT_REPLAY_TTL: Duration = Duration::from_secs(10);

/// Outcome of a `ReplayCache::observe` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acceptance {
    /// First observation of this nonce inside the current TTL window.
    Fresh,
    /// A duplicate of an earlier observation still inside the TTL window.
    Replayed,
}

/// In-memory sliding LRU + TTL cache of recently-observed response nonces.
///
/// The cache is intended to live for the duration of one `pam_sm_*` call and
/// be dropped with the surrounding tokio runtime. It carries no `'static`
/// state — there is intentionally no global registry.
#[derive(Debug)]
pub struct ReplayCache {
    cap: usize,
    ttl: Duration,
    entries: VecDeque<([u8; NONCE_LEN], Instant)>,
}

impl ReplayCache {
    /// Build an empty cache with the given capacity and TTL.
    ///
    /// `cap == 0` is tolerated; the cache retains nothing in that case and
    /// every `observe` call returns `Acceptance::Fresh`. `ttl == Duration::ZERO`
    /// is tolerated; entries are evicted on the very next call.
    #[must_use]
    pub fn new(cap: usize, ttl: Duration) -> Self {
        Self {
            cap,
            ttl,
            entries: VecDeque::with_capacity(cap),
        }
    }

    /// Record `nonce` as seen at instant `now`. Returns whether the nonce was
    /// fresh (not in the surviving window) or a replay (already cached).
    pub fn observe(&mut self, nonce: [u8; NONCE_LEN], now: Instant) -> Acceptance {
        self.evict_expired(now);
        if self.entries.iter().any(|(n, _)| *n == nonce) {
            return Acceptance::Replayed;
        }
        if self.cap == 0 {
            return Acceptance::Fresh;
        }
        self.entries.push_back((nonce, now));
        while self.entries.len() > self.cap {
            let _ = self.entries.pop_front();
        }
        Acceptance::Fresh
    }

    fn evict_expired(&mut self, now: Instant) {
        while let Some((_, inserted_at)) = self.entries.front() {
            if now.saturating_duration_since(*inserted_at) >= self.ttl {
                let _ = self.entries.pop_front();
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small nudge past TTL used by expiration tests. Named to satisfy the
    /// "no magic literals" rule.
    const DEFAULT_REPLAY_TTL_NUDGE: Duration = Duration::from_millis(1);

    fn nonce(seed: u8) -> [u8; NONCE_LEN] {
        [seed; NONCE_LEN]
    }

    #[test]
    fn fresh_nonce_accepted() {
        let mut cache = ReplayCache::new(DEFAULT_REPLAY_CAP, DEFAULT_REPLAY_TTL);
        let origin = Instant::now();
        assert_eq!(cache.observe(nonce(1), origin), Acceptance::Fresh);
    }

    #[test]
    fn exact_replay_rejected() {
        let mut cache = ReplayCache::new(DEFAULT_REPLAY_CAP, DEFAULT_REPLAY_TTL);
        let origin = Instant::now();
        let half_ttl = DEFAULT_REPLAY_TTL / 2;
        assert_eq!(cache.observe(nonce(7), origin), Acceptance::Fresh);
        assert_eq!(cache.observe(nonce(7), origin + half_ttl), Acceptance::Replayed);
    }

    #[test]
    fn lru_eviction_by_capacity() {
        // Pick a small cap so the test stays readable; the same path that
        // evicts at `cap = 3` also evicts at `cap = DEFAULT_REPLAY_CAP`.
        const SMALL_CAP: usize = 3;
        let mut cache = ReplayCache::new(SMALL_CAP, DEFAULT_REPLAY_TTL);
        let origin = Instant::now();

        // Insert cap + 1 distinct nonces; the very first one should be
        // evicted to make room for the last.
        for i in 0..=SMALL_CAP {
            let when = origin + Duration::from_millis(i as u64);
            assert_eq!(cache.observe(nonce(i as u8), when), Acceptance::Fresh, "fill index {i}");
        }

        // Confirm the cap surviving nonces (1..=SMALL_CAP) report Replayed
        // FIRST — observing them is read-only when they hit, so the cache
        // state stays unchanged.
        let probe_at = origin + Duration::from_millis(SMALL_CAP as u64 + 1);
        for i in 1..=SMALL_CAP {
            assert_eq!(cache.observe(nonce(i as u8), probe_at), Acceptance::Replayed, "kept index {i}");
        }
        // Re-observing the first inserted nonce must report Fresh because it
        // has been LRU-evicted.
        assert_eq!(cache.observe(nonce(0), probe_at), Acceptance::Fresh, "oldest evicted");
    }

    #[test]
    fn ttl_expiration_re_accepts() {
        let mut cache = ReplayCache::new(DEFAULT_REPLAY_CAP, DEFAULT_REPLAY_TTL);
        let origin = Instant::now();
        assert_eq!(cache.observe(nonce(42), origin), Acceptance::Fresh);
        let past_ttl = origin + DEFAULT_REPLAY_TTL + DEFAULT_REPLAY_TTL_NUDGE;
        assert_eq!(cache.observe(nonce(42), past_ttl), Acceptance::Fresh);
    }

    #[test]
    fn interleaved_fresh_and_replay() {
        let mut cache = ReplayCache::new(DEFAULT_REPLAY_CAP, DEFAULT_REPLAY_TTL);
        let origin = Instant::now();
        let step = Duration::from_millis(1);

        let a = nonce(0xAA);
        let b = nonce(0xBB);
        let c = nonce(0xCC);

        // Sequence: A B A C B at strictly increasing instants, all inside TTL.
        assert_eq!(cache.observe(a, origin), Acceptance::Fresh);
        assert_eq!(cache.observe(b, origin + step), Acceptance::Fresh);
        assert_eq!(cache.observe(a, origin + step * 2), Acceptance::Replayed);
        assert_eq!(cache.observe(c, origin + step * 3), Acceptance::Fresh);
        assert_eq!(cache.observe(b, origin + step * 4), Acceptance::Replayed);
    }

    #[test]
    fn cap_zero_accepts_everything_as_fresh() {
        let mut cache = ReplayCache::new(0, DEFAULT_REPLAY_TTL);
        let origin = Instant::now();
        assert_eq!(cache.observe(nonce(9), origin), Acceptance::Fresh);
        assert_eq!(cache.observe(nonce(9), origin), Acceptance::Fresh, "no retention at cap=0");
    }

    #[test]
    fn replay_does_not_refresh_inserted_at() {
        let mut cache = ReplayCache::new(DEFAULT_REPLAY_CAP, DEFAULT_REPLAY_TTL);
        let origin = Instant::now();
        let half_ttl = DEFAULT_REPLAY_TTL / 2;

        // Insert once at origin.
        assert_eq!(cache.observe(nonce(5), origin), Acceptance::Fresh);
        // Replay at origin + ttl/2 — replay must NOT push the deadline forward.
        assert_eq!(cache.observe(nonce(5), origin + half_ttl), Acceptance::Replayed);
        // After origin + ttl + nudge, the entry has aged out from the *original*
        // insertion instant, so re-observation is Fresh.
        let past_ttl = origin + DEFAULT_REPLAY_TTL + DEFAULT_REPLAY_TTL_NUDGE;
        assert_eq!(cache.observe(nonce(5), past_ttl), Acceptance::Fresh);
    }

    #[test]
    fn zero_ttl_expires_entries_instantly() {
        let mut cache = ReplayCache::new(DEFAULT_REPLAY_CAP, Duration::ZERO);
        let origin = Instant::now();
        assert_eq!(cache.observe(nonce(1), origin), Acceptance::Fresh);
        // Even at the same instant, `now - inserted_at == 0 >= ttl == 0` so
        // the entry is expired before the lookup runs.
        assert_eq!(cache.observe(nonce(1), origin), Acceptance::Fresh);
    }
}
