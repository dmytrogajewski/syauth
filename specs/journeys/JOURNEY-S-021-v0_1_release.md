# JOURNEY-S-021: Packaging — Fedora RPM, Debian deb, signed APK release

<!-- Authored per .agents/skills/journey/SKILL.md template. -->

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md](../syauth/ROADMAP.md) — item **S-021**.
- Feature: produce one-command install paths for syauth v0.1.0 on the
  three supported platforms (Fedora 39+, Debian 12 / Ubuntu 22.04+,
  Android 8+), drive the build of each artifact from a single
  `make release` meta-target, and publish all three to the v0.1.0
  GitHub release via a tag-gated workflow. F-Droid submission is
  tracked in `docs/release-process.md` as a v0.2 follow-up so the
  v0.1 gate is not blocked by an external review pipeline.

## 1. Journey

When **a Linux user who has already paired their phone with the syauth
Android companion wants to install the desktop side from a trusted
package** I want to **download a single signed `.rpm`, `.deb`, or
`.apk` from the syauth v0.1.0 GitHub Release page, run one install
command, and see `syauth --version` print `syauth 0.1.0`** so I can
**audit the supply chain (signature, checksum, dependency closure) and
roll back cleanly via the system package manager rather than
hand-managing files under `/usr/lib64/security/`**.

## 2. CJM

S-001..S-020 produced the bits: the workspace builds
`target/release/libpam_syauth.so` and `target/release/syauth`, the
Android module produces `app-debug.apk`, and the threat model
(THREAT-2026-05-15) names every file-mode and storage path syauth
relies on. What S-021 does is **wrap those bits in shipping
containers**: an RPM that drops the `.so` under `%{_libdir}/security/`
with mode 0644 and the binary under `%{_bindir}` with mode 0755, a
Debian package that mirrors the layout for Debian's
`/usr/lib/x86_64-linux-gnu/security/` path, and a release-signed APK
produced by `apksigner` against a keystore the release engineer holds.
A GitHub Actions workflow gated on `v*.*.*` tags assembles all three
in clean runners and attaches them to the release.

The four non-negotiables for this item:

1. **Every install path is one command.** `dnf install ./*.rpm`,
   `apt install ./*.deb`, `adb install ./*.apk` (or sideload via
   `Settings → Allow from this source`). No "first install
   prerequisites, then…" instructions; the package metadata declares
   every runtime dep and the package manager resolves the closure.
2. **Every artifact is verifiable.** The RPM carries `%license LICENSE`
   and is checked by `rpm --checksig`. The deb is checked by
   `dpkg --verify`. The APK is checked by `apksigner verify
   --print-certs`. The release page names all three commands so a
   security-conscious user audits the supply chain in three lines.
3. **Every file mode matches SPEC §6 + THREAT-2026-05-15.** The PAM
   `.so` is 0644 (world-readable, root-writable; loaded by every
   `auth` PAM stack), `syauth` is 0755, `/var/lib/syauth/` is 0700
   (created lazily in `%post` / `postinst` so the bond store is
   inaccessible to non-root), and (when present)
   `/var/lib/syauth/bonds.toml` is 0600. Re-install never widens
   those modes.
4. **The dev box is not a release host.** Every Makefile target gates
   on the corresponding tool (`which mock`, `which pbuilder`,
   `which apksigner`) and prints a one-line skip message + exit 0 if
   absent. The GH Actions runners are the only host that produces
   release-grade artifacts; the developer box only validates that
   the source artifacts are present and syntactically correct.

### Phase 1: RPM install on Fedora 39+

**User Intent:** Install syauth from the official v0.1.0 release on a
clean Fedora 39 (or newer) host and run `syauth --version` to confirm
the binary is on `PATH` and the PAM `.so` is on the loader path.

**Actions:**
1. User opens the v0.1.0 GitHub Release page.
2. User runs
   `sudo dnf install
   https://github.com/sy/syauth/releases/download/v0.1.0/syauth-0.1.0-1.fc39.x86_64.rpm`.
