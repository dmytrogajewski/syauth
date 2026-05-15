//! syauth secret storage — kernel keyring with libsecret fallback.
//!
//! Per SPEC §D6 and SPEC §4.4, the host's Ed25519 private key plus every
//! per-peer symmetric bond key live in **secure storage** on Linux. The
//! primary backend is the Linux kernel keyring (`keyctl(2)`, via the
//! safe `linux-keyutils` wrapper); the fallback is the freedesktop
//! Secret Service API (a.k.a. libsecret / gnome-keyring / kwallet), via
//! the `secret-service` crate's blocking submodule.
//!
//! All returned secrets are wrapped in [`zeroize::Zeroizing<Vec<u8>>`]
//! so they are wiped on drop. Backend error messages never include the
//! secret payload — see the audit note at the bottom of this module.
//!
//! # Trait surface
//!
//! ```rust,no_run
//! use syauth_core::{KeyStore, SecretError, detect};
//! # fn main() -> Result<(), SecretError> {
//! let store: Box<dyn KeyStore> = detect()?;
//! store.put("host", b"private-key-bytes")?;
//! if let Some(secret) = store.get("host")? {
//!     // `secret: Zeroizing<Vec<u8>>` — wiped when the binding ends.
//!     assert_eq!(&*secret, b"private-key-bytes");
//! }
//! store.remove("host")?;
//! # Ok(())
//! # }
//! ```
//!
//! # Async-vs-sync
//!
//! The trait is **synchronous** because the PAM C ABI is synchronous
//! (SPEC §2.2). Making `KeyStore` async would force the PAM hot path to
//! enter a tokio runtime around every call; today's secret access
//! happens once per `pam_sm_authenticate` for a tiny key and once per
//! `syauth pair` / `syauth revoke`. We pay a one-time DBus connection
//! per libsecret call in exchange for a clean sync surface. See
//! [`SecretService`] for the runtime-cost detail.
//!
//! # Audit note on error messages
//!
//! [`SecretError::Backend`] wraps the upstream error's `Display` output
//! verbatim. By inspection:
//!
//! - `linux_keyutils::KeyError` prints only the variant name (e.g.
//!   `"KeyDoesNotExist"`, `"AccessDenied"`), never the payload.
//! - `secret_service::Error` prints DBus error names, item paths, and
//!   prompt names — never the secret bytes (the bytes only ever travel
//!   inside the encrypted `Secret` zbus struct).
//!
//! If either crate changes that contract in the future, this module
//! must be re-audited.

use std::{collections::HashMap, sync::Mutex};

use thiserror::Error;
use zeroize::Zeroizing;

/// Prefix prepended to every caller-supplied id when forming a kernel
/// keyring description (`format!("{KEYRING_ID_PREFIX}{id}")`). The
/// trailing colon mirrors the conventional `service:key` style used by
/// other Rust keyring wrappers and makes a `keyctl list @s` dump
/// readable.
pub const KEYRING_ID_PREFIX: &str = "syauth:";

/// libsecret collection label that holds every syauth-managed secret.
/// We do not create a fresh collection (that would prompt the user);
/// instead the secret is stored in the *default* collection with this
/// value used as an attribute and as the item label prefix, so an
/// operator inspecting `secret-tool search kind syauth-bond` finds
/// exactly our items.
pub const SECRET_SERVICE_COLLECTION: &str = "syauth";

/// DBus attribute key used to discriminate syauth secrets from any
/// other items in the user's default collection.
pub const SECRET_SERVICE_ATTR_KIND: &str = "kind";

/// Value paired with [`SECRET_SERVICE_ATTR_KIND`] on every libsecret
/// item we create. Lets us search the default collection without
/// confusing third-party tools that store their own items there.
pub const SECRET_SERVICE_ATTR_KIND_VALUE: &str = "syauth-bond";

/// DBus attribute key used to carry the caller-supplied id on libsecret
/// items. Paired with the actual id at `put` / `get` / `remove` time.
pub const SECRET_SERVICE_ATTR_ID: &str = "id";

