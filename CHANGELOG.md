# Changelog

All notable changes to Transom are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1] — 2026-11-25

A security release: update from 0.1.0, the AppImage above all.

### Security

- The AppImage bundles FreeRDP 3.33.1, built from the same pinned source, patches and
  options as the Flatpak, instead of Debian 13's FreeRDP 3.15, which has many known
  vulnerabilities a malicious server or RD Gateway can exploit. Like the Flatpak, it
  decodes H.264 only on the GPU, and it gains Remote Credential Guard. The .deb, .rpm
  and Arch packages require FreeRDP 3.33.1 or later: on Debian 13 the .deb needs
  FreeRDP from trixie-backports (see the README); on Ubuntu, use the AppImage or the
  Flatpak.
- Passwords are sent only with Network Level Authentication, unless a connection's
  security is set to TLS Only or Allow Legacy RDP Security. When a server declines
  NLA, Transom asks first and explains the risk, instead of sending the saved
  password. Connections from files and links, and links pasted into Quick Connect,
  require NLA and ask before connecting.
- Clipboard: the remote computer can read your clipboard only while the session (in
  multi-monitor mode, any of its windows) has focus. On GNOME (Wayland) and macOS,
  0.1.0 could let a server read what you copied in other apps after you had once
  pasted into the session. What a server sends after you've switched away no longer
  replaces your clipboard; it's applied when you come back.
- Clipboard on macOS: entries that password managers mark as concealed or transient
  are recognised, and are neither offered nor sent to the remote computer.
- Clipboard files: symbolic links and special files in a copied folder aren't offered,
  a file replaced after the copy isn't read, and reads stop at the size that was
  offered. Offered files stop being readable once you copy something else, and files
  copied from a server can't be fetched after reconnecting. Data from a server is
  size-checked before it's copied, and its own clipboard content is never offered back
  to it.
- Server redirections no longer bypass certificate checks, and credential prompts name
  the computer that is actually asking.
- Importing an `.rdp` file no longer keeps passwords or access tokens in the profile or
  in exported files, and profiles saved by 0.1.0 are cleaned. Settings that would
  change where mstsc connects or what it runs there aren't exported either.
- Computer and gateway names must be host names or IP addresses (FreeRDP reads some
  other strings as local sockets or open files).
- Graphics: video layers are dropped whenever the graphics channel closes or the
  server redirects, so they can't pile up, and they're clipped to the remote desktop.
  A desktop larger than Transom supports ends the session instead of exhausting memory.
  Region and damage bookkeeping is bounded, and screen updates waiting for a busy UI
  are merged.
- Logging in with Remember Password off forgets a saved password.
- On Windows, DLLs load only from Transom's folder and the system's.
- The README no longer suggests a plain `--appimage-extract-and-run`, which unpacks
  into a shared temporary directory.
- Dependabot no longer merges GitHub Actions updates by itself.

### Changed

- The connection editor warns while Allow Legacy RDP Security or TLS Only is chosen.

## [0.1.0] — 2026-11-24

The first release.

### Added

- Pixel-exact remote desktops at 100%, 125%, 150%, 166.7% and 200%, through window
  resizes and display scale changes; the remote desktop follows the window size.
- Windows 10/11, Windows Server and gnome-remote-desktop (including remote login)
  servers.
- Saved connections, quick connect and search; `.rdp` files (import, export, open) and
  `rdp://` links on Linux, macOS and Windows.
- Certificate trust: system CAs, then trust on first use; a changed certificate is
  refused unless you explicitly accept it after a warning.
- Passwords in the system keyring (Secret Service, Keychain, Credential Manager).
- Tabs (detachable), fullscreen with keyboard shortcuts sent to the remote, multiple
  monitors.
- Clipboard with text, HTML, images and files; remote audio and microphone.
- RD Gateway (HTTP, RPC over HTTP, WebSocket); Kerberos single sign-on and Remote
  Credential Guard.
- Folders as drives, printers, smart cards and (on Linux) USB devices.
- H.264 graphics, with zero-copy hardware decoding on Linux (VA-API and GTK's Vulkan
  renderer).
- Screen reader labels and keyboard navigation.
- The transom-view GTK 4 widget, with a C API, GObject Introspection and Vala bindings.
- Packages: Flatpak, AppImage (glibc 2.39 and later, so Ubuntu 24.04 too), .deb, .rpm,
  Arch, macOS .dmg, Windows installer and .zip, each with the licence notices of
  everything it contains.

### Security

- Connections from `.rdp` files and `rdp://` links ask first, and share neither the
  clipboard nor the microphone unless you turn them on. An imported file can't turn
  off network-level authentication, and an RD Gateway gets your password only when
  the file asks for one.
- Copies are offered to the remote computer only while the session has focus, and
  entries a password manager marks as concealed are neither offered nor sent to it (on
  Linux and macOS).
- Limits on the decoding work, file transfers and images a server can ask for.
- When Remote Credential Guard can't be used and Transom connects without it, it
  tells you which session that was.
- Passwords are saved only when you ask (Remember Password is off by default).
- The Windows installer registers `.rdp` files and `rdp://` links only if you choose to.
- The Flatpak can't reach the host's smart-card (pcscd) or printer (CUPS) services until
  you grant them; a connection that shares those devices names the `flatpak override`
  command. Devices a connection can't share (a missing folder, a missing permission) are
  reported when it connects.
- The AppImage, macOS and Windows packages bundle FreeRDP, FFmpeg and OpenSSL; they are
  rebuilt and released when those publish security fixes ([SECURITY.md](https://github.com/jm2/transom/blob/main/SECURITY.md)).
- Known limitation: the RDP engine (FreeRDP, FFmpeg) runs inside Transom's process, and
  only the Flatpak sandbox contains it; a sandboxed helper process is planned
  ([docs/security.md](https://github.com/jm2/transom/blob/main/docs/security.md),
  [#18](https://github.com/jm2/transom/issues/18)).

[Unreleased]: https://github.com/jm2/transom/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/jm2/transom/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/jm2/transom/releases/tag/v0.1.0