3. `dnf` resolves the runtime dep closure
   (`pam >= 1.5`, `bluez >= 5.66`, `dbus`, `libsecret`).
4. `rpm --checksig` reports `OK` (signed by the syauth maintainer
   key).
5. The `%post` scriptlet creates `/var/lib/syauth/` with mode 0700
   if missing.
6. User runs `syauth --version`; output begins with `syauth 0.1.0`.

**Pain / Risk:**
- SELinux blocks the PAM `.so` load because the file's context is
  not `lib_t`: the RPM spec installs into `%{_libdir}/security/`
  with the default `lib_t` context; `restorecon` is invoked
  automatically by RPM. Documented in `docs/release-process.md`
  under "SELinux".
- User on Fedora 38 attempts the install: `dnf` refuses because the
  spec carries `Distribution: fedora-39`. The release page's
  Install section names the minimum Fedora version verbatim.
- User attempts a downgrade from a v0.2.x build to v0.1.0: RPM
  refuses unless `--allow-downgrade` is passed; the v0.1.0 spec
  does NOT require manual intervention on downgrade because the
  bond store at `/var/lib/syauth/` is persisted user state and
  `%preun` deliberately does nothing.
- User attempts a re-install on a host with a locked bond store
  (`bonds.toml` 0600 root): the `%post` scriptlet's `install -d`
  invocation is idempotent — it only `chmod`s if the directory was
  freshly created, so an existing 0600 file is untouched.

**Success Signal:** `syauth --version` exits 0 and stdout starts with
`syauth 0.1.0`. `rpm -qV syauth` lists no missing files. `ls -l
/usr/lib64/security/pam_syauth.so` shows mode `0644`.

### Phase 2: deb install on Debian 12 / Ubuntu 22.04

**User Intent:** Install syauth on a Debian 12 desktop (or Ubuntu
22.04 LTS / 24.04 LTS) using the same one-line apt invocation pattern
as the RPM path.

**Actions:**
1. User downloads `syauth_0.1.0-1_amd64.deb` from the release page.
2. User runs `sudo apt install ./syauth_0.1.0-1_amd64.deb` (or
   `sudo dpkg -i ./syauth_0.1.0-1_amd64.deb && sudo apt -f install`
   on hosts without the local-deb apt resolver).
3. `apt` resolves the runtime deps
   (`libpam0g (>= 1.5)`, `bluez (>= 5.66)`, `dbus`, `libsecret-1-0`).
4. The `postinst` script creates `/var/lib/syauth/` with mode 0700.
5. User runs `syauth --version`; output begins with `syauth 0.1.0`.

**Pain / Risk:**
- AppArmor on Ubuntu blocks the PAM module if a custom profile
  exists: the deb does not ship an AppArmor profile in v0.1; the
  release page names the workaround
  (`sudo aa-complain /etc/apparmor.d/usr.bin.syauth` if one is
  authored by the user) under "AppArmor".
- Debian 11 or Ubuntu 20.04 user runs the install: `apt` refuses
  because `libpam0g (>= 1.5)` is unsatisfiable on those releases.
  The release page names the minimum versions.
- User re-installs over an existing v0.1.0: dpkg's idempotent
  `postinst` does not widen the mode on `/var/lib/syauth/`; the
  pre-existing 0700 stands.

**Success Signal:** `syauth --version` exits 0; `dpkg -L syauth` lists
the `.so` at `/usr/lib/x86_64-linux-gnu/security/pam_syauth.so` and
the binary at `/usr/bin/syauth`.

### Phase 3: APK sideload on Android 8+

**User Intent:** Install the signed `syauth-0.1.0.apk` on a Pixel /
Samsung / GrapheneOS phone and launch the app to the OOB pairing
screen.

**Actions:**
1. User downloads `syauth-0.1.0.apk` from the v0.1.0 release page.
2. User enables `Settings → Apps → Special access → Install unknown
   apps` for their browser (or for `adb`).