/// MIME content-type recorded on every libsecret item. The Secret
/// Service spec requires a value here; `application/octet-stream` is
/// the conventional opaque-bytes choice.
pub const SECRET_SERVICE_CONTENT_TYPE: &str = "application/octet-stream";

/// Log line emitted by [`detect_with_logger`] when the kernel keyring
/// backend is selected. Named at module scope so consumers
/// (e.g. tests in `tests/keyring_linux.rs`) can match on it.
pub const LOG_LINE_KERNEL: &str = "syauth: using kernel keyring backend";

/// Log line emitted by [`detect_with_logger`] when the libsecret /
/// secret-service backend is selected. Named at module scope for the
/// same reason as [`LOG_LINE_KERNEL`].
pub const LOG_LINE_SECRET_SERVICE: &str = "syauth: using libsecret (secret-service) backend";

/// Identifier returned alongside a successful [`detect_with_logger`]
/// for observability and for tests that need to assert which backend
/// was chosen without scraping the log line.
///
/// Variants:
/// - [`BackendKind::Kernel`] — `linux-keyutils`, the primary
///   production backend.
/// - [`BackendKind::SecretService`] — DBus libsecret, the fallback
///   when the kernel keyring is unreachable.
/// - [`BackendKind::InMemory`] — only ever returned when a caller
///   constructs [`InMemoryKeyStore`] directly. The
///   [`detect`] / [`detect_with_logger`] factories **never** return
///   this — production builds without a real backend get
///   [`SecretError::NotImplemented`] instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// Linux kernel keyring (primary).
    Kernel,
    /// libsecret / freedesktop Secret Service (fallback).
    SecretService,
    /// In-memory map; tests only.
    InMemory,
}

/// Errors returned by every method on [`KeyStore`] and by the
/// [`detect`] / [`detect_with_logger`] factories.
///
/// Variants never include secret payload bytes — see the module-level
/// audit note.
#[derive(Debug, Error)]
pub enum SecretError {
    /// The upstream backend (kernel keyring or libsecret) returned an
    /// error. The wrapped `String` is the upstream error's `Display`
    /// rendering; by audit this never contains the secret payload.
    #[error("syauth secret-store backend error: {0}")]
    Backend(String),

    /// No working backend could be detected on this host. Returned by
    /// [`detect`] / [`detect_with_logger`] when both the kernel
    /// keyring and libsecret probes fail. Production callers should
    /// fail closed.
    #[error("syauth secret-store: no working backend detected")]
    NotImplemented,
}

/// Read-on-demand secret storage for the host's Ed25519 private key and
/// per-peer bond keys.
///
/// **Contract for implementors:**
///
/// - `put` overwrites silently if `id` already exists.
/// - `get` returns `Ok(None)` for an unknown `id` — never an error.
/// - `remove` is idempotent: removing an unknown `id` returns
///   `Ok(())`.
/// - All `Ok(Some(...))` payloads are wrapped in
///   [`Zeroizing<Vec<u8>>`] and are wiped on drop. Implementors must
///   NOT return a bare `Vec<u8>` and rely on the caller to wrap it.
/// - The trait is `Send + Sync` so a `Box<dyn KeyStore>` can be passed
///   to a tokio task or a PAM module that re-enters from a different
///   thread.
///
/// **Contract for callers:**
///
/// - Never `clone()` a returned `Zeroizing<Vec<u8>>` and let the clone
///   outlive the original — that defeats the wipe-on-drop guarantee.
/// - Treat `Ok(None)` from `get` as "no such bond"; treat
///   [`SecretError`] as a hard failure (fail closed in PAM contexts).
pub trait KeyStore: Send + Sync {
    /// Store `secret` under `id`, overwriting any prior value for the
    /// same id.
    fn put(&self, id: &str, secret: &[u8]) -> Result<(), SecretError>;

    /// Read the secret for `id`. Returns `Ok(None)` if no value is
    /// stored under that id. The returned bytes are wiped from RAM when
    /// the [`Zeroizing<Vec<u8>>`] is dropped.
    fn get(&self, id: &str) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError>;

    /// Remove the secret for `id`. Idempotent: removing a non-existent
    /// id returns `Ok(())`.
    fn remove(&self, id: &str) -> Result<(), SecretError>;
}

