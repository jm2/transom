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
