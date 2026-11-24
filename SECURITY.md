# Security policy

## Reporting a vulnerability

Please report security problems privately, through GitHub's private vulnerability
reporting: the repository's **Security** tab, **Report a vulnerability**
(<https://github.com/jm2/transom/security/advisories/new>). If that isn't available,
open an issue that asks for a private contact, without details of the problem.

Include the Transom version and package (Flatpak, AppImage, .deb, …), what an attacker
needs (a server you connect to, a file or link you open, a position on the network),
and how to reproduce it. A problem in FreeRDP, FFmpeg or another library is welcome
too: Transom will help get it to that project and ship the fix.

## Supported versions

The latest release. Fixes ship as a new release.

## Bundled libraries

The AppImage, macOS and Windows packages contain their own FreeRDP, FFmpeg, OpenSSL
and other libraries (from Debian 13, Homebrew and MSYS2), and the Flatpak builds
FreeRDP from a pinned source, so none of them gets your distribution's security
updates. When FreeRDP, FFmpeg, OpenSSL or another library that handles what a server
sends publishes a security fix, Transom rebuilds and releases these packages. The
weekly [Bundled libraries](.github/workflows/bundled-libraries.yml) workflow opens an
issue when their versions move. The .deb, .rpm and Arch packages use the system's
libraries and get fixes from the distribution.

## Security model

[docs/security.md](docs/security.md) describes what Transom protects, how, and where
it doesn't yet: the RDP engine runs inside Transom's process, and only the Flatpak
sandbox contains it ([#18](https://github.com/jm2/transom/issues/18)).
