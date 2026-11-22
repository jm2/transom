// SPDX-License-Identifier: GPL-3.0-or-later
//! The connection editor's "Devices" group (PLAN Phase 4b): local folders, smart
//! cards, printers and USB devices offered to the remote computer.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use transom_view::core::devices::{
    DriveShare, Redirections, UsbPolicy, UsbSelector, drive_name, usb_devices, usb_redirectable,
};

/// The group, and the redirections as edited (read them when saving).
pub fn group(initial: &Redirections) -> (adw::PreferencesGroup, Rc<RefCell<Redirections>>) {
    let state = Rc::new(RefCell::new(initial.clone()));
    let group = adw::PreferencesGroup::builder()
        .title("Devices")
        .description("Make devices on this computer available to the remote computer. The remote computer may refuse some of them")
        .build();

    let smartcards = adw::SwitchRow::builder()
        .title("Smart Cards")
        .subtitle("All smart card readers, e.g. for signing in with a card")
        .active(initial.smartcards)
        .build();
    let s = state.clone();
    smartcards.connect_active_notify(move |r| s.borrow_mut().smartcards = r.is_active());
    group.add(&smartcards);

    let printers = adw::SwitchRow::builder()
        .title("Printers")
        .subtitle("All printers on this computer")
        .active(initial.printers)
        .build();
    let s = state.clone();
    printers.connect_active_notify(move |r| s.borrow_mut().printers = r.is_active());
    group.add(&printers);

    group.add(&folders_row(&state));
    group.add(&usb_row(&state));
    (group, state)
}

fn folders_row(state: &Rc<RefCell<Redirections>>) -> adw::ExpanderRow {
    let row = adw::ExpanderRow::builder()
        .title("Shared Folders")
        .subtitle("Shown on the remote computer as drives")
        .build();
    let add = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .valign(gtk::Align::Center)
        .tooltip_text("Add Folder…")
        .build();
    add.add_css_class("flat");
    row.add_suffix(&add);

    let list: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::default();
    let rebuild = Rc::new(RefCell::new(None::<Box<dyn Fn()>>));
    let r = rebuild.clone();
    let st = state.clone();
    let rw = row.downgrade();
    let l = list.clone();
    *rebuild.borrow_mut() = Some(Box::new(move || {
        let Some(row) = rw.upgrade() else { return };
        for old in l.borrow_mut().drain(..) {
            row.remove(&old);
        }
        let drives = st.borrow().drives.clone();
        row.set_subtitle(&match drives.len() {
            0 => "Shown on the remote computer as drives".to_string(),
            1 => "1 folder".to_string(),
            n => format!("{n} folders"),
        });
        for (i, d) in drives.iter().enumerate() {
            let item = adw::ActionRow::builder()
                .title(&d.name)
                .subtitle(d.path.display().to_string())
                .build();
            let remove = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .valign(gtk::Align::Center)
                .tooltip_text("Stop Sharing")
                .build();
            remove.add_css_class("flat");
            let st2 = st.clone();
            let r2 = r.clone();
            remove.connect_clicked(move |_| {
                st2.borrow_mut().drives.remove(i);
                if let Some(f) = r2.borrow().as_ref() {
                    f();
                }
            });
            item.add_suffix(&remove);
            row.add_row(&item);
            l.borrow_mut().push(item);
        }
    }));
    if let Some(f) = rebuild.borrow().as_ref() {
        f();
    }

    let st = state.clone();
    add.connect_clicked(move |b| {
        let dialog = gtk::FileDialog::builder().title("Share Folder").build();
        let parent = b.root().and_downcast::<gtk::Window>();
        let st = st.clone();
        let rebuild = rebuild.clone();
        dialog.select_folder(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
            let Ok(folder) = res else { return };
            let Some(path) = folder.path() else { return };
            add_drive(&mut st.borrow_mut(), path);
            if let Some(f) = rebuild.borrow().as_ref() {
                f();
            }
        });
    });
    row
}

/// Add `path` as a drive named after the folder, unique among the shared folders.
fn add_drive(r: &mut Redirections, path: PathBuf) {
    if r.drives.iter().any(|d| d.path == path) {
        return;
    }
    let base = drive_name(
        &path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    let mut name = base.clone();
    let mut n = 2;
    while r.drives.iter().any(|d| d.name.eq_ignore_ascii_case(&name)) {
        let suffix = n.to_string();
        name = format!("{}{suffix}", &base[..base.len().min(7 - suffix.len())]);
        n += 1;
    }
    r.drives.push(DriveShare { name, path });
}

fn usb_row(state: &Rc<RefCell<Redirections>>) -> adw::ExpanderRow {
    let row = adw::ExpanderRow::builder()
        .title("USB Devices")
        .subtitle("Connected to the remote computer instead of this one while the session runs")
        .build();
    let policy = UsbPolicy::default();
    let devices = usb_devices();
    let selected = state.borrow().usb.clone();
    let mut shown = Vec::new();
    for dev in &devices {
        let sel = dev.selector();
        if shown.contains(&sel) {
            continue; // several identical devices: one entry selects them all
        }
        shown.push(sel);
        let check = gtk::CheckButton::builder()
            .active(selected.contains(&sel))
            .valign(gtk::Align::Center)
            .build();
        let item = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&dev.label()).as_str())
            .activatable_widget(&check)
            .build();
        match usb_redirectable(dev, &policy) {
            Ok(()) => item.set_subtitle(&format!("Bus {} device {}", dev.bus, dev.address)),
            Err(why) => {
                item.set_subtitle(&format!("Unavailable: {why}"));
                // Selected earlier (e.g. permission since lost): leave it selectable
                // so it can be turned off.
                if !check.is_active() {
                    item.set_sensitive(false);
                }
            }
        }
        item.add_prefix(&check);
        let st = state.clone();
        check.connect_toggled(move |c| toggle(&mut st.borrow_mut(), sel, c.is_active()));
        row.add_row(&item);
    }
    // Selected devices that aren't plugged in now: keep them (and allow removing).
    for sel in selected.iter().filter(|s| !shown.contains(s)) {
        let UsbSelector::VidPid { vid, pid } = *sel;
        let check = gtk::CheckButton::builder()
            .active(true)
            .valign(gtk::Align::Center)
            .build();
        let item = adw::ActionRow::builder()
            .title(format!("USB device {vid:04x}:{pid:04x}"))
            .subtitle("Not connected now; redirected when plugged in during a session")
            .activatable_widget(&check)
            .build();
        item.add_prefix(&check);
        let st = state.clone();
        let sel = *sel;
        check.connect_toggled(move |c| toggle(&mut st.borrow_mut(), sel, c.is_active()));
        row.add_row(&item);
    }
    if devices.is_empty() && selected.is_empty() {
        let empty = adw::ActionRow::builder()
            .title(if cfg!(target_os = "linux") {
                "No USB devices found"
            } else {
                "USB redirection isn't available on this system yet"
            })
            .build();
        empty.set_sensitive(false);
        row.add_row(&empty);
    }
    row
}

fn toggle(r: &mut Redirections, sel: UsbSelector, on: bool) {
    r.usb.retain(|s| *s != sel);
    if on {
        r.usb.push(sel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_names_are_unique() {
        let mut r = Redirections::default();
        add_drive(&mut r, "/home/u/Documents".into());
        add_drive(&mut r, "/data/Documents".into());
        add_drive(&mut r, "/home/u/Documents".into()); // same folder: ignored
        let names: Vec<_> = r.drives.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Documen", "Docume2"]);
    }
}
