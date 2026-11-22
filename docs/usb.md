# USB device redirection

Transom can hand individual USB devices to the remote computer (MS-RDPEUSB, FreeRDP's
`urbdrc` channel with libusb). While the session runs, the device belongs to the
remote computer: it disappears from this one and comes back when the session ends.

Choose devices per connection under **Devices → USB Devices**. These are never
offered:
- **Keyboards, mice and other input devices** (HID class). Redirecting one would take
  your input away from this computer.
- **USB hubs.**
- **Devices on the deny list.**

A device selected by vendor:product ID is also redirected when plugged in during the
session.

## Permissions (Linux)

libusb needs read-write access to the device node (`/dev/bus/usb/BBB/DDD`). Most
distributions give that only to root. A device without access is listed as
"Unavailable: no permission". To allow one device for whoever is logged in at the
seat, add a udev rule. For example, for an FTDI adapter (`0403:6001`):

```
# /etc/udev/rules.d/70-transom-usb.rules
SUBSYSTEM=="usb", ENV{DEVTYPE}=="usb_device", ATTR{idVendor}=="0403", ATTR{idProduct}=="6001", TAG+="uaccess"
```

Then run `sudo udevadm control --reload && sudo udevadm trigger` (or replug the device).

**Flatpak:** USB redirection needs `--device=all`
(`flatpak override --user --device=all io.github.jm2.Transom`).

## The remote side

- **Windows:** Windows 10/11 Pro/Enterprise accept redirected USB devices, and so does
  Windows Server with the RD Session Host role. The policy "Do not allow supported
  Plug and Play device redirection" turns it off.
- **Other servers:** gnome-remote-desktop and xrdp don't support it.
- **When the server refuses it:** the device simply stays on this computer, and the
  session is unaffected.
- **Prefer the dedicated channels where they exist:** drives, smart cards, printers,
  cameras and audio each have one, and they work better than raw USB for those devices.