// =============================================================================
// InMemoryKeyStore — test seam.
// =============================================================================

/// Process-local secret store backed by a `Mutex<HashMap>`. Stores
/// each value as a [`Zeroizing<Vec<u8>>`] so dropping the store wipes
/// every secret. Intended only for unit tests of upstream callers —
/// [`detect`] / [`detect_with_logger`] never return this backend.
#[derive(Default)]
pub struct InMemoryKeyStore {
    inner: Mutex<HashMap<String, Zeroizing<Vec<u8>>>>,
}

impl InMemoryKeyStore {
    /// Construct an empty in-memory store. Cheap and infallible.
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeyStore for InMemoryKeyStore {
    fn put(&self, id: &str, secret: &[u8]) -> Result<(), SecretError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|err| SecretError::Backend(format!("in-memory store mutex poisoned: {err}")))?;
        guard.insert(id.to_owned(), Zeroizing::new(secret.to_vec()));
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
        let guard = self
            .inner
            .lock()
            .map_err(|err| SecretError::Backend(format!("in-memory store mutex poisoned: {err}")))?;
        // We must produce a fresh `Zeroizing<Vec<u8>>` (not clone the
        // stored one — `Zeroizing` deliberately implements `Clone` in a
        // way that produces an independent zeroizable buffer, which is
        // exactly what we want here for the returned value). The stored
        // copy stays in the map until `remove` or the store drops.
        Ok(guard.get(id).map(|stored| Zeroizing::new(stored.to_vec())))
    }

    fn remove(&self, id: &str) -> Result<(), SecretError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|err| SecretError::Backend(format!("in-memory store mutex poisoned: {err}")))?;
        guard.remove(id);
        Ok(())
    }
}

// =============================================================================
// KernelKeyring — Linux primary backend.
// =============================================================================

#[cfg(target_os = "linux")]
pub use self::kernel::KernelKeyring;

#[cfg(target_os = "linux")]
mod kernel {
    //! Linux kernel keyring backend (primary).
    //!
    //! Wraps `linux-keyutils` against [`KeyRingIdentifier::Session`].
    //! We use the session keyring, not the user keyring (`@u`), because
    //! the session keyring is process-scoped and inherits with
    //! `setuid()` boundaries the way PAM stacks expect (SPEC §D6).

    use linux_keyutils::{KeyError, KeyRing, KeyRingIdentifier};
    use zeroize::Zeroizing;

    use super::{KEYRING_ID_PREFIX, KeyStore, SecretError};

    /// Compose the kernel keyring description for a caller-supplied
    /// id. Kept as a free function so tests can assert the exact
    /// description string we hand to the kernel.
    pub(crate) fn description_for(id: &str) -> String {
        format!("{KEYRING_ID_PREFIX}{id}")
    }

    /// Linux kernel keyring backed implementation of [`KeyStore`].
    pub struct KernelKeyring {
        ring: KeyRing,
    }

    impl KernelKeyring {
        /// Open the session keyring. Used by [`super::detect`] and by
        /// the integration test in `tests/keyring_linux.rs`.
        pub fn open() -> Result<Self, SecretError> {
            let ring = KeyRing::from_special_id(KeyRingIdentifier::Session, false).map_err(map_key_error)?;
            Ok(Self { ring })
        }

        /// Cheap reachability probe — opens the session keyring and
        /// searches for an obviously-absent key. Used by
        /// [`super::detect_with_logger`] to decide whether the kernel
        /// keyring backend is selectable on this host.
        pub fn probe() -> bool {
            let Ok(ring) = KeyRing::from_special_id(KeyRingIdentifier::Session, false) else {
                return false;
            };
            // Searching for a name we definitely have not registered
            // either returns `KeyDoesNotExist` (kernel reachable, behaves
            // as expected) or some other error (kernel unreachable).
            match ring.search(PROBE_DESCRIPTION) {
                Err(KeyError::KeyDoesNotExist) => true,
                Err(_) => false,
                // The probe key should not exist; if a parallel process
                // happens to have written it, the kernel is clearly
                // reachable and we treat that as success.
                Ok(_) => true,
            }
        }
    }

