// SPDX-License-Identifier: Apache-2.0
//! Local devices made available to the remote session (PLAN Phase 4b): folders as
//! drives, smart cards, printers, serial ports (MS-RDPEFS family) and individual USB
//! devices (MS-RDPEUSB).
//!
//! Every redirection is opt-in, per profile. The server decides what it accepts
//! (policy, role, OS edition). A refusal never fails the connection: the device
//! just isn't there.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What a profile redirects.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Redirections {
    /// Local folders shown as drives (`\\tsclient\NAME` on Windows).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub drives: Vec<DriveShare>,
    /// All local smart card readers (PC/SC).
    pub smartcards: bool,
    /// All local printers (CUPS).
    pub printers: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub serial_ports: Vec<SerialPort>,
    /// USB devices, redirected as raw USB.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub usb: Vec<UsbSelector>,
}

impl Redirections {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveShare {
    /// The name the remote sees (see [`drive_name`]).
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerialPort {
    /// The remote port name, e.g. `COM3`.
    pub name: String,
    /// The local device, e.g. `/dev/ttyUSB0`.
    pub path: PathBuf,
}

/// Which USB device to redirect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "kebab-case")]
pub enum UsbSelector {
    /// Every device with this vendor/product ID, including ones plugged in later.
    VidPid { vid: u16, pid: u16 },
}

/// A name the remote accepts for a drive: MS-RDPEFS `PreferredDosName` is at most 7
/// ASCII characters (plus NUL), so non-alphanumerics become `_` and the name is
/// truncated. Empty input gives `drive`.
pub fn drive_name(wanted: &str) -> String {
    let mut s: String = wanted
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(7)
        .collect();
    if s.trim_matches('_').is_empty() {
        s = "drive".into();
    }
    s
}

/// Problems with a profile's redirections that stop a device being offered at all
/// (checked before connecting; the session goes ahead without the device).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectionProblem {
    MissingFolder(PathBuf),
    MissingPort(PathBuf),
    BadPortName(String),
}

impl std::fmt::Display for RedirectionProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingFolder(p) => write!(f, "folder {} doesn't exist", p.display()),
            Self::MissingPort(p) => write!(f, "serial port {} doesn't exist", p.display()),
            Self::BadPortName(n) => write!(f, "\"{n}\" isn't a COM port name (COM1–COM256)"),
        }
    }
}

/// The redirections that can actually be offered, and why the others can't.
pub fn usable(r: &Redirections) -> (Redirections, Vec<RedirectionProblem>) {
    let mut ok = r.clone();
    let mut problems = Vec::new();
    ok.drives.retain(|d| {
        let good = d.path.is_dir();
        if !good {
            problems.push(RedirectionProblem::MissingFolder(d.path.clone()));
        }
        good
    });
    ok.serial_ports.retain(|p| {
        let name_ok = p
            .name
            .strip_prefix("COM")
            .and_then(|n| n.parse::<u16>().ok())
            .is_some_and(|n| (1..=256).contains(&n));
        if !name_ok {
            problems.push(RedirectionProblem::BadPortName(p.name.clone()));
            return false;
        }
        let exists = p.path.exists();
        if !exists {
            problems.push(RedirectionProblem::MissingPort(p.path.clone()));
        }
        exists
    });
    (ok, problems)
}

// ---- USB ------------------------------------------------------------------------

/// USB base class codes (usb.org) that matter for the policy below.
pub mod usb_class {
    pub const HID: u8 = 0x03;
    pub const HUB: u8 = 0x09;
}

/// A USB device attached to this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDevice {
    pub bus: u8,
    pub address: u8,
    pub vid: u16,
    pub pid: u16,
    /// Device class and the classes of all interfaces of the active configuration.
    pub classes: Vec<u8>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    /// The usbfs node FreeRDP opens (`/dev/bus/usb/BBB/DDD` on Linux).
    pub node: PathBuf,
}

