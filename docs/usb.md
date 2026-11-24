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

`uaccess` gives access to the user of the *active local session* at the seat. If
you use Transom from a session that isn't the active one at the seat (a remote
desktop, or with the login screen showing locally), give the access to a system
group you're in instead, for example `MODE="0660", GROUP="dialout"` (udev deprecates
personal groups for device nodes). A one-off `setfacl` on the device node doesn't
last: Transom resets the device when the session ends, and udev then applies its
rules again.

Devices without access aren't redirected; the connection goes ahead without them.

**Flatpak:** USB redirection needs raw device access, which the Flatpak doesn't
have by default: `flatpak override --user --device=all io.github.jm2.Transom`. The
udev rule above is still needed.

## The remote side

- **Windows:** Windows 10/11 Pro/Enterprise accept redirected USB devices, and so does
  Windows Server with the RD Session Host role. The policy "Do not allow supported
  Plug and Play device redirection" controls it. On Windows 11 it must be set to
  **Disabled** explicitly: left at "Not configured", Windows doesn't offer USB
  redirection at all (found with Windows 11 Enterprise 25H2).
- **The remote computer needs a driver** for the device, as if it were plugged in
  there. Without one the device shows in Device Manager with "no driver installed".
- **Other servers:** gnome-remote-desktop and xrdp don't support it.
- **When the server refuses it:** the device simply stays on this computer, and the
  session is unaffected.
- **Prefer the dedicated channels where they exist:** drives, smart cards, printers,
  cameras and audio each have one, and they work better than raw USB for those devices.