    /// Description used by [`KernelKeyring::probe`]. Includes a
    /// recognizable suffix so an operator who sees it in
    /// `keyctl list @s` immediately understands it is a probe artifact.
    const PROBE_DESCRIPTION: &str = "syauth:__probe__";

    impl KeyStore for KernelKeyring {
        fn put(&self, id: &str, secret: &[u8]) -> Result<(), SecretError> {
            // `add_key` updates in-place when an entry with the same
            // description already exists (per `linux-keyutils` docs and
            // `man 2 add_key`), so put-overwrite is implicit.
            self.ring.add_key(&description_for(id), secret).map_err(map_key_error)?;
            Ok(())
        }

        fn get(&self, id: &str) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
            match self.ring.search(&description_for(id)) {
                Ok(key) => match key.read_to_vec() {
                    Ok(raw) => Ok(Some(Zeroizing::new(raw))),
                    // A key that was just invalidated (`KEY_REVOKED`)
                    // or whose timeout elapsed (`KEY_EXPIRED`) is
                    // semantically absent — treat as `None` to keep
                    // `get` consistent with the trait contract.
                    Err(KeyError::KeyDoesNotExist | KeyError::KeyRevoked | KeyError::KeyExpired) => Ok(None),
                    Err(err) => Err(map_key_error(err)),
                },
                Err(KeyError::KeyDoesNotExist | KeyError::KeyRevoked | KeyError::KeyExpired) => Ok(None),
                Err(err) => Err(map_key_error(err)),
            }
        }

        fn remove(&self, id: &str) -> Result<(), SecretError> {
            match self.ring.search(&description_for(id)) {
                Ok(key) => match key.invalidate() {
                    // Already invalidated / revoked / expired by a
                    // racing process or a prior call: idempotent
                    // success per the trait contract.
                    Ok(()) | Err(KeyError::KeyDoesNotExist) | Err(KeyError::KeyRevoked) | Err(KeyError::KeyExpired) => Ok(()),
                    Err(err) => Err(map_key_error(err)),
                },
                // Search did not find a live key — already gone.
                Err(KeyError::KeyDoesNotExist | KeyError::KeyRevoked | KeyError::KeyExpired) => Ok(()),
                Err(err) => Err(map_key_error(err)),
            }
        }
    }

    /// Convert a `linux_keyutils::KeyError` to our typed
    /// [`SecretError`]. The wrapped message comes from `KeyError`'s
    /// `Display`, which by inspection contains only the variant name —
    /// no secret bytes.
    fn map_key_error(err: KeyError) -> SecretError {
        SecretError::Backend(format!("kernel keyring: {err}"))
    }
}

// =============================================================================
// SecretService — libsecret fallback.
// =============================================================================

#[cfg(target_os = "linux")]
pub use self::secret_service_impl::SecretService;

#[cfg(target_os = "linux")]
mod secret_service_impl {
    //! libsecret / freedesktop Secret Service fallback.
    //!
    //! Uses the upstream `secret-service` crate's `blocking` submodule
    //! so the public [`super::KeyStore`] trait stays synchronous. Each
    //! call opens a short-lived DBus session connection; that is
    //! acceptable for the pairing and revoke flows (rare) and for the
    //! once-per-PAM-call read (a single DBus round-trip is on the order
    //! of a millisecond on a warm bus).

    use std::collections::HashMap;

    use secret_service::{
        EncryptionType,
        blocking::{Collection, SecretService as SsClient},
    };
    use zeroize::Zeroizing;

    use super::{
        KeyStore, SECRET_SERVICE_ATTR_ID, SECRET_SERVICE_ATTR_KIND, SECRET_SERVICE_ATTR_KIND_VALUE, SECRET_SERVICE_COLLECTION,
        SECRET_SERVICE_CONTENT_TYPE, SecretError,
    };

    /// libsecret-backed implementation of [`KeyStore`].
    pub struct SecretService;

    impl SecretService {
        /// Construct the fallback backend. Does NOT eagerly open the
        /// DBus session — each operation opens a fresh short-lived
        /// connection because the underlying `secret-service` types
        /// borrow a `&Connection` and would force us to either build a
        /// long-lived self-referential struct or keep the connection
        /// alive globally. Connection-per-call keeps lifetimes simple
        /// and matches the rare-access usage profile.
        pub fn new() -> Self {
            Self
        }

