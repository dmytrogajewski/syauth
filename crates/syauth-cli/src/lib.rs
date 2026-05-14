//! `syauth-cli` library surface.
//!
//! The binary in `src/main.rs` is a thin clap-based dispatcher; every
//! subcommand's logic lives here as a library function so that integration
//! tests, future fuzz harnesses, and in-process callers can drive the
//! behavior directly. The canonical user path remains the built `syauth`
//! binary driven by `assert_cmd` in `tests/install_pam.rs`.
//!
//! Roadmap: specs/syauth/ROADMAP.md item S-013.
//! Journey: specs/journeys/JOURNEY-S-013-pam-install-helper.md

pub mod install_pam;
pub mod uninstall_pam;