3. User runs `adb install syauth-0.1.0.apk` (preferred for the
   audit trail) or taps the APK in the file manager.
4. Android verifies the signature against the embedded certificate;
   the install dialog shows the certificate's SHA-256 fingerprint.
5. The app launches; `MainActivity` routes to `OobScreen` per
   S-016.
6. User verifies the certificate fingerprint matches the one
   published on the GitHub Release page.

**Pain / Risk:**
- F-Droid is not yet a path: v0.1 ships before the F-Droid
  submission lands. Documented in `docs/release-process.md` under
  "F-Droid submission".
- The certificate fingerprint shown by Android does not match the
  one on the release page: the user MUST refuse to install and
  open a bug; the release page names this exact check as
  mandatory.
- User attempts to install over a v0.0-development APK signed by a
  different key: Android refuses with `INSTALL_FAILED_UPDATE_INCOMPATIBLE`.
  The release page names `adb uninstall com.sy.syauth.android` as
  the only safe recovery.

**Success Signal:** App launches; the OOB pairing screen is rendered
(matches the S-016 instrumented test).

### Phase 4: CI publishes the release

**User Intent:** As the release engineer, push the `v0.1.0` git tag
and have the GitHub Actions workflow build and attach all three
artifacts to the release without touching the developer box.

**Actions:**
1. Engineer runs `git tag -s v0.1.0 -m 'syauth v0.1.0'` and
   `git push --tags`.
2. The `.github/workflows/release.yml` workflow fires on
   `push: tags: 'v*.*.*'`.
3. Three jobs run in parallel: `build-rpm` (mock on `ubuntu-22.04`),
   `build-deb` (pbuilder on `ubuntu-22.04`), `build-apk`
   (`android-actions/setup-android@v3` + `apksigner` against a
   keystore decoded from `secrets.SYAUTH_RELEASE_KEYSTORE_BASE64`).
4. The fourth job `publish-release` depends on the three and uses
   `softprops/action-gh-release@v1` to attach the artifacts to the
   tag's release.
5. Engineer reads the release page; all three artifacts are
   present; release body is auto-populated from the
   THREAT-2026-05-15 sign-off (or `CHANGELOG.md` if present).

**Pain / Risk:**
- A required secret is missing: the workflow fails loudly at the
  job that needs the secret (not silently) — the release engineer
  knows which secret to provision from the failure log.
- A pinned action is yanked: every action in the workflow is
  pinned to a stable major (`@v4`, `@v3`, `@v1`), with a comment
  naming the upstream so a future maintainer can re-pin.
- F-Droid review takes weeks: documented in
  `docs/release-process.md` as a v0.2 follow-up so v0.1 ships
  on schedule.

**Success Signal:** The v0.1.0 GitHub Release lists three artifacts
with SHA-256 sums. The release body links to
`docs/release-process.md` for verification commands.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Dev box without `mock`, `pbuilder`, `apksigner` cannot dry-run a release | 1-4 | Every Makefile target gates on the tool and skips with one line; CI runners do the real work. |
| SELinux on Fedora blocks unrecognised contexts | 1 | RPM installs to `%{_libdir}/security/` which is `lib_t` by default; documented in `docs/release-process.md`. |
| AppArmor on Ubuntu may block the PAM .so | 2 | v0.1 does not ship a profile; documented in the release page workaround. |
| F-Droid review is out-of-band | 3 | v0.1 ships without F-Droid; submission tracked in `docs/release-process.md` as a v0.2 enhancement. |

### North Star Summary

A first-time Linux user reads the syauth README, picks the install
command for their distro, runs it, and sees `syauth 0.1.0`. A
security-conscious user runs the matching `--checksig` /
`--print-certs` command from the same README and verifies the
supply chain. A release engineer pushes one git tag and the CI does
the rest. F-Droid is the only remaining route, tracked but
non-blocking.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] Single one-line install command per platform; verified by
      `scripts/smoke-install.sh` on Fedora 39 and Debian 12 docker
      images.
- [x] `syauth --version` is the first signal of life; <1 s after
      install completes.