impl UsbDevice {
    pub fn label(&self) -> String {
        let name = match (&self.manufacturer, &self.product) {
            (Some(m), Some(p)) if p.starts_with(m.as_str()) => p.clone(),
            (Some(m), Some(p)) => format!("{m} {p}"),
            (None, Some(p)) => p.clone(),
            (Some(m), None) => m.clone(),
            (None, None) => "USB device".into(),
        };
        format!("{name} ({:04x}:{:04x})", self.vid, self.pid)
    }

    pub fn selector(&self) -> UsbSelector {
        UsbSelector::VidPid {
            vid: self.vid,
            pid: self.pid,
        }
    }
}

/// Why a USB device can't be redirected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbRefusal {
    /// Keyboards, mice and other input devices: redirecting one takes it away from
    /// this computer (and from the session's own input).
    InputDevice,
    Hub,
    /// On the user's deny list.
    Denied,
    /// No write access to the device node (Linux: a udev rule, see the docs).
    NoPermission,
}

impl std::fmt::Display for UsbRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InputDevice => "input devices stay on this computer",
            Self::Hub => "hubs can't be redirected",
            Self::Denied => "on the deny list",
            Self::NoPermission => "no permission to use it (see the USB section of the help)",
        })
    }
}

/// User policy on top of the built-in rules.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsbPolicy {
    /// Never offered, whatever the profile says.
    pub deny: Vec<(u16, u16)>,
}

/// Whether `dev` may be redirected. Input devices and hubs never are.
pub fn usb_redirectable(dev: &UsbDevice, policy: &UsbPolicy) -> Result<(), UsbRefusal> {
    if dev.classes.contains(&usb_class::HID) {
        return Err(UsbRefusal::InputDevice);
    }
    if dev.classes.contains(&usb_class::HUB) {
        return Err(UsbRefusal::Hub);
    }
    if policy.deny.contains(&(dev.vid, dev.pid)) {
        return Err(UsbRefusal::Denied);
    }
    if !writable(&dev.node) {
        return Err(UsbRefusal::NoPermission);
    }
    Ok(())
}

fn writable(node: &Path) -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(node)
        .is_ok()
}