        /// Reachability probe — open a DBus session and confirm the
        /// default collection responds. Used by
        /// [`super::detect_with_logger`].
        pub fn probe() -> bool {
            let Ok(ss) = SsClient::connect(EncryptionType::Dh) else {
                return false;
            };
            ss.get_default_collection().is_ok()
        }
    }

    impl Default for SecretService {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Compose the attribute map for an item with the given id. Used
    /// by both `put` (to register) and `get` / `remove` (to search).
    fn attributes_for(id: &str) -> HashMap<&str, &str> {
        let mut attrs: HashMap<&str, &str> = HashMap::new();
        attrs.insert(SECRET_SERVICE_ATTR_KIND, SECRET_SERVICE_ATTR_KIND_VALUE);
        attrs.insert(SECRET_SERVICE_ATTR_ID, id);
        attrs
    }

    /// Open a DBus connection and resolve the default collection.
    /// Centralized so `put` / `get` / `remove` share one error path.
    fn open_collection<'a>(ss: &'a SsClient<'a>) -> Result<Collection<'a>, SecretError> {
        ss.get_default_collection().map_err(map_ss_error)
    }

    impl KeyStore for SecretService {
        fn put(&self, id: &str, secret: &[u8]) -> Result<(), SecretError> {
            let ss = SsClient::connect(EncryptionType::Dh).map_err(map_ss_error)?;
            let collection = open_collection(&ss)?;
            let label = format!("{SECRET_SERVICE_COLLECTION}: {id}");
            collection
                .create_item(
                    &label,
                    attributes_for(id),
                    secret,
                    /* replace = */ true,
                    SECRET_SERVICE_CONTENT_TYPE,
                )
                .map_err(map_ss_error)?;
            Ok(())
        }

        fn get(&self, id: &str) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
            let ss = SsClient::connect(EncryptionType::Dh).map_err(map_ss_error)?;
            let collection = open_collection(&ss)?;
            let items = collection.search_items(attributes_for(id)).map_err(map_ss_error)?;
            let Some(item) = items.into_iter().next() else {
                return Ok(None);
            };
            let raw = item.get_secret().map_err(map_ss_error)?;
            Ok(Some(Zeroizing::new(raw)))
        }

        fn remove(&self, id: &str) -> Result<(), SecretError> {
            let ss = SsClient::connect(EncryptionType::Dh).map_err(map_ss_error)?;
            let collection = open_collection(&ss)?;
            let items = collection.search_items(attributes_for(id)).map_err(map_ss_error)?;
            for item in items {
                item.delete().map_err(map_ss_error)?;
            }
            Ok(())
        }
    }

    /// Convert a `secret_service::Error` to our typed [`SecretError`].
    /// The upstream `Display` impl prints DBus error names and item
    /// paths but never the secret payload (audited at module top).
    fn map_ss_error(err: secret_service::Error) -> SecretError {
        SecretError::Backend(format!("libsecret: {err}"))
    }
}

// =============================================================================
// detect() factory.
// =============================================================================

/// Detect the first working secret-store backend at process start.
///
/// Tries the kernel keyring first; if that probe fails (e.g. container
/// without `CONFIG_KEYS`), falls back to libsecret. If both fail,
/// returns [`SecretError::NotImplemented`] — production callers must
/// fail closed rather than running without persistent secrets.
///
/// Emits exactly one log line to `eprintln!` naming the selected
/// backend. Use [`detect_with_logger`] to route the log into a custom
/// sink (e.g. `tracing::info!`).
pub fn detect() -> Result<Box<dyn KeyStore>, SecretError> {
    detect_with_logger(|line| eprintln!("{line}"))
}