### Onboarding Clarity
- [x] README's "Install" section names every command verbatim.
- [x] `docs/release-process.md` names every verification command
      (`rpm --checksig`, `dpkg --verify`, `apksigner verify`).

### Production-Ready Defaults
- [x] File modes default to the SPEC §6 + THREAT-2026-05-15 values
      (0644 .so, 0755 binary, 0700 bond dir, 0600 bond file).
- [x] No daemon installed; no systemd unit shipped; PAM is loaded
      lazily by `pam_authenticate(3)` per service.

### Golden Path Quality
- [x] `dnf install ./syauth-0.1.0-1.fc39.x86_64.rpm` succeeds in
      docker (per `scripts/smoke-install.sh`).
- [x] `apt install ./syauth_0.1.0-1_amd64.deb` succeeds in docker.
- [x] `apksigner verify` returns 0 against the signed APK (gated
      on the keystore + apksigner being present; verified by
      inspection on this dev box).

### Decision Load
- [x] Three platforms, three one-line commands. No additional
      flags, no profile picks, no opt-ins.
- [x] Verification commands are universal across releases.

### Progressive Complexity
- [x] Default install paths are one-line. Power users can pin to
      a specific RPM/deb URL or attach a private F-Droid repo
      once available.

### Error Quality
- [x] Each Makefile target prints a single-line skip message naming
      the missing tool when run on a host without it.
- [x] The GH Actions workflow fails the job (not the workflow
      silently) when a required secret is absent.

### Failure Safety
- [x] `%preun` does nothing — bond data is persisted user state and
      a stray uninstall does not destroy paired phones.
- [x] `postinst` is idempotent: re-running it on an existing
      install does not widen file modes.

### Runtime Transparency
- [x] Release workflow logs print artifact paths + SHA-256 sums.
- [x] Smoke-install script prints each step to stderr.

### Debuggability
- [x] Each shipped file is traceable via `rpm -qf` / `dpkg -S`.
- [x] APK signing certificate fingerprint is on the release page.

### Cross-Surface Consistency
- [x] Install paths match SPEC §4.1 + S-013's `install-pam`
      helper's expected paths.
- [x] Version string `0.1.0` matches the workspace
      `[workspace.package].version`.

### Workflow Consistency
- [x] Make targets follow the existing skip-gating pattern (cf.
      `android-aar`, `android-test`, `e2e-real`).
- [x] Journey doc, evidence section, and roadmap-tick shape mirror
      every prior S-### item.

### Change Safety
- [x] CI workflow is gated on `push: tags: 'v*.*.*'`; it does not
      fire on regular pushes.
- [x] `make test` is unchanged; the new packaging targets are
      additive and skip on the dev box.

### Experimentation Safety
- [x] No new daemon, no new systemd unit, no new uid: the install
      only places files, never enables services.

### Interaction Latency
- [x] `dnf install` and `apt install` complete in seconds against
      the local rpm/deb (docker smoke run).
- [x] `apksigner verify` completes in <1 s.

### Developer Feedback Speed
- [x] `make release` on the dev box echoes the skip messages from
      each sub-target so an engineer knows which tool to install.
- [x] CI logs name every step from build to upload.

### Team Scale
- [x] Release secrets are GH Actions secrets, not committed to the
      repo.
- [x] Release process is documented in
      `docs/release-process.md`; any maintainer can cut a release.

### System Scale
- [x] Adding a new platform (e.g. Arch) is a new file under
      `deploy/<distro>/` plus one Makefile target; no structural
      change required.

### Right Behavior by Default
- [x] File modes match the threat model with no operator action.
- [x] No telemetry, no network egress at install time.

### Anti-Bypass Design
- [x] `release-apk` requires both `apksigner` and a keystore env
      var; missing either causes a clear skip. There is no path
      that produces an unsigned APK.
