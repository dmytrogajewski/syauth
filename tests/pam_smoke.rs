//! S-008 e2e smoke test for the syauth PAM module.
//!
//! Journey: specs/journeys/JOURNEY-S-008-pam-skeleton.md
//!
//! This test is **gated**. It is a no-op (with an explanatory skip message)
//! unless the environment variable `SYAUTH_E2E=1` is set. When enabled and
//! the host has `pamtester` installed, it:
//!
//! 1. resolves the absolute path to `target/release/libpam_syauth.so`
//!    (built by `make build` / `cargo build --release -p syauth-pam`);
//! 2. rewrites `tests/pam.d/syauth-test` so the `auth` and `account` lines
//!    point at that absolute path (placeholder `__SYAUTH_SO_PATH__`);
//! 3. runs `pamtester --conf-dir tests/pam.d syauth-test "$USER" authenticate`
//!    and asserts the exit status corresponds to `PAM_AUTHINFO_UNAVAIL`;
//! 4. greps `journalctl -t pam_syauth` for the documented stub log line.
//!
//! When pamtester is absent on the runner, the test prints a one-line skip
//! and exits 0 — `make test` must stay green on a vanilla developer box.
//!
//! Verification matrix:
//!
//! | DoD checkbox | Asserted by which code path below |
//! |--------------|-----------------------------------|
//! | three `pam_sm_*` symbols, no Rust mangling | `nm_symbol_audit` test (always on if release artefact exists; skipped otherwise) |
//! | fixture references the `.so` by absolute path | `fixture_path_is_absolute_when_generated` test |
//! | `authenticate` returns `PAM_AUTHINFO_UNAVAIL` | `pamtester_authenticate_returns_authinfo_unavail` |
//! | syslog line `syauth: unlock unavailable reason=stub` appears | same test, second assertion via `journalctl_recent_has_stub_line` helper |

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Environment switch that enables the e2e test. The skill instructions and
/// the roadmap both pin this exact name.
const E2E_GATE_VAR: &str = "SYAUTH_E2E";

/// Placeholder substituted in the fixture file at test time.
const SO_PATH_PLACEHOLDER: &str = "__SYAUTH_SO_PATH__";

/// The exact substring the e2e test greps for in journalctl output.
const STUB_LOG_SUBSTR: &str = "syauth: unlock unavailable reason=stub";

/// The standard libpam string emitted by pamtester when a module returns
/// `PAM_AUTHINFO_UNAVAIL`. Stable across libpam versions in the Linux PAM
/// project ([Linux-PAM source]). We match a substring to tolerate trailing
/// whitespace differences.
const PAM_AUTHINFO_UNAVAIL_MSG: &str = "Authentication service cannot retrieve authentication info";

/// Repo root resolved from `CARGO_MANIFEST_DIR`. The root crate's manifest
/// IS the repo root for this workspace.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Absolute path to the built `libpam_syauth.so`. The path matches what
/// `make build` (and `cargo build --release -p syauth-pam`) produces.
fn cdylib_path() -> PathBuf {
    repo_root().join("target").join("release").join("libpam_syauth.so")
}

/// Path to the fixture pam.d directory and the per-service file we drive.
fn fixture_dir() -> PathBuf {
    repo_root().join("tests").join("pam.d")
}
fn fixture_file() -> PathBuf {
    fixture_dir().join("syauth-test")
}

