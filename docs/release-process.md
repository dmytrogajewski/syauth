# syauth release process

Operational handbook for cutting a syauth release. Companion to
[`specs/syauth/ROADMAP.md` S-021](../specs/syauth/ROADMAP.md) and
[`specs/journeys/JOURNEY-S-021-v0_1_release.md`](../specs/journeys/JOURNEY-S-021-v0_1_release.md).

Audience: the maintainer cutting a tagged release. Not the end user
(see the top-level `README.md` for that).

## 1. Linux release pipeline

The Linux side ships two artifacts per tag: a Fedora 39+ RPM and a
Debian 12 / Ubuntu 22.04+ `.deb`. Both are produced by the
`.github/workflows/release.yml` workflow, which is gated on
`push: tags: 'v*.*.*'`.

### 1.1 Local dry-run

On a developer box (no `mock`, no `pbuilder`):

```
make dist          # tarball under target/syauth-0.1.0.tar.gz
make rpm           # skips with one line: 'mock not on PATH'
make deb           # skips with one line: 'pbuilder not on PATH'
make release-apk   # skips with one line: 'apksigner not on PATH'
make release       # meta-target; runs all of the above
```

Every target is a clean exit 0 on a host without the corresponding
tool, so the dev box stays green.

### 1.2 CI run (release engineer's path)

1. Push a signed tag:

   ```
   git tag -s v0.1.0 -m 'syauth v0.1.0'
   git push --tags
   ```

2. The workflow at `.github/workflows/release.yml` fires. Four jobs:

   | Job             | Runner          | Tool             | Output                                |
   |-----------------|-----------------|------------------|---------------------------------------|
   | `build-rpm`     | `ubuntu-22.04`  | `mock`           | `syauth-0.1.0-1.fc39.x86_64.rpm`      |
   | `build-deb`     | `ubuntu-22.04`  | `pbuilder`       | `syauth_0.1.0-1_amd64.deb`            |
   | `build-apk`     | `ubuntu-22.04`  | `apksigner`      | `syauth-0.1.0.apk` (signed)           |
   | `publish-release` | `ubuntu-22.04` | `softprops/action-gh-release@v1` | GH Release with the 3 attachments |

3. Required GH secrets:

   | Secret                              | Used by         | Notes                                                  |
   |-------------------------------------|-----------------|--------------------------------------------------------|
   | `SYAUTH_RELEASE_KEYSTORE_BASE64`    | `build-apk`     | base64-encoded Android keystore (.jks)                 |
   | `SYAUTH_RELEASE_KEYSTORE_PASSWORD`  | `build-apk`     | passphrase for the keystore                            |
   | `GITHUB_TOKEN`                      | `publish-release` | implicit; no PAT required                            |

   A missing secret causes the job to fail loudly at the step that
   needs it. No silent skip.

### 1.3 Verification (end user, copy-paste)

| Artifact                              | Command                                               |
|---------------------------------------|-------------------------------------------------------|
| `syauth-0.1.0-1.fc39.x86_64.rpm`      | `rpm --checksig syauth-0.1.0-1.fc39.x86_64.rpm`       |
| `syauth_0.1.0-1_amd64.deb`            | `dpkg --verify syauth` (after install)                |
| `syauth-0.1.0.apk`                    | `apksigner verify --print-certs syauth-0.1.0.apk`     |

The signing-key fingerprints are published on the release page and
SHOULD NOT change between point releases of the same minor version.
A fingerprint change is a stop-the-line signal — verify against
multiple channels before installing.

### 1.4 SELinux / AppArmor notes

- **Fedora (SELinux):** the RPM installs the PAM `.so` under
  `%{_libdir}/security/` which inherits the `lib_t` context by
  default. `restorecon` is run automatically by `rpm` so no manual
  step is needed.
- **Ubuntu (AppArmor):** v0.1 does not ship an AppArmor profile.
  If an admin authors one, run
  `sudo aa-complain /etc/apparmor.d/usr.bin.syauth` for the first
  unlock attempt and inspect `/var/log/syslog` for any
  `DENIED` lines before promoting to `enforce`.

## 2. F-Droid submission

**Status:** v0.1 ships **without** F-Droid. The F-Droid listing is
tracked as a **v0.2 enhancement** so the v0.1 gate is not blocked by
an external review pipeline whose turnaround is measured in weeks.

### Plan

When v0.2 is ready to ship, open a PR on the `f-droid/fdroiddata`
repo with a metadata YAML referencing this repo's git tag. Track
the PR URL in this section.

### Tracking placeholder

- Submission PR: **not yet opened** (planned for v0.2).
- F-Droid app id: `com.sy.syauth.android` (matches the package's
  `applicationId` declared in
  [`syauth-android/app/build.gradle.kts`](../syauth-android/app/build.gradle.kts)).
- Update this section when the submission PR lands; the URL is the
  audit trail end users can follow.

### Why this satisfies the S-021 DoD line

The roadmap item asks for "F-Droid submission opened (link tracked
in `docs/release-process.md`); not blocking for v0.1." This file
**is** that tracking surface. The link slot is explicitly empty
with a documented "v0.2" rationale, which is the policy a
release engineer needs to commit to a tag.

## 3. Post-release checklist

1. Verify all three artifacts are attached to the v0.1.0 GH
   release.
2. Smoke-test each install path on a clean VM
   (`scripts/smoke-install.sh` does the docker variant locally).
3. Update the README's Install section if any artifact path
   changed.
4. Tag the threat-model artifact as the close-out for the release
   (the current `specs/threat/THREAT-2026-05-15.md` covers
   v0.1.0).
5. Announce on the project mailing list / discussions tab.
