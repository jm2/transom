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
  which RD Gateway).
- Such connections share neither the clipboard nor the microphone unless you turn them
  on in that dialog, and never devices: the settings that grant local resources
  (`drivestoredirect`, `redirectsmartcards`, `usbdevicestoredirect`, …) are not taken
  from a file, and not written to an exported one.
- A file can't turn off network-level authentication or certificate checks, and an RD
  Gateway gets your password only when the file asks for one.

## Certificates and credentials

- Certificates are checked against the system's certificate authorities, then trusted
  on first use. A certificate that changes is refused unless you explicitly accept it:
  the warning's default and close actions cancel, and accepting is styled as the
  dangerous choice.
- Passwords go to the system keyring (Secret Service, Keychain or Credential Manager)
  only when you ask: Remember Password is off by default. They are never passed on a
  command line or in the environment.
- The clipboard is shared only while the session has focus. Entries a password
  manager marks as concealed (with KDE's or macOS's clipboard markers) are neither
  offered nor sent to the remote computer, including when one is copied while the
  remote is reading the clipboard; entries without such a marker can't be told apart.

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
inside the Transom process. So do GTK and GDK-Pixbuf, which decode the images a server
puts on the clipboard. A memory-safety bug a server can trigger in any of them runs
with Transom's privileges, next to your saved passwords and certificate trust.

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
| .deb, .rpm, Arch | The distribution | With the distribution's updates |
| Flatpak | FreeRDP and pcsc-lite built from pinned sources; FFmpeg from the runtime's codecs-extra extension, the rest from the GNOME runtime | The runtime with Flathub's updates; FreeRDP and pcsc-lite with a Transom release |
| AppImage, macOS, Windows | Bundled from Debian 13, Homebrew and MSYS2 | With a Transom release |

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