/// Look for a binary on `$PATH`. Returns `None` if not found.
fn which(bin: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// True if the e2e gate is on (env var literally `1`).
fn e2e_gate_on() -> bool {
    env::var(E2E_GATE_VAR).ok().as_deref() == Some("1")
}

/// Rewrite the fixture file so every occurrence of the placeholder is
/// replaced with the absolute path to the built `.so`.
///
/// Returns the rewritten contents so the caller can assert invariants on
/// them without re-reading the file.
fn generate_fixture(so_path: &Path) -> std::io::Result<String> {
    let template_path = fixture_file();
    let template = fs::read_to_string(&template_path)?;
    let so_str = so_path.to_string_lossy();
    // Idempotent: if the placeholder is gone (previous run left an absolute
    // path) we restore it first, so reruns are deterministic.
    let normalized = if template.contains(SO_PATH_PLACEHOLDER) {
        template
    } else {
        // Best-effort: replace any line that mentions `libpam_syauth.so`
        // with the placeholder form before substitution.
        template
            .lines()
            .map(|l| {
                if l.contains("libpam_syauth.so") {
                    let leading: String = l.chars().take_while(|c| c.is_whitespace()).collect();
                    if let Some(kw) = l.split_whitespace().next() {
                        let mut parts = l.split_whitespace();
                        let _ = parts.next();
                        let control = parts.next().unwrap_or("required");
                        format!("{leading}{kw}     {control}    {SO_PATH_PLACEHOLDER}")
                    } else {
                        l.to_owned()
                    }
                } else {
                    l.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let rendered = normalized.replace(SO_PATH_PLACEHOLDER, &so_str);
    fs::write(&template_path, &rendered)?;
    Ok(rendered)
}

/// Run `journalctl -t pam_syauth --since "1 minute ago" --no-pager` and
/// return its stdout. If `journalctl` is unavailable or returns non-zero,
/// returns `None` so the caller can decide whether to fail the test.
fn journalctl_recent_tag() -> Option<String> {
    let out = Command::new("journalctl")
        .args(["-t", "pam_syauth", "--since", "1 minute ago", "--no-pager"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

// -----------------------------------------------------------------------------
// Always-on assertions (these run even when SYAUTH_E2E is unset, because
// they don't need any external binary — they only inspect repo state and
// any release artefact that happens to be on disk).
// -----------------------------------------------------------------------------

/// Confirm the committed fixture file uses the placeholder, not a stale
/// absolute path. A committed absolute path would be unusable on another
/// machine.
#[test]
fn fixture_template_uses_placeholder() {
    let template = fs::read_to_string(fixture_file()).expect("fixture file exists");
    assert!(
        template.contains(SO_PATH_PLACEHOLDER),
        "fixture must reference the placeholder {SO_PATH_PLACEHOLDER}; got:\n{template}"
    );
    assert!(
        !template.contains("/home/") && !template.contains("/tmp/"),
        "fixture must not contain a stale absolute path baked in"
    );
}

/// If the release `.so` has been built, audit its dynamic symbol table.
/// Skips with a single-line message if the artefact is missing.
#[test]
fn nm_symbol_audit() {
    let so = cdylib_path();
    if !so.is_file() {
        eprintln!(
            "skipping nm_symbol_audit: {} not built yet (run `cargo build --release -p syauth-pam`)",
            so.display()
        );
        return;
    }
    let nm = match which("nm") {
        Some(p) => p,
        None => {
            eprintln!("skipping nm_symbol_audit: `nm` not on PATH");
            return;
        }
    };
    let out = Command::new(&nm).args(["-D", "--defined-only"]).arg(&so).output().expect("nm runs");
    assert!(out.status.success(), "nm failed: {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);

    let pam_sm_lines: Vec<&str> = stdout.lines().filter(|l| l.contains(" pam_sm_")).collect();
    assert_eq!(
        pam_sm_lines.len(),
        3,
        "expected exactly three pam_sm_* symbols, got {}: {pam_sm_lines:?}",
        pam_sm_lines.len()
    );
    for want in &["pam_sm_authenticate", "pam_sm_setcred", "pam_sm_acct_mgmt"] {
        assert!(
            pam_sm_lines.iter().any(|l| l.ends_with(want)),
            "missing symbol {want} in nm output:\n{stdout}"
        );
    }

    // No Rust-mangled names should leak. The `_ZN` prefix is the canonical
    // Itanium mangling Rust emits for non-`no_mangle` symbols.
    let mangled: Vec<&str> = stdout.lines().filter(|l| l.contains(" _ZN")).collect();
    assert!(
        mangled.is_empty(),
        "Rust-mangled symbols leaked into the dynamic table:\n{mangled:?}"
    );
}

// -----------------------------------------------------------------------------
// E2E path (gated on SYAUTH_E2E=1 and the presence of pamtester).
// -----------------------------------------------------------------------------

#[test]
fn pamtester_authenticate_returns_authinfo_unavail() {
    if !e2e_gate_on() {
        eprintln!("skipping pam_smoke: set SYAUTH_E2E=1 to run");
        return;
    }
    let pamtester = match which("pamtester") {
        Some(p) => p,
        None => {
            eprintln!("skipping pam_smoke: `pamtester` not on PATH (install pamtester to enable)");
            return;
        }
    };
    let so = cdylib_path();
    assert!(
        so.is_file(),
        "{} not built; run `cargo build --release -p syauth-pam` first",
        so.display()
    );

    let rendered = generate_fixture(&so).expect("can rewrite fixture");
    assert!(
        rendered.contains(so.to_str().expect("ascii path")),
        "rewritten fixture must reference {}",
        so.display()
    );

    let user = env::var("USER").unwrap_or_else(|_| "nobody".to_owned());
    let out = Command::new(&pamtester)
        .args(["--conf-dir"])
        .arg(fixture_dir())
        .arg("syauth-test")
        .arg(&user)
        .arg("authenticate")
        .output()
        .expect("pamtester runs");

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stderr.contains(PAM_AUTHINFO_UNAVAIL_MSG) || stdout.contains(PAM_AUTHINFO_UNAVAIL_MSG),
        "pamtester output did not name PAM_AUTHINFO_UNAVAIL.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !out.status.success(),
        "pamtester unexpectedly exited 0 — the stub must return PAM_AUTHINFO_UNAVAIL"
    );

    // Verify the syslog line. journalctl is the default on systemd hosts;
    // if it's absent (e.g. alpine/musl), document the gap with a skip and
    // leave the rest of the assertions intact.
    match journalctl_recent_tag() {
        Some(log) => {
            assert!(
                log.contains(STUB_LOG_SUBSTR),
                "journalctl -t pam_syauth did not contain {STUB_LOG_SUBSTR:?}; got:\n{log}"
            );
        }
        None => {
            eprintln!("warning: journalctl unavailable or empty; the return-code assertion still passed, but the syslog grep was skipped");
        }
    }
}

#[test]
fn fixture_path_is_absolute_when_generated() {
    if !e2e_gate_on() {
        eprintln!("skipping fixture_path_is_absolute_when_generated: set SYAUTH_E2E=1 to run");
        return;
    }
    let so = cdylib_path();
    if !so.is_file() {
        eprintln!("skipping fixture_path_is_absolute_when_generated: {} not built", so.display());
        return;
    }
    let rendered = generate_fixture(&so).expect("rewrite fixture");
    // Every non-comment, non-blank line that references libpam_syauth.so
    // must do so with an absolute path.
    for (lineno, line) in rendered.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.contains("libpam_syauth.so") {
            let token = trimmed.split_whitespace().find(|t| t.contains("libpam_syauth.so")).unwrap_or("");
            assert!(
                token.starts_with('/'),
                "line {n}: .so path is not absolute: {token:?}",
                n = lineno + 1
            );
            assert!(
                PathBuf::from(token).is_file(),
                "line {n}: .so path does not resolve to a file: {token:?}",
                n = lineno + 1
            );
        }
    }
}