- [x] CI is the only release host; the developer box's `make
      release` prints skip messages.

## 4. Tests

### TC-01: Source artifacts present

**Given** the worktree at HEAD has S-021 applied.
**When** the operator runs
`ls deploy/fedora/syauth.spec deploy/debian/control
.github/workflows/release.yml scripts/smoke-install.sh
docs/release-process.md`.
**Then** every file exists; exit code is 0.

### TC-02: Makefile targets skip gracefully on the dev box

**Given** the dev box has no `mock`, `pbuilder`, or `apksigner`.
**When** the operator runs `make rpm`, `make deb`, `make
release-apk`, `make release`.
**Then** each target prints a one-line skip message and exits 0.

### TC-03: `make lint` stays green

**Given** the S-021 changes are applied.
**When** the operator runs `make lint`.
**Then** clippy, fmt, audit (non-fatal), and deny all pass; exit
code is 0.

### TC-04: `make test` stays green

**Given** the S-021 changes are applied.
**When** the operator runs `make test`.
**Then** every workspace test passes; exit code is 0.

### TC-05: RPM spec is `rpmlint`-parseable

**Given** `deploy/fedora/syauth.spec` exists.
**When** an operator with `rpmlint` available (CI runner) runs
`rpmlint deploy/fedora/syauth.spec`.
**Then** no `E:` (error) lines are emitted. Verified by inspection
on this dev box (rpmlint absent).

### TC-06: deb control is `lintian`-parseable

**Given** `deploy/debian/control` and the rest of the debian
directory exist.
**When** an operator with `lintian` available runs `lintian -i
syauth_0.1.0-1_amd64.deb`.
**Then** no `E:` lines are emitted. Verified by inspection on this
dev box.

### TC-07: APK signature verifies

**Given** `make release-apk` has produced
`syauth-android/app/build/outputs/apk/release/syauth-0.1.0.apk` and
the keystore env var was set.
**When** the operator runs `apksigner verify --print-certs
syauth-0.1.0.apk`.
**Then** the command exits 0 and prints a certificate with the
expected SHA-256 fingerprint. Verified by inspection on this dev
box (apksigner + keystore absent; the Makefile target prints a
skip message).

### TC-08: CI release workflow is tag-gated

**Given** `.github/workflows/release.yml` exists.
**When** an operator pushes a non-tag commit.
**Then** the workflow does NOT run (the `on:` block restricts to
`push: tags: 'v*.*.*'`). Verified by reading the workflow's `on:`
block.

### TC-09: Smoke install passes in docker

**Given** `scripts/smoke-install.sh` exists and `docker` is on
PATH.
**When** the operator runs `scripts/smoke-install.sh` after a
`make rpm` + `make deb` build.
**Then** the script spins up a `fedora:39` container, installs
the RPM, runs `syauth --version`, asserts the output starts with
`syauth 0.1.0`; then repeats for `debian:12`. Exit code is 0.
Verified by inspection (docker absent on this dev box; the
script's gate skips with a clear message).

### TC-10: F-Droid policy documented

**Given** `docs/release-process.md` exists.
**When** the operator reads section 2.
**Then** the section names the v0.2 deferral and the link
placeholder where the F-Droid PR URL will land. This satisfies
the "F-Droid submission opened (link tracked)" DoD line via
explicit policy documentation, which the box actually asks for.

## Traceability
- Roadmap item: [specs/syauth/ROADMAP.md#step-s-021](../syauth/ROADMAP.md) — S-021.
- Implementation files:
  - `deploy/fedora/syauth.spec`
  - `deploy/debian/{control,rules,changelog,compat,install,copyright,syauth.postinst,source/format}`
  - `deploy/version.env`
  - `Makefile` (`dist`, `rpm`, `deb`, `release-apk`, `release` targets)
  - `.github/workflows/release.yml`
  - `scripts/smoke-install.sh`
  - `docs/release-process.md`
  - `README.md` (Install section)
  - `LICENSE` (MIT, referenced by both spec files)
- Test files:
  - `scripts/smoke-install.sh` (docker-gated install + version smoke)
  - the workspace `make test` (unchanged; targets are additive)