/// The USB devices attached now. Linux reads sysfs; other systems return nothing
/// yet (ADR 0002 seam: IOKit on macOS, SetupAPI on Windows).
pub fn usb_devices() -> Vec<UsbDevice> {
    #[cfg(target_os = "linux")]
    {
        sysfs::devices(Path::new("/sys/bus/usb/devices"), Path::new("/dev/bus/usb"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

/// The devices a profile's selectors match (for the session summary and checks).
pub fn usb_matching(selectors: &[UsbSelector], devices: &[UsbDevice]) -> Vec<UsbDevice> {
    devices
        .iter()
        .filter(|d| selectors.contains(&d.selector()))
        .cloned()
        .collect()
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod sysfs {
    use std::fs;
    use std::path::Path;

    use super::UsbDevice;

    fn read(p: &Path) -> Option<String> {
        fs::read_to_string(p)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn hex16(p: &Path) -> Option<u16> {
        u16::from_str_radix(&read(p)?, 16).ok()
    }

    fn hex8(p: &Path) -> Option<u8> {
        u8::from_str_radix(&read(p)?, 16).ok()
    }

    /// Devices under `root` (`/sys/bus/usb/devices`): entries like `1-2` or `3-1.4`
    /// (interfaces have a `:`), whose usbfs nodes live under `dev_root`.
    pub(super) fn devices(root: &Path, dev_root: &Path) -> Vec<UsbDevice> {
        let Ok(rd) = fs::read_dir(root) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.contains(':') || name.starts_with("usb") {
                continue; // interfaces, root hubs
            }
            let d = e.path();
            let (Some(vid), Some(pid), Some(bus), Some(address)) = (
                hex16(&d.join("idVendor")),
                hex16(&d.join("idProduct")),
                read(&d.join("busnum")).and_then(|s| s.parse().ok()),
                read(&d.join("devnum")).and_then(|s| s.parse().ok()),
            ) else {
                continue;
            };
            let mut classes: Vec<u8> = hex8(&d.join("bDeviceClass")).into_iter().collect();
            if let Ok(ifs) = fs::read_dir(&d) {
                for i in ifs.flatten() {
                    if i.file_name()
                        .to_string_lossy()
                        .starts_with(&format!("{name}:"))
                    {
                        classes.extend(hex8(&i.path().join("bInterfaceClass")));
                    }
                }
            }
            classes.sort_unstable();
            classes.dedup();
            out.push(UsbDevice {
                bus,
                address,
                vid,
                pid,
                classes,
                manufacturer: read(&d.join("manufacturer")),
                product: read(&d.join("product")),
                node: dev_root
                    .join(format!("{bus:03}"))
                    .join(format!("{address:03}")),
            });
        }
        out.sort_by_key(|d| (d.bus, d.address));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_names_fit_dos_names() {
        assert_eq!(drive_name("Home"), "Home");
        assert_eq!(drive_name("My Documents"), "My_Docu");
        assert_eq!(drive_name("ü/.."), "drive");
        assert_eq!(drive_name(""), "drive");
    }

    fn dev(classes: &[u8]) -> UsbDevice {
        UsbDevice {
            bus: 1,
            address: 2,
            vid: 0x1234,
            pid: 0x5678,
            classes: classes.to_vec(),
            manufacturer: Some("ACME".into()),
            product: Some("ACME Stick".into()),
            node: PathBuf::from("/nonexistent"),
        }
    }

    #[test]
    fn usb_policy_keeps_input_devices_and_hubs_local() {
        let p = UsbPolicy::default();
        assert_eq!(
            usb_redirectable(&dev(&[0, 3]), &p),
            Err(UsbRefusal::InputDevice)
        );
        assert_eq!(usb_redirectable(&dev(&[9]), &p), Err(UsbRefusal::Hub));
        // A storage device that isn't accessible here: permission, not a policy refusal.
        assert_eq!(
            usb_redirectable(&dev(&[0, 8]), &p),
            Err(UsbRefusal::NoPermission)
        );
        let deny = UsbPolicy {
            deny: vec![(0x1234, 0x5678)],
        };
        assert_eq!(usb_redirectable(&dev(&[8]), &deny), Err(UsbRefusal::Denied));
        assert_eq!(dev(&[8]).label(), "ACME Stick (1234:5678)");
    }

    #[test]
    fn unusable_redirections_are_dropped_with_reasons() {
        let r = Redirections {
            drives: vec![DriveShare {
                name: "x".into(),
                path: "/nonexistent/folder".into(),
            }],
            serial_ports: vec![
                SerialPort {
                    name: "LPT1".into(),
                    path: "/dev/null".into(),
                },
                SerialPort {
                    name: "COM3".into(),
                    path: "/dev/null".into(),
                },
            ],
            ..Default::default()
        };
        let (ok, problems) = usable(&r);
        assert!(ok.drives.is_empty());
        assert_eq!(ok.serial_ports.len(), 1);
        assert_eq!(problems.len(), 2);
    }

    #[test]
    fn sysfs_enumeration() {
        let t = std::env::temp_dir().join(format!("transom-usb-{}", std::process::id()));
        let d = t.join("1-2");
        std::fs::create_dir_all(d.join("1-2:1.0")).unwrap();
        std::fs::create_dir_all(t.join("1-2:1.0")).unwrap();
        for (f, v) in [
            ("idVendor", "0781"),
            ("idProduct", "5581"),
            ("busnum", "1"),
            ("devnum", "7"),
            ("bDeviceClass", "00"),
            ("product", "Ultra"),
            ("manufacturer", "SanDisk"),
        ] {
            std::fs::write(d.join(f), v).unwrap();
        }
        std::fs::write(d.join("1-2:1.0/bInterfaceClass"), "08").unwrap();
        let devs = sysfs::devices(&t, Path::new("/dev/bus/usb"));
        std::fs::remove_dir_all(&t).unwrap();
        assert_eq!(devs.len(), 1);
        assert_eq!(devs[0].classes, vec![0, 8]);
        assert_eq!(devs[0].node, PathBuf::from("/dev/bus/usb/001/007"));
        assert_eq!(devs[0].label(), "SanDisk Ultra (0781:5581)");
    }
}
