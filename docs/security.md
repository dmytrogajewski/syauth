# syauth Security Model

End-user guide to syauth's security properties. Companion to the
protocol-level threat model at
[`specs/threat/THREAT-2026-05-15.md`](../specs/threat/THREAT-2026-05-15.md).
Written for an operator deciding whether to install syauth on Linux,
not for a cryptographer.

## Should you install syauth?

If you type your `sudo` password 30 times a day and your phone is
usually within arm's reach, syauth replaces that password with a tap
on your phone. The phone still requires a biometric or PIN to approve
the tap, so a stolen phone does not become a key to your desktop
unless the attacker also defeats your phone's lockscreen. The password
remains a fallback: when the phone is dead, far, or uncooperative,
syauth steps aside and the normal password prompt appears.

If your threat model includes a coercive attacker who can force you to
unlock your phone, a nation-state with custom Bluetooth injection
hardware, or root-on-host malware that survives undetected, syauth
does not change your situation. Use a Yubikey-style hardware token
instead.

## What syauth protects against

- **Shoulder surfing your password.** Approving on the phone is
  invisible to a bystander.
- **Password sniffing from a keylogger or evil-USB device.** The
  password never leaves your fingers during a syauth unlock because
  you never type it.
- **Theft of ssh keys via a misconfigured home directory.** syauth's
  secrets live in the kernel keyring on Linux and the
  hardware-backed Android Keystore on the phone; they are never in
  `~/.config` or `~/.ssh`.
- **Opportunistic over-the-shoulder `sudo` attempts.** An attacker
  who finds your terminal unlocked still needs your phone *and* your
  fingerprint to escalate.
- **Replayed unlock attempts within a session.** Every unlock
  carries a fresh 16-byte nonce, and the PAM module's per-call
  replay cache rejects any frame whose nonce it just admitted.

## What syauth does NOT protect against

- **A compromised PAM stack on the host.** If an attacker is already
  root on your desktop they can read syauth's bond key and forge
  unlocks. Detection (`journalctl -t pam_syauth`) and revocation
  (`syauth revoke`) are your only recourse.
- **A coerced biometric prompt.** An attacker who physically forces
  you to put your finger on your phone's sensor unlocks the desktop
  the same way you would. syauth piggybacks on the phone's biometric.
- **Side-channel attacks on the bond key after the phone is
  physically extracted and the Keystore is downgraded.** Use a phone
  with a hardware-backed StrongBox keystore (Pixel 6+ or Samsung
  S22+) if this matters to you.
- **A nation-state with custom BLE injection hardware.** Active
  link-layer jamming and protocol-level fuzzing campaigns are not
  what v0.1 is sized for.
- **A lost phone with an attacker who has your PIN.** Same residual
  as Bitwarden unlock, Authy push, and Apple Auto Unlock.

## Operational hygiene

- **Keep the phone updated.** Android security patches are how the
  Keystore stays trustworthy.
- **Disable USB debugging on the production phone.** ADB plus root
  is a fast path past the Keystore.
- **Use a StrongBox-backed device when possible.** Pixel 6 or newer
  is the easiest recommendation.
- **Audit the bond store.** Run `syauth list` periodically; revoke
  any peer you don't recognize before doing anything else.
- **Rotate bonds when you change phones.** Pair the new phone first,
  verify with `syauth list`, then `syauth revoke <old>`. Do not
  leave a revoked phone bonded.
- **Keep the password fallback in your PAM stack.** The
  `syauth install-pam` helper inserts syauth at `auth required`
  *before* the next module (typically `pam_unix`). Do not change
  `required` to `sufficient` and do not delete the `pam_unix` line.
  Both are foot-guns that either lock you out or weaken the stack.
- **Do not pair in a crowd.** Pairing is the one moment where a
  nearby attacker has a chance to spoof the device-picker. Pair at
  home or in your office.

## What changes between v0.1 and v0.2

The following residual risks have v0.2 candidates tracked in
`specs/syauth/ROADMAP.md` "Out of roadmap (v0.2 candidates)." None
are promised:

- **Wi-Fi RTT or UWB distance bounding** as an optional secondary
  signal addresses relay attacks that synthesise fresh biometrics.
- **Multi-peer racing** lets a user bond multiple phones and unlock
  with whichever responds first.
- **LAN/mDNS fallback transport** for desktops whose Bluetooth
  adapter is broken.
- **iOS port** of the companion app via the same UniFFI surface.

If one of these matters to you, file an issue and link the residual
ID from the threat document.

## Where to look in the source

The full per-threat audit (file paths, line ranges, test names) is in
[`specs/threat/THREAT-2026-05-15.md`](../specs/threat/THREAT-2026-05-15.md).
The high-traffic modules are `crates/syauth-core` (wire format,
replay cache, MAC, signing), `crates/syauth-pam` (PAM entry points
and panic boundary), and `crates/syauth-cli/src/pair.rs` (the
pairing flow with LESC and OOB confirmation).
