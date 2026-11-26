# Security

What Transom protects, from whom, and where it falls short. Report problems as
[SECURITY.md](../SECURITY.md) describes.

## What it defends against

- **Rogue `.rdp` files and `rdp://` links**, a real phishing technique: a file can name
  any server, and ask for your drives, smart cards or clipboard.
- **An attacker on the network** who wants to impersonate a server or read the session.
- **A hostile or compromised server**, which controls everything the RDP engine parses.

## Files and links

- Opening a file or link asks before connecting, and shows where it goes (and through
  which RD Gateway). So does a link pasted into Quick Connect.
- Such connections share neither the clipboard nor the microphone unless you turn them
  on in that dialog, and never devices: the settings that grant local resources
  (`drivestoredirect`, `redirectsmartcards`, `usbdevicestoredirect`, …) are not taken
  from a file, and not written to an exported one.
- Connections from files and links require Network Level Authentication (NLA): the
  server must prove it knows your password before it receives it. A server without
  NLA can be reached by adding it as a connection and choosing TLS.
- A file can't turn off certificate checks. An RD Gateway the file names gets your
  credentials only when the file asks for that, and the dialog says so. (It signs you
  in with NTLM or Kerberos, so it doesn't receive the password itself.)
- Computer and gateway names must be host names or IP addresses. (FreeRDP reads some
  other strings as local socket paths or open file descriptors.)
- Passwords and access tokens in a file (`password`, `gatewayaccesstoken`, …) are
  neither kept nor exported, and neither are settings that change where mstsc would
  connect or what it would run there without showing it (`alternate full address`,
  RemoteApp programs).

## Certificates and credentials

- Certificates are checked against the system's certificate authorities. Any other
  certificate needs your decision, which you can have Transom remember; a remembered
  certificate that changes is refused unless you explicitly accept it: the warning's
  default and close actions cancel, and accepting is styled as the dangerous choice.
  A server that redirects the connection to another computer can't vouch for that
  computer's certificate; it's checked like any other.
- A saved or entered password is sent only with NLA, unless the connection's security
  is set to TLS Only or Allow Legacy RDP Security. When a server declines NLA, Transom
  asks first and says why: a server, or someone impersonating it, can decline it to
  have the password sent directly. Legacy RDP security has no certificate at all, so
  anyone on the network can read a connection that allows it.
- Passwords go to the system keyring (Secret Service, Keychain or Credential Manager)
  only when you ask: Remember Password is off by default, and logging in with it off
  forgets a saved one. They are never passed on a command line or in the environment.
- FreeRDP's packet traces (`WLOG_LEVEL=TRACE`) include the password: don't enable them
  for a connection you care about.
- The remote computer can read your clipboard only while the session has focus (in
  multi-monitor mode, any of its windows), and only what was offered to it: what you
  copy in other apps is offered when you come back to the session, not before. The
  one exception is the files of a copy you offered: the remote can go on reading
  those while you work elsewhere, so that a paste in progress finishes, but not files
  you copied later. Its copies reach your clipboard only while the session has focus,
  too.
- Entries a password manager marks as concealed are neither offered nor sent to the
  remote computer, including one copied while the remote is reading the clipboard.
  Transom recognises KDE's marker on Linux, and on macOS the concealed and transient
  markers, which it reads from the pasteboard itself.
- On Windows, password managers' markers aren't recognised yet, and a copy made while
  the remote is reading the clipboard can slip through to it
  ([#21](https://github.com/jm2/transom/issues/21)).

## Devices

Folders, printers, smart cards, serial ports and USB devices are off by default, and
each connection lists its own. In the Flatpak, smart cards, printers and USB need a
permission you grant once, then restart Transom. For smart cards and printers, Transom
names the command when a connection shares them; USB has its own setup
([usb.md](usb.md)):

```sh
flatpak override --user --socket=pcsc io.github.jm2.Transom     # smart cards
flatpak override --user --socket=cups io.github.jm2.Transom     # printers
flatpak override --user --device=all io.github.jm2.Transom      # USB (docs/usb.md)
```

These permissions apply to every connection, not just the one that asked.

## Where the RDP engine runs: the main limitation

FreeRDP (the protocol, its codecs and every channel: clipboard, drives, printers,
smart cards, USB, audio), FFmpeg's H.264 decoder and VA-API decoding run on a thread
inside the Transom process. So does GTK, though little of what a server sends reaches
its parsers: a PNG a server puts on the clipboard is passed undecoded to the
application you paste it into (bitmaps are converted by Transom's own Rust code), and
reaches GDK's PNG loader (libpng) only on read-back, when Transom offers it to another
session's server. A memory-safety bug a server can trigger in any of them runs with
Transom's privileges, next to your saved passwords and certificate trust.

| Package | What contains an exploited engine |
|---|---|
| Flatpak | The sandbox: no home directory or host files (folders only through the file chooser), only Transom's own secrets (the Secret portal), device sockets only when you grant them |
| AppImage, .deb, .rpm, Arch, macOS, Windows | Nothing: it runs as you, with your keyring and files |

This is how other Linux RDP clients (Remmina, GNOME Connections) work too. For servers
you don't control or don't trust, use the Flatpak.

**Planned, not yet implemented:** the engine moves into a separate helper process,
sandboxed on Linux (landlock, seccomp, no session bus, device access passed in only
when a connection uses it), then on macOS and Windows. An exploited engine would then
hold one session, not your account. [#18](https://github.com/jm2/transom/issues/18)
tracks the work.

## Bundled libraries

| Package | FreeRDP, FFmpeg, OpenSSL, … come from | Security fixes arrive |
|---|---|---|
| .deb, .rpm, Arch | The distribution; FreeRDP 3.33.1 or later is required, as older releases have known security problems a server can exploit (Debian 13: from trixie-backports) | With the distribution's updates (Debian 13: FreeRDP's through trixie-backports) |
| Flatpak | FreeRDP and pcsc-lite built from pinned sources; FFmpeg from the runtime's codecs-extra extension, the rest from the GNOME runtime | The runtime with Flathub's updates; FreeRDP and pcsc-lite with a Transom release |
| AppImage | FreeRDP built from the Flatpak's pinned source, with its patches and options; the rest bundled from Debian 13 | With a Transom release |
| macOS, Windows | Bundled from Homebrew and MSYS2 | With a Transom release |

Transom rebuilds and releases when one of these libraries publishes a security fix
([SECURITY.md](../SECURITY.md)); a weekly workflow
([bundled-libraries.yml](../.github/workflows/bundled-libraries.yml), with
`scripts/bundled-versions.py`) opens an issue when their versions move.

## The engine's `unsafe` code

Transom's Rust code denies `unsafe` everywhere except where it calls C: the FreeRDP
engine (`crates/transom-view-freerdp`), the C API and the macOS glue. The FreeRDP crate
has about 200 `unsafe` blocks and functions, written with AI assistance; a line-by-line
human review is still to come. [unsafe-review.md](unsafe-review.md) is its guide: every
site, ordered by how directly a server's data reaches it.
