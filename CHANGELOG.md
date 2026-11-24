# Changelog

All notable changes to Transom are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] — 2026-11-24

The first release.

### Added

- Pixel-exact remote desktops at 100%, 125%, 150%, 166.7% and 200%, through window
  resizes and display scale changes; the remote desktop follows the window size.
- Windows 10/11, Windows Server and gnome-remote-desktop (including remote login)
  servers.
- Saved connections, quick connect and search; `.rdp` files (import, export, open) and
  `rdp://` links on Linux, macOS and Windows.
- Certificate trust: system CAs, then trust on first use; a changed certificate blocks
  the connection.
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
- Packages: Flatpak, .deb, .rpm, Arch, macOS .dmg, Windows installer and .zip, each
  with the licence notices of everything it contains.

### Security

- Connections from `.rdp` files and `rdp://` links ask first, and share neither the
  clipboard nor the microphone unless you turn them on. An imported file can't turn
  off network-level authentication, and an RD Gateway gets your password only when
  the file asks for one.
- The clipboard is shared only while the session has focus, and password managers'
  concealed entries are never offered to the remote computer.
- Limits on the decoding work, file transfers and images a server can ask for.
- When Remote Credential Guard can't be used and Transom connects without it, it
  tells you which session that was.
- Passwords are saved only when you ask (Remember Password is off by default).
- The Windows installer registers `.rdp` files and `rdp://` links only if you choose to.

[Unreleased]: https://github.com/jm2/transom/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jm2/transom/releases/tag/v0.1.0