/// Same as [`detect`] but routes the one-shot log line into a
/// caller-supplied sink so the host process can plumb it into its own
/// `tracing` / `log` setup.
pub fn detect_with_logger<F: Fn(&str)>(log: F) -> Result<Box<dyn KeyStore>, SecretError> {
    #[cfg(target_os = "linux")]
    {
        if KernelKeyring::probe() {
            let store = KernelKeyring::open()?;
            log(LOG_LINE_KERNEL);
            return Ok(Box::new(store));
        }
        if SecretService::probe() {
            log(LOG_LINE_SECRET_SERVICE);
            return Ok(Box::new(SecretService::new()));
        }
    }
    // On non-Linux hosts neither backend exists; the
    // workspace-level `cfg(target_os = "linux")` gating in the
    // PAM module means we should never get here in production,
    // but a clear error beats a silent in-memory fallback.
    let _ = &log;
    Err(SecretError::NotImplemented)
}

// =============================================================================
// Unit tests — InMemoryKeyStore.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// TC-01: a fresh `InMemoryKeyStore` roundtrips one put → get.
    #[test]
    fn inmemory_roundtrip() {
        let store = InMemoryKeyStore::new();
        store.put("test-id", b"secret-bytes").unwrap();
        let got = store.get("test-id").unwrap().expect("expected Some after put");
        assert_eq!(&*got, b"secret-bytes");
    }

    /// TC-02: `get` on an id that was never put returns `Ok(None)`.
    #[test]
    fn inmemory_get_missing_returns_none() {
        let store = InMemoryKeyStore::new();
        let got = store.get("never-put").unwrap();
        assert!(got.is_none());
    }

    /// TC-03: a second `put` for the same id overwrites the prior
    /// value.
    #[test]
    fn inmemory_double_put_overwrites() {
        let store = InMemoryKeyStore::new();
        store.put("id", b"first").unwrap();
        store.put("id", b"second").unwrap();
        let got = store.get("id").unwrap().expect("expected Some after put");
        assert_eq!(&*got, b"second");
    }

    /// TC-04: after `remove`, `get` reports `Ok(None)`.
    #[test]
    fn inmemory_remove_makes_get_return_none() {
        let store = InMemoryKeyStore::new();
        store.put("id", b"x").unwrap();
        store.remove("id").unwrap();
        assert!(store.get("id").unwrap().is_none());
    }

    /// TC-05: `remove` of a never-put id is `Ok(())` — idempotent.
    #[test]
    fn inmemory_remove_missing_is_ok() {
        let store = InMemoryKeyStore::new();
        store.remove("never-put").unwrap();
    }

    /// TC-06: the public `KeyStore::get` signature returns
    /// `Zeroizing<Vec<u8>>`. We assert this at compile time by binding
    /// the result to a value with the exact expected type; if the
    /// trait signature ever drops `Zeroizing`, this test fails to
    /// compile.
    #[test]
    fn inmemory_get_returns_zeroizing_vec() {
        let store = InMemoryKeyStore::new();
        store.put("id", b"x").unwrap();
        let got: Option<Zeroizing<Vec<u8>>> = store.get("id").unwrap();
        assert!(got.is_some());
    }

    /// TC-07: `detect_with_logger` never panics. On a host with the
    /// kernel keyring it returns Ok; on a container without either
    /// backend it returns `SecretError::NotImplemented`. Either is
    /// acceptable — the test only pins the "never panic, never silent
    /// fallthrough to InMemory" invariant.
    #[test]
    fn detect_returns_real_backend_or_not_implemented() {
        let mut lines: Vec<String> = Vec::new();
        // `detect_with_logger` takes `impl Fn(&str)`, so the closure
        // cannot mutably capture; collect via a shared `Mutex` and read
        // it back after.
        let collected = std::sync::Mutex::new(Vec::<String>::new());
        let result = detect_with_logger(|line| {
            collected.lock().unwrap().push(line.to_owned());
        });
        lines.extend(collected.into_inner().unwrap());
        match result {
            Ok(_store) => {
                assert!(
                    lines.iter().any(|l| l == LOG_LINE_KERNEL || l == LOG_LINE_SECRET_SERVICE),
                    "expected exactly one backend-selection log line; got {lines:?}"
                );
            }
            Err(SecretError::NotImplemented) => {
                assert!(lines.is_empty(), "no log expected on NotImplemented; got {lines:?}");
            }
            Err(err) => panic!("unexpected error from detect_with_logger: {err}"),
        }
    }
}
