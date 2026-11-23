# Known issues in servers and dependencies

Problems observed in software Transom talks to. Each has a reproduction, so it can
be reported upstream and re-checked.

## gnome-remote-desktop 51: non-ASCII client clipboard text is escaped

- Seen: 2026-11-21, grd 51.0 (Fedora 45, FreeRDP/WinPR 3.33.1), headless mode.
- Client → server text containing `✓` (U+2713) arrives in the session as the six
  characters `✓`. grd's own `text/plain;charset=utf-8` stream contains
  `5c 75 32 37 31 33`, so the conversion happens in grd (or the WinPR code it uses),
  not in GTK or mutter. ASCII and newlines are fine, and server → client Unicode
  is correct.
- Transom sends correct `CF_UNICODETEXT` (UTF-16LE, CRLF, NUL-terminated; covered by
  unit and property tests).
- Repro: `scripts/test-servers.sh up && scripts/clipboard-check.sh`. The probe logs a
  `paste-raw` hex dump.

## gnome-remote-desktop: Unicode keyboard input limited to the session keymap

- grd injects `TS_UNICODE_KEYBOARD_EVENT` as keysyms through mutter, which can only
  produce keysyms in the active keymap. With a US keymap, `é` is dropped.
  Transom only uses Unicode input as a fallback for keys without a scancode.
- Repro: `scripts/input-check.sh` (it treats `eacute` as optional).

## gnome-remote-desktop in containers needs FUSE

- grd aborts when a client enables clipboard redirection and it can't mount its FUSE
  clipboard filesystem (`[FUSE Clipboard] Failed to mount FUSE filesystem`). The test
  container runs with `--device /dev/fuse --cap-add SYS_ADMIN` (namespaced to the
  rootless container).

## gnome-remote-desktop: clipboard images only as BMP ⇄ CF_DIB

- grd converts clipboard images only between `image/bmp` in the session and `CF_DIB`
  on the wire. A GTK app that copies a texture offers PNG/TIFF, so grd announces
  nothing and the image never reaches the client. Windows servers offer `CF_DIB`
  directly. Transom handles DIB (and PNG) both ways; `scripts/clipboard-check.sh`
  verifies this with an `image/bmp` copy in the session.
- Test-environment note: in rootless containers, GTK's sandboxed image loader (glycin
  with bubblewrap) can't start, so the probe reads raw `image/bmp` bytes instead of
  decoding them.

## Windows Server 2025: microphone redirection not opened

Against the Server 2025 test VM, audio playback reaches the client, but the server
never opens the `AUDIO_INPUT` dynamic channel, so applications in the session cannot
record (the same client against Windows 11 25H2 records fine: `TARGET=win11
scripts/audio-check.sh`). In "Remote Desktop for Administration" mode no capture
endpoint exists at all; with the RD Session Host role installed the endpoint appears
but `waveInOpen` fails (`MMSYSERR_ERROR`) because the channel is never created.
Checked: policy and RDP-Tcp `fDisableAudioCapture = 0`, Windows Audio running,
Server-Media-Foundation installed, fresh session. Not yet compared with mstsc
against the same server, so it may still be a server-side setting.

## Kerberos to Windows: the interactive logon needs MS-RDPEAR

With only a Kerberos ticket and no password, NLA authenticates fine, but Windows also
needs a credential to log the user on to the desktop. Without a password, the only
way to provide one is Remote Credential Guard: the client delegates its ticket, and
Windows sends later Kerberos requests back over the MS-RDPEAR channel.
- **FreeRDP builds without the channel (Fedora's, as of 3.33).** Upstream switches it off by
  default (`-DCHANNEL_RDPEAR=ON`). Transom detects this and connects without Remote
  Credential Guard: the session opens at Windows' logon screen, where the user can
  sign in with a password. The Flatpak builds FreeRDP with the channel.
  `scripts/build-freerdp-rdpear.sh` builds a matching library for the tests.
- **Windows host policy.** The target must allow Remote Credential Guard
  (`HKLM\System\CurrentControlSet\Control\Lsa\DisableRestrictedAdmin = 0`, or the GPO
  "Remote host allows delegation of non-exportable credentials"). Microsoft's own
  client needs the same setting. Otherwise Windows accepts the request and then shows
  "Account restrictions are preventing this user from signing in" at its logon screen.
  The user can still sign in there with a password.
- **Servers that don't support it at all** (gnome-remote-desktop) drop the connection at
  negotiation when Remote Credential Guard is requested. Transom retries once without it, and
  Kerberos to grd then works as before (`scripts/kerberos-check.sh`, with both FreeRDP
  builds).

## Zero-copy video: when it applies

Hardware H.264 (ADR 0006) needs each of these, and falls back to software otherwise:
- **A full libavcodec.** Fedora's `libavcodec-free` has no native H.264 decoder; RPM
  Fusion's `libavcodec-freeworld` does. In the Flatpak, the runtime's codecs-extra
  extension provides it.
- **A VA-API driver with H.264.** Fedora's Mesa build leaves it out; RPM Fusion's
  `mesa-va-drivers-freeworld` and Intel's media driver include it.
- **GTK's Vulkan renderer**, the default where Vulkan works. Under
  `GSK_RENDERER=gl`, Transom stays in software, because GTK's GL renderer can't
  import VA-API's tiled frames.
- **A server that sends AVC420.** Windows uses AVC420 only for regions it detects as
  video. Its whole-desktop H.264 is AVC444, which is decoded in software. xrdp sends
  AVC420.

## Serial ports: Windows accepts the redirected port but doesn't expose it

`scripts/devices-check.sh` redirects a pty as COM9. FreeRDP announces it, and both
Windows 11 25H2 and Server 2025 accept it (device reply status 0). But no application
in the session can open it: `mode COM9` says "Device COM9 is not currently available",
and no open request ever reaches the client. Drives and printers on the same channel
work. Not yet compared with mstsc and a physical COM port.

## USB: plugging in a device Transom can't open crashes it (FreeRDP 3.x)

FreeRDP's USB channel crashes the client when it fails to open a device: `udev_free`
runs on the `udev_init` failure path before the request queue exists (still the case
in FreeRDP master, November 2026).

- **At connect:** Transom checks the devices attached now and leaves out any it
  can't open, so this doesn't happen.
- **During the session:** a matching device plugged in that Transom has no access
  to still triggers it with a distribution's FreeRDP. The Flatpak carries a fix
  (`build-aux/flatpak/freerdp-urbdrc-open-failure.patch`). Give access before plugging
  devices in (docs/usb.md, Permissions).
