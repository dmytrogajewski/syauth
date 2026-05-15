# syauth — phone-as-key unlock for Linux

syauth is a Linux PAM module plus Android companion app that replaces
typing your password for `sudo`, login, and lockscreen with an
approve-tap on your paired phone. The phone's signing key is unlocked
by a fresh biometric per unlock, defeating the NCC link-layer relay
class of attacks that broke every prior passive BLE PAM module. When
the phone is dead or out of range, syauth steps aside and the
configured fallback (typically `pam_unix`) takes over.

Status: **v0.1.0 release candidate.** See
[`specs/syauth/SPEC.md`](specs/syauth/SPEC.md) for the protocol and
[`docs/security.md`](docs/security.md) for the threat model.

## Install

Pick the install command for your platform. Each is one line; the
package manager resolves the runtime dep closure.

### Fedora 39+

```
sudo dnf install \
  https://github.com/sy/syauth/releases/download/v0.1.0/syauth-0.1.0-1.fc39.x86_64.rpm
```

### Debian 12 / Ubuntu 22.04 LTS+

```
sudo apt install \
  https://github.com/sy/syauth/releases/download/v0.1.0/syauth_0.1.0-1_amd64.deb
```

### Android 8+

1. Download `syauth-0.1.0.apk` from the
   [v0.1.0 release page](https://github.com/sy/syauth/releases/tag/v0.1.0).
2. Side-load via `adb install syauth-0.1.0.apk` (preferred — leaves
   an audit trail) or tap the APK in your file manager after
   enabling `Settings → Apps → Special access → Install unknown
   apps` for the source.

F-Droid is a planned v0.2 delivery; tracked in
[`docs/release-process.md`](docs/release-process.md).

### Verify

Before installing, audit the signature on every artifact:

```
rpm --checksig syauth-0.1.0-1.fc39.x86_64.rpm
dpkg --verify syauth   # after install
apksigner verify --print-certs syauth-0.1.0.apk
```

The signing-key fingerprints are published on the release page. A
fingerprint change between point releases is a stop-the-line signal
— verify against multiple channels before proceeding.

## Quick start

After the desktop package is installed and the Android app is
running:

```
syauth pair             # on the desktop
# Follow the on-screen OOB confirmation on both ends
syauth list             # confirm the phone is bonded
sudo cat /dev/null      # first unlock — phone prompts for approval
```

If anything goes wrong, `syauth status` reports adapter state, bonded
peers, and the last error.

## Documentation

- [`specs/syauth/SPEC.md`](specs/syauth/SPEC.md) — protocol design,
  wire format, install layout.
- [`docs/security.md`](docs/security.md) — end-user threat model.
- [`docs/android-setup.md`](docs/android-setup.md) — Android side.
- [`docs/release-process.md`](docs/release-process.md) — release
  engineering reference.
- [`specs/threat/THREAT-2026-05-15.md`](specs/threat/THREAT-2026-05-15.md)
  — formal v0.1 threat-model close-out.

## License

[MIT](LICENSE). Copyright 2026 syauth contributors.
