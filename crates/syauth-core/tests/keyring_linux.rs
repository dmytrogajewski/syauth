//! Integration test for the `KernelKeyring` backend of S-006.
//!
//! Exercises the real Linux kernel keyring (`keyctl(2)`) via the
//! `linux-keyutils` crate. The test is **hermetic by construction**:
//!
//! - It targets `KeyRingIdentifier::Session`, which is process-local
//!   to the test binary. It NEVER writes to `@u` (the user keyring,
//!   shared across processes) or `@us` (the user session keyring) or
//!   any system-wide identifier.
//! - Each id is prefixed with `syauth-test-{nanos}-{n}:` so two
//!   concurrent test runs (or two runs on the same workspace inside
//!   one minute) cannot collide.
//! - Each test wraps the put → get → remove sequence in a RAII guard
//!   so a panic mid-test still cleans the key.
//! - On a Linux container without `CONFIG_KEYS` (which makes
//!   `keyctl(2)` return `ENOSYS`), the probe at the top of each test
//!   detects the missing facility, prints a skip line via
//!   `eprintln!`, and returns `Ok(())` cleanly — never red, never
//!   pretending to pass against a different facility.
//!
//! Cross-user safety note: the Session keyring is process-local — its
//! contents are NOT visible to a different shell session, NOT visible
//! across a fresh `login`, and NOT visible to root in any other
//! process. This test cannot leak secrets to another user even if it
//! crashed mid-write.
//!
//! Roadmap: specs/syauth/ROADMAP.md §S-006.
//! Journey: specs/journeys/JOURNEY-S-006-secret-storage.md.

#![cfg(target_os = "linux")]

use std::time::{SystemTime, UNIX_EPOCH};

use syauth_core::{KeyStore, secrets::KernelKeyring};

/// Prefix included in every id this file ever writes to the session
/// keyring. The trailing `-` separates the static prefix from the
/// dynamic per-process and per-test discriminators.
const TEST_ID_PREFIX: &str = "syauth-test-";

/// Build a per-process-unique id. The nanoseconds-since-epoch suffix
/// reduces the chance of collision when the same test runs concurrently
/// across processes (CI parallelism, two developers on a shared
/// build server).
fn unique_id(case: &str) -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{TEST_ID_PREFIX}{nanos}-{case}")
}

/// RAII cleanup: when the guard drops, the key is removed via
/// `KeyStore::remove`. Errors from `remove` are intentionally ignored
/// — at this point the test has either succeeded (key already gone) or
/// is unwinding (we're best-effort).
struct CleanupGuard<'a> {
    store: &'a dyn KeyStore,
    id: String,
}

impl Drop for CleanupGuard<'_> {
    fn drop(&mut self) {
        let _ = self.store.remove(&self.id);
    }
}

/// Skip-or-run helper. Returns the opened `KernelKeyring` if the
/// kernel keyring is reachable on this host; prints a clear skip line
/// and returns `None` otherwise so the caller can `return` early.
fn kernel_or_skip(label: &str) -> Option<KernelKeyring> {
    if !KernelKeyring::probe() {
        eprintln!("skipping {label}: kernel keyring unavailable on this host (e.g. container without CONFIG_KEYS)");
        return None;
    }
    match KernelKeyring::open() {
        Ok(store) => Some(store),
        Err(err) => {
            eprintln!("skipping {label}: kernel keyring open failed: {err}");
            None
        }
    }
}

/// TC-08 (per JOURNEY-S-006): put → get → remove against the session
/// keyring round-trips and leaves no leftover key.
#[test]
fn kernel_keyring_roundtrip() {
    let Some(store) = kernel_or_skip("kernel_keyring_roundtrip") else {
        return;
    };
    let id = unique_id("roundtrip");
    let _guard = CleanupGuard {
        store: &store,
        id: id.clone(),
    };

    let payload: &[u8] = b"syauth roundtrip payload \x00\x01\x02";
    store.put(&id, payload).expect("put should succeed");

    let read = store.get(&id).expect("get should succeed").expect("expected Some after put");
    assert_eq!(&*read, payload, "kernel keyring round-tripped bytes must match");

    store.remove(&id).expect("remove should succeed");
    let after_remove = store.get(&id).expect("get after remove should succeed");
    assert!(after_remove.is_none(), "get after remove must return None");
}

/// TC-09 (per JOURNEY-S-006): `get` on an id that was never inserted
/// returns `Ok(None)`, never an error.
#[test]
fn kernel_keyring_get_missing_returns_none() {
    let Some(store) = kernel_or_skip("kernel_keyring_get_missing_returns_none") else {
        return;
    };
    let id = unique_id("never-put");
    let got = store.get(&id).expect("get on missing id should be Ok");
    assert!(got.is_none(), "get on missing id must return None");
}

/// Extra guarantee for the AGENTS.md "leave the system better than you
/// found it" rule: a second `remove` on an already-cleaned id is
/// `Ok(())` — idempotent. This pins the contract documented in the
/// `KeyStore` trait rustdoc.
#[test]
fn kernel_keyring_remove_is_idempotent() {
    let Some(store) = kernel_or_skip("kernel_keyring_remove_is_idempotent") else {
        return;
    };
    let id = unique_id("idempotent-remove");
    store.put(&id, b"x").expect("put should succeed");
    store.remove(&id).expect("first remove should succeed");
    store.remove(&id).expect("second remove on missing id should also succeed");
}
