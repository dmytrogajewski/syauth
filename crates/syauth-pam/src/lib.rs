//! Placeholder for `syauth-pam`.
//!
//! The eventual cdylib will export `pam_sm_authenticate`, `pam_sm_setcred`,
//! and `pam_sm_acct_mgmt`. S-001 ships an empty `cdylib` so that `make build`
//! produces `target/release/libpam_syauth.so` and proves the `crate-type =
//! ["cdylib"]` + `name = "pam_syauth"` configuration is correct. Roadmap
//! items S-008 and S-009 land the entry points.
