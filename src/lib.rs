//! syauth workspace root package.
//!
//! This crate intentionally exposes no public API. It exists so that the
//! repo-level `tests/` directory has a host package for `cargo test --workspace`
//! to drive integration tests against. Business code lives in the member crates
//! under `crates/syauth-*`.
//!
//! See `specs/syauth/SPEC.md` §4.1 for the workspace layout rationale and
//! `specs/journeys/JOURNEY-S-001-workspace-bootstrap.md` for the bootstrap journey.
