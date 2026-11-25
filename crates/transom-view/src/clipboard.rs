// SPDX-License-Identifier: Apache-2.0
//! Bridges the GDK clipboard and the RDP clipboard channel.
//!
//! "The session has focus" means the view or one of its followers (the other
//! monitors' windows in multi-monitor mode) has keyboard focus.
//!
//! Local → remote: when the local clipboard changes while the session has focus,
//! announce what it offers (a change made elsewhere is announced when the session
//! gets focus back). When the remote asks, read and convert it, but only while the
//! session has focus, only content that was announced, never what a password manager
//! marked as concealed, and never what we published from the remote. Reads need
//! focus because the local clipboard can change without GDK noticing: Wayland
//! compositors tell only the focused client about a new selection, and GDK on macOS
//! checks the pasteboard only when the application becomes active, yet a read
//! returns the current content. Local files are served only from the list the remote
//! was sent, and only while each is still the regular file that was listed.
//!
//! Remote → local: when the remote clipboard changes, fetch every format we
//! understand and publish them as one GDK content provider, again only while the
//! session has focus, so a background session can't replace the local clipboard.
//! Images are passed on as PNG bytes without being decoded here. The provider we
//! published is remembered so our own change notification doesn't bounce back.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use transom_view_core::clipboard::{
    self, ClipFormat, FileDescriptor, FileRange, Image, MAX_CLIP_BYTES,
};
use transom_view_core::config::ClipboardMode;
use transom_view_core::engine::EngineCommand;

use crate::TransomView;

/// Most bytes of a local file served per remote request (the remote asks again).
const MAX_FILE_CHUNK: u32 = 8 * 1024 * 1024;
/// How long to wait for the remote to answer one file request.
const FILE_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the remote has to deliver the clipboard content it announced.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a read of the local clipboard (or the walk of the files on it) may take
/// before the remote's request is answered with a failure.
const LOCAL_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// The clipboard type KDE's password managers and KeePassXC put on secrets. macOS's
/// markers aren't visible through GDK: see [`PlatformClipboard`].
const CONCEALED_TYPES: &[&str] = &["x-kde-passwordManagerHint"];

/// What the system clipboard reports that GDK doesn't, from the function given to
/// [`TransomView::set_platform_clipboard`].
///
/// On macOS GDK sees neither the markers password managers put on secrets
/// (`org.nspasteboard.ConcealedType`, which it maps to `application/octet-stream`)
/// nor pasteboard changes made while the application stays active.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlatformClipboard {
    /// The content is marked concealed or transient (a password manager's secret):
    /// it is neither offered nor sent to the remote.
    pub concealed: bool,
    /// A counter that changes whenever the clipboard's content does (macOS:
    /// `NSPasteboard.changeCount`). A change GDK didn't report is treated as a new
    /// copy: announced, and not served under the previous announcement.
    pub change_count: Option<i64>,
}

type Probe = Rc<dyn Fn() -> PlatformClipboard>;

/// What a change of the session's focus calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusChange {
    /// Focus didn't change.
    None,
    /// Focus left: the remote may not read the local clipboard until it's back.
    Lost,
    /// Focus is back and nothing changed: the last announcement holds again.
    Restored,
    /// Focus is back and the local clipboard changed meanwhile: announce it.
    Announce,
    /// Focus is back and the remote copied last: apply what it copied, if that's
    /// still pending.
    ApplyRemote,
}

/// When the remote may read the local clipboard, and what to do when focus returns:
/// the bridge's bookkeeping, without GDK.
#[derive(Debug, Default)]
struct Gate {
    focused: bool,
    /// Bumped by everything that may change what a read of the local clipboard would
    /// send, or whether it may be sent: a local change, losing focus, a remote copy.
    /// A read started at one epoch is answered only if it's still the same one.
    epoch: u64,
    /// The epoch at which the last announcement was made (or restored).
    valid: Option<u64>,
    /// The last announcement still describes the local clipboard: no local change
    /// and no remote copy since.
    fresh: bool,
    /// The latest change was a local copy, not a remote one.
    local_newer: bool,
}

impl Gate {
    /// A new session: nothing announced yet.
    fn reset(&mut self, focused: bool) {
        *self = Self {
            focused,
            epoch: self.epoch + 1,
            ..Self::default()
        };
    }

    /// The local clipboard changed. Returns whether to announce it now (the session
    /// has focus); otherwise it's announced when focus returns.
    fn local_changed(&mut self) -> bool {
        self.epoch += 1;
        self.fresh = false;
        self.local_newer = true;
        self.focused
    }

    /// The remote copied something. Its format list supersedes ours (MS-RDPECLIP),
    /// so it no longer reads what we announced.
    fn remote_changed(&mut self) {
        self.epoch += 1;
        self.fresh = false;
        self.local_newer = false;
    }

    /// The local clipboard was announced to the remote.
    fn announced(&mut self) {
        self.valid = Some(self.epoch);
        self.fresh = true;
    }

    fn set_focus(&mut self, focused: bool) -> FocusChange {
        if focused == self.focused {
            return FocusChange::None;
        }
        self.focused = focused;
        // Either way, reads started before this point aren't answered.
        self.epoch += 1;
        if !focused {
            FocusChange::Lost
        } else if self.fresh {
            self.valid = Some(self.epoch);
            FocusChange::Restored
        } else if self.local_newer {
            FocusChange::Announce
        } else {
            FocusChange::ApplyRemote
        }
    }

    /// Whether the remote may read the local clipboard now.
    fn may_serve(&self) -> bool {
        self.focused && self.valid == Some(self.epoch)
    }

    /// Whether a read started at `epoch` may still be answered.
    fn still_valid(&self, epoch: u64) -> bool {
        self.may_serve() && self.epoch == epoch
    }
}

/// A local file offered to the remote: where it is, how it was described, and (on
/// Unix) which file it was, so that a different file put in its place isn't read.
#[derive(Debug, Clone)]
struct LocalFile {
    path: PathBuf,
    desc: FileDescriptor,
    /// Device and inode when it was offered.
    #[cfg(unix)]
    id: (u64, u64),
}

#[derive(Default)]
struct State {
    mode: Option<ClipboardMode>,
    clipboard: Option<gdk::Clipboard>,
    handler: Option<glib::SignalHandlerId>,
    /// Counts sessions, so that work started for one never reaches the next.
    session: u64,
    gate: Gate,
    /// The remote formats on offer, those still to fetch, what's been fetched so far,
    /// and the fetch's timeout.
    offered: Vec<ClipFormat>,
    fetch: VecDeque<ClipFormat>,
    fetched: Vec<(ClipFormat, Vec<u8>)>,
    fetch_timeout: Option<glib::SourceId>,
    /// A remote clipboard change that arrived while unfocused, applied on focus.
    pending_remote: Option<Vec<ClipFormat>>,
    /// The provider we last put on the local clipboard.
    ours: Option<gdk::ContentProvider>,
    /// Local files offered to the remote (index = position in the descriptor we
    /// sent). Cleared whenever the clipboard content changes.
    local_files: Vec<LocalFile>,
    /// Remote reads in progress: of the local clipboard (one per format), and of
    /// local files (one per stream).
    reading: HashSet<ClipFormat>,
    reading_files: HashSet<u32>,
    /// Outstanding remote file requests by stream id.
    file_waiters: HashMap<u32, async_channel::Sender<Option<Vec<u8>>>>,
    next_stream: u32,
    /// Where remote files are staged for this session (removed on stop).
    staging: Option<PathBuf>,
    /// The application's view of the system clipboard, and its change count when
    /// GDK last reported a change.
    probe: Option<Probe>,
    platform_count: Option<i64>,
}

impl State {
    fn cancel_fetch(&mut self) {
        self.offered.clear();
        self.fetch.clear();
        self.fetched.clear();
        if let Some(id) = self.fetch_timeout.take() {
            id.remove();
        }
    }
}

/// Per-view clipboard bridge.
#[derive(Clone, Default)]
pub struct ClipboardBridge(Rc<RefCell<State>>);

impl std::fmt::Debug for ClipboardBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardBridge").finish_non_exhaustive()
    }
}

impl ClipboardBridge {
    /// Start bridging for a new session.
    pub fn start(&self, view: &TransomView, mode: ClipboardMode) {
        self.stop();
        let cb = view.clipboard();
        let mut s = self.0.borrow_mut();
        s.mode = Some(mode);
        s.clipboard = Some(cb.clone());
        // "Focus" = the view (or a follower) is the focus widget of the active window
        // (has-focus), so switching to another window counts as leaving the session.
        s.gate.reset(view.session_has_focus());
        if mode.to_remote() {
            let weak = view.downgrade();
            let bridge = self.clone();
            s.handler = Some(cb.connect_changed(move |cb| {
                if let Some(view) = weak.upgrade() {
                    bridge.local_changed(&view, cb);
                }
            }));
            drop(s);
            self.local_changed(view, &cb);
        }
    }

    pub fn stop(&self) {
        let mut s = self.0.borrow_mut();
        if let (Some(cb), Some(id)) = (s.clipboard.take(), s.handler.take()) {
            cb.disconnect(id);
        }
        s.mode = None;
        s.session += 1;
        s.gate.reset(false);
        s.cancel_fetch();
        s.pending_remote = None;
        s.local_files.clear();
        s.reading.clear();
        s.reading_files.clear();
        s.file_waiters.clear(); // wakes pending downloads with an error
        if let Some(dir) = s.staging.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    pub fn set_probe(&self, probe: Probe) {
        self.0.borrow_mut().probe = Some(probe);
    }

    fn platform(&self) -> PlatformClipboard {
        let probe = self.0.borrow().probe.clone();
        probe.map(|p| p()).unwrap_or_default()
    }

    /// GDK reported a change: note the platform's change count. Returns it.
    fn sync_platform(&self) -> Option<i64> {
        let count = self.platform().change_count;
        self.0.borrow_mut().platform_count = count;
        count
    }

    /// After a copy made in this process: accept the change count GDK's own write
    /// produced, one more than `before`, and no other. GDK on macOS (4.24) reports
    /// such a copy before writing it to the pasteboard, then writes it with one
    /// `clearContents` (one change). Reads of a copy made in this process come from
    /// GDK's own provider, not the pasteboard. Any other count means another
    /// application wrote as well: it stays unseen, so the next request is refused
    /// and the clipboard announced again.
    fn accept_own_write(&self, before: Option<i64>) {
        let expected = before.and_then(|c| c.checked_add(1));
        if expected.is_some() && self.platform().change_count == expected {
            self.0.borrow_mut().platform_count = expected;
        }
    }

    /// The platform clipboard changed without GDK reporting it (macOS).
    fn changed_unseen(&self) -> bool {
        let count = self.platform().change_count;
        count.is_some() && count != self.0.borrow().platform_count
    }

    fn concealed(&self, cb: &gdk::Clipboard) -> bool {
        let f = cb.formats();
        CONCEALED_TYPES.iter().any(|t| f.contain_mime_type(t)) || self.platform().concealed
    }

    /// The clipboard holds what we published from the remote.
    fn holds_ours(&self, cb: &gdk::Clipboard) -> bool {
        let ours = self.0.borrow().ours.clone();
        ours.is_some() && cb.content() == ours
    }

    fn local_formats(&self, cb: &gdk::Clipboard) -> Vec<ClipFormat> {
        if self.concealed(cb) {
            return Vec::new();
        }
        let f = cb.formats();
        let mut out = Vec::new();
        if f.contain_mime_type("text/plain;charset=utf-8")
            || f.contain_mime_type("text/plain")
            || f.contains_type(glib::Type::STRING)
        {
            out.push(ClipFormat::Text);
        }
        if f.contain_mime_type("text/html") {
            out.push(ClipFormat::Html);
        }
        if f.contains_type(gdk::FileList::static_type()) || f.contain_mime_type("text/uri-list") {
            out.push(ClipFormat::Files);
        }
        if f.contains_type(gdk::Texture::static_type()) || f.contain_mime_type("image/png") {
            out.push(ClipFormat::Png);
            out.push(ClipFormat::Dib);
        }
        out
    }

    fn local_changed(&self, view: &TransomView, cb: &gdk::Clipboard) {
        let before = self.sync_platform();
        if cb.is_local() && before.is_some() {
            // GDK writes a copy made in this process to the system clipboard right
            // after reporting it: accept the count that write produces once it has.
            let bridge = self.clone();
            glib::idle_add_local_once(move || bridge.accept_own_write(before));
        }
        // Our own publication of remote content: the remote has it already, and it
        // isn't offered back (nor read back: GDK would decode a remote image).
        if self.holds_ours(cb) {
            return;
        }
        let announce = {
            let mut s = self.0.borrow_mut();
            // The files offered before are gone: not readable by index any more.
            s.local_files.clear();
            // The local copy is newer than a remote change still pending or being
            // fetched (focus can return before GDK reports the copy): that must not
            // replace it.
            s.pending_remote = None;
            s.cancel_fetch();
            s.gate.local_changed()
        };
        // Copies made in other apps while this session is in the background aren't
        // offered to the remote until the user comes back to it.
        if announce {
            self.announce(view, cb);
        } else {
            tracing::debug!("local clipboard changed; announced when the session gets focus");
        }
    }

    fn announce(&self, view: &TransomView, cb: &gdk::Clipboard) {
        let formats = self.local_formats(cb);
        {
            let mut s = self.0.borrow_mut();
            s.local_files.clear();
            s.gate.announced();
        }
        tracing::debug!(?formats, "announcing local clipboard");
        view.send_command(EngineCommand::ClipboardLocalFormats(formats));
    }

    /// The keyboard focus of the session's view or of one of its followers changed.
    pub fn focus_changed(&self, view: &TransomView) {
        let focused = view.session_has_focus();
        let (mode, cb) = {
            let s = self.0.borrow();
            (s.mode, s.clipboard.clone())
        };
        let (Some(mode), Some(cb)) = (mode, cb) else {
            return;
        };
        // macOS: a pasteboard change GDK didn't report is a copy made elsewhere.
        if focused && mode.to_remote() && self.changed_unseen() {
            self.local_changed(view, &cb);
        }
        let change = self.0.borrow_mut().gate.set_focus(focused);
        if change != FocusChange::None {
            tracing::debug!(focused, ?change, "session focus");
        }
        match change {
            FocusChange::None | FocusChange::Lost | FocusChange::Restored => {}
            FocusChange::Announce => {
                // The local copy is newer than whatever the remote offered before.
                {
                    let mut s = self.0.borrow_mut();
                    s.pending_remote = None;
                    s.cancel_fetch();
                }
                if mode.to_remote() {
                    self.announce(view, &cb);
                }
            }
            FocusChange::ApplyRemote => {
                let pending = self.0.borrow_mut().pending_remote.take();
                if let Some(formats) = pending {
                    self.fetch_remote(view, formats);
                }
            }
        }
    }

    /// Whether the remote may read the local clipboard: the session has focus, the
    /// content is what we announced (at `epoch`, for a read already started), it
    /// isn't concealed, and it isn't what we published from the remote.
    fn may_serve(&self, cb: &gdk::Clipboard, epoch: Option<u64>) -> bool {
        let gate = {
            let s = self.0.borrow();
            match epoch {
                Some(e) => s.gate.still_valid(e),
                None => s.gate.may_serve(),
            }
        };
        let ok = gate && !self.changed_unseen() && !self.holds_ours(cb) && !self.concealed(cb);
        if !ok {
            tracing::debug!(
                "not answering the remote's clipboard request (no focus, changed, concealed or its own)"
            );
        }
        ok
    }

    /// The remote asks for our clipboard in `format`.
    pub fn data_requested(&self, view: &TransomView, format: ClipFormat) {
        let refuse = || view.send_command(EngineCommand::ClipboardReply { format, data: None });
        let (mode, cb) = {
            let s = self.0.borrow();
            (s.mode, s.clipboard.clone())
        };
        let (Some(mode), Some(cb)) = (mode, cb) else {
            refuse();
            return;
        };
        if !mode.to_remote() {
            refuse();
            return;
        }
        if self.changed_unseen() {
            // macOS: what the clipboard holds now was never announced: announce it
            // (if the session has focus) instead of answering for the old content.
            self.local_changed(view, &cb);
            refuse();
            return;
        }
        if !self.may_serve(&cb, None) {
            refuse();
            return;
        }
        let (epoch, session) = {
            let mut s = self.0.borrow_mut();
            if !s.reading.insert(format) {
                drop(s);
                tracing::debug!(?format, "the remote is already reading this format");
                refuse();
                return;
            }
            (s.gate.epoch, s.session)
        };
        let view = view.clone();
        let bridge = self.clone();
        // Reads are bounded: an application that never delivers its clipboard mustn't
        // keep this format busy (and every later request for it refused).
        let timed_out = |format| {
            tracing::debug!(?format, "local clipboard read timed out");
        };
        glib::spawn_future_local(async move {
            if format == ClipFormat::Files {
                let mut files = match glib::future_with_timeout(
                    LOCAL_READ_TIMEOUT,
                    read_local_files(&cb),
                )
                .await
                {
                    Ok(files) => files.unwrap_or_default(),
                    Err(_) => {
                        timed_out(format);
                        Vec::new()
                    }
                };
                if !bridge.finish_read(session, format) {
                    return;
                }
                if !bridge.may_serve(&cb, Some(epoch)) {
                    files.clear();
                }
                let descriptors: Vec<FileDescriptor> =
                    files.iter().map(|f| f.desc.clone()).collect();
                tracing::debug!(count = descriptors.len(), "offering local files");
                bridge.0.borrow_mut().local_files = files;
                let data =
                    (!descriptors.is_empty()).then(|| clipboard::encode_file_group(&descriptors));
                view.send_command(EngineCommand::ClipboardReply { format, data });
                return;
            }
            let mut data = match glib::future_with_timeout(
                LOCAL_READ_TIMEOUT,
                read_local(&cb, format),
            )
            .await
            {
                Ok(data) => data,
                Err(_) => {
                    timed_out(format);
                    None
                }
            };
            if !bridge.finish_read(session, format) {
                return;
            }
            // Reads are asynchronous: what they return may already be a newer copy (a
            // password the user copied meanwhile, or focus may have gone), which must
            // not reach the remote. (On Windows GDK reads on another thread, and a
            // change made during the read can go unseen here: #21.)
            if data.is_none() {
                tracing::debug!(?format, "could not read local clipboard");
            } else if !bridge.may_serve(&cb, Some(epoch)) {
                data = None;
            }
            view.send_command(EngineCommand::ClipboardReply { format, data });
        });
    }

    /// A read for `session` finished: whether that session is still the current one
    /// (otherwise there's no one to answer).
    fn finish_read(&self, session: u64, format: ClipFormat) -> bool {
        let mut s = self.0.borrow_mut();
        if s.session != session {
            return false;
        }
        s.reading.remove(&format);
        true
    }

    /// The remote clipboard changed.
    pub fn remote_formats(&self, view: &TransomView, formats: Vec<ClipFormat>) {
        {
            let mut s = self.0.borrow_mut();
            let Some(mode) = s.mode else {
                return;
            };
            // The remote's copy supersedes ours: it reads neither what we announced
            // nor the files in it, and an older remote change still pending is moot.
            s.gate.remote_changed();
            s.local_files.clear();
            s.pending_remote = None;
            s.cancel_fetch();
            if !mode.from_remote() || formats.is_empty() {
                return;
            }
            if !s.gate.focused {
                // Applied when the session gets focus back (latest change wins).
                s.pending_remote = Some(formats);
                return;
            }
        }
        self.fetch_remote(view, formats);
    }

    /// Fetch the remote's clipboard: files, text, HTML, and the best image format.
    fn fetch_remote(&self, view: &TransomView, formats: Vec<ClipFormat>) {
        let mut want: VecDeque<ClipFormat> =
            [ClipFormat::Files, ClipFormat::Text, ClipFormat::Html]
                .into_iter()
                .filter(|f| formats.contains(f))
                .collect();
        if formats.contains(&ClipFormat::Png) {
            want.push_back(ClipFormat::Png);
        } else if formats.contains(&ClipFormat::Dib) {
            want.push_back(ClipFormat::Dib);
        }
        let first = {
            let mut s = self.0.borrow_mut();
            s.cancel_fetch();
            let first = want.front().copied();
            s.fetch = want;
            s.offered = formats;
            if first.is_some() {
                // A remote that never delivers mustn't leave the fetch hanging, nor
                // deliver much later.
                let weak = Rc::downgrade(&self.0);
                s.fetch_timeout = Some(glib::timeout_add_local_once(FETCH_TIMEOUT, move || {
                    if let Some(state) = weak.upgrade() {
                        let mut s = state.borrow_mut();
                        s.fetch_timeout = None; // this source is done
                        if !s.fetch.is_empty() {
                            tracing::warn!("the remote didn't deliver its clipboard in time");
                            s.cancel_fetch();
                        }
                    }
                }));
            }
            first
        };
        if let Some(f) = first {
            view.send_command(EngineCommand::ClipboardRequest(f));
        }
    }

    /// A requested remote format arrived.
    pub fn remote_data(&self, view: &TransomView, format: ClipFormat, data: Option<Vec<u8>>) {
        let mut s = self.0.borrow_mut();
        if s.fetch.front() != Some(&format) {
            return; // stale (the remote clipboard changed meanwhile)
        }
        if !s.gate.focused {
            // Focus left while the remote was answering: what it sends mustn't replace
            // the local clipboard while the user is elsewhere. Fetched again when the
            // session gets focus back.
            tracing::debug!("session lost focus during the remote clipboard transfer");
            let offered = std::mem::take(&mut s.offered);
            s.cancel_fetch();
            s.pending_remote = Some(offered);
            return;
        }
        s.fetch.pop_front();
        if let Some(d) = data.filter(|d| d.len() <= MAX_CLIP_BYTES) {
            s.fetched.push((format, d));
        }
        if let Some(next) = s.fetch.front().copied() {
            drop(s);
            view.send_command(EngineCommand::ClipboardRequest(next));
            return;
        }
        let fetched = std::mem::take(&mut s.fetched);
        s.cancel_fetch();
        let (Some(cb), session) = (s.clipboard.clone(), s.session) else {
            return;
        };
        drop(s);
        let providers: Vec<gdk::ContentProvider> = fetched
            .into_iter()
            .filter_map(|(f, d)| match f {
                ClipFormat::Files => self.files_provider(view, &d, session),
                _ => provider_for(f, d),
            })
            .collect();
        if providers.is_empty() {
            return;
        }
        let union = gdk::ContentProvider::new_union(&providers);
        self.0.borrow_mut().ours = Some(union.clone());
        // (Its change count is accepted by `local_changed`, which this triggers.)
        if let Err(e) = cb.set_content(Some(&union)) {
            tracing::warn!("setting local clipboard: {e}");
        }
    }
}

impl ClipboardBridge {
    /// The remote asks for (part of) one of the local files we offered.
    pub fn file_requested(&self, view: &TransomView, stream_id: u32, index: u32, range: FileRange) {
        // Only files from the list sent for the current clipboard content (it's
        // cleared when that changes), and one read per stream at a time. Focus isn't
        // needed: the list was read and sent while the session had focus, these are
        // files the user offered, and a paste in progress may continue while the user
        // is elsewhere.
        let (entry, session) = {
            let mut s = self.0.borrow_mut();
            let entry = s.local_files.get(index as usize).cloned();
            let entry = entry.filter(|_| s.reading_files.insert(stream_id));
            (entry, s.session)
        };
        let Some(entry) = entry else {
            view.send_command(EngineCommand::ClipboardFileReply {
                stream_id,
                data: None,
            });
            return;
        };
        let view = view.clone();
        let bridge = self.clone();
        glib::spawn_future_local(async move {
            let data = match range {
                FileRange::Size => Some(entry.desc.size.to_le_bytes().to_vec()),
                FileRange::Bytes { offset, len } if !entry.desc.is_dir => {
                    gio::spawn_blocking(move || read_range(&entry, offset, len))
                        .await
                        .ok()
                        .flatten()
                }
                FileRange::Bytes { .. } => None,
            };
            {
                let mut s = bridge.0.borrow_mut();
                if s.session != session {
                    return;
                }
                s.reading_files.remove(&stream_id);
            }
            view.send_command(EngineCommand::ClipboardFileReply { stream_id, data });
        });
    }

    /// Data for a remote file request arrived.
    pub fn file_data(&self, stream_id: u32, data: Option<Vec<u8>>) {
        if let Some(tx) = self.0.borrow_mut().file_waiters.remove(&stream_id) {
            let _ = tx.try_send(data);
        }
    }

    async fn request_file(
        &self,
        view: &TransomView,
        session: u64,
        index: u32,
        range: FileRange,
    ) -> Option<Vec<u8>> {
        let (tx, rx) = async_channel::bounded(1);
        let stream_id = {
            let mut s = self.0.borrow_mut();
            // Indices from one session's file list mean nothing to the next server.
            if s.session != session {
                tracing::warn!("files copied in an earlier session can't be fetched");
                return None;
            }
            s.next_stream = s.next_stream.wrapping_add(1);
            let id = s.next_stream;
            s.file_waiters.insert(id, tx);
            id
        };
        view.send_command(EngineCommand::ClipboardFileRequest {
            stream_id,
            index,
            range,
        });
        // A remote that never answers (or a session that ended) mustn't hang the
        // application that's pasting.
        let reply = glib::future_with_timeout(FILE_REQUEST_TIMEOUT, rx.recv()).await;
        self.0.borrow_mut().file_waiters.remove(&stream_id);
        match reply {
            Ok(r) => r.ok().flatten(),
            Err(_) => {
                tracing::warn!(stream_id, "remote didn't answer a file request");
                None
            }
        }
    }

    fn files_provider(
        &self,
        view: &TransomView,
        data: &[u8],
        session: u64,
    ) -> Option<gdk::ContentProvider> {
        let files = clipboard::decode_file_group_indexed(data)?;
        if files.is_empty() {
            return None;
        }
        tracing::debug!(count = files.len(), "remote offers files");
        let bridge = self.clone();
        let view = view.downgrade();
        let fetch: remote_files::Fetch = Rc::new(move || {
            let (bridge, view, files) = (bridge.clone(), view.clone(), files.clone());
            Box::pin(async move {
                let view = view.upgrade()?;
                bridge.download(&view, &files, session).await
            })
        });
        Some(remote_files::RemoteFiles::new(fetch).upcast())
    }

    /// Download the remote files into this session's staging directory and return
    /// the top-level paths. Only from the session that offered them.
    async fn download(
        &self,
        view: &TransomView,
        files: &[(u32, FileDescriptor)],
        session: u64,
    ) -> Option<Vec<PathBuf>> {
        const CHUNK: u32 = 4 * 1024 * 1024;
        let root = {
            let mut s = self.0.borrow_mut();
            if s.session != session {
                tracing::warn!("files copied in an earlier session can't be fetched");
                return None;
            }
            let base = s.staging.get_or_insert_with(new_staging_dir).clone();
            base.join(format!("{}", s.next_stream))
        };
        create_private_dir(&root).ok()?;
        let mut top = Vec::new();
        for &(i, ref d) in files {
            let path = root.join(&d.path);
            let first = root.join(d.path.split('/').next().unwrap_or(&d.path));
            if !top.contains(&first) {
                top.push(first);
            }
            if d.is_dir {
                create_private_dir(&path).ok()?;
                continue;
            }
            if let Some(parent) = path.parent() {
                create_private_dir(parent).ok()?;
            }
            let size = if d.size > 0 {
                d.size
            } else {
                let b = self.request_file(view, session, i, FileRange::Size).await?;
                u64::from_le_bytes(b.get(..8)?.try_into().ok()?)
            };
            let mut out = std::fs::File::create(&path).ok()?;
            let mut offset = 0u64;
            while offset < size {
                // In u64 first: (size - offset) as u32 wraps for files over 4 GiB.
                let len = u64::from(CHUNK).min(size - offset) as u32;
                let chunk = self
                    .request_file(view, session, i, FileRange::Bytes { offset, len })
                    .await?;
                // Short, empty or oversized answers are errors, not the end of the file:
                // a truncated file must not look like a successful paste.
                if chunk.is_empty() || chunk.len() > len as usize {
                    tracing::warn!(path = %d.path, offset, "remote file transfer failed");
                    return None;
                }
                std::io::Write::write_all(&mut out, &chunk).ok()?;
                offset += chunk.len() as u64;
            }
        }
        tracing::debug!(root = %root.display(), "remote files staged");
        Some(top)
    }
}

/// A new staging directory for this session's remote files, 0700. Leftovers of
/// sessions that ended without cleaning up (a crash) are removed after a day.
fn new_staging_dir() -> PathBuf {
    let base = glib::user_cache_dir().join("transom").join("clipboard");
    if let Ok(entries) = std::fs::read_dir(&base) {
        let day = std::time::Duration::from_secs(24 * 3600);
        for e in entries.flatten() {
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > day);
            if old {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    base.join(format!(
        "{}-{:x}",
        std::process::id(),
        glib::monotonic_time()
    ))
}

/// `create_dir_all`, with the new directories readable by this user only.
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(path)
}

/// How many bytes a request for `len` bytes at `offset` of a file offered with
/// `size` bytes may read: at most [`MAX_FILE_CHUNK`], and nothing past `size`.
fn readable_len(size: u64, offset: u64, len: u32) -> u32 {
    size.saturating_sub(offset)
        .min(u64::from(len.min(MAX_FILE_CHUNK))) as u32
}

/// Open a file we offered, if it is still a regular file and not a link put in its
/// place (which isn't followed); on Unix, also the same device and inode.
fn open_offered(f: &LocalFile) -> Option<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        // O_NONBLOCK: a FIFO put in its place can't block the open (it's refused
        // below); it has no effect on regular files.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY)
            .open(&f.path)
            .ok()?;
        let m = file.metadata().ok()?;
        (m.is_file() && (m.dev(), m.ino()) == f.id).then_some(file)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT: a link or junction put in its place is opened
        // itself, not followed, and isn't a regular file, so it's refused below.
        // There's no identity check here (the volume serial number and file index
        // aren't in stable std yet): a different regular file put at the same path
        // is read, up to the size that was offered.
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&f.path)
            .ok()?;
        file.metadata().ok()?.file_type().is_file().then_some(file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = f;
        None
    }
}

fn read_range(f: &LocalFile, offset: u64, len: u32) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = open_offered(f)?;
    let len = readable_len(f.desc.size, offset, len);
    let mut buf = Vec::with_capacity(len as usize);
    if len > 0 {
        file.seek(SeekFrom::Start(offset)).ok()?;
        file.take(u64::from(len)).read_to_end(&mut buf).ok()?;
    }
    Some(buf)
}

/// Expand the paths on the local clipboard into the files to offer: directories are
/// walked; symbolic links and special files (FIFOs, devices, sockets) are skipped,
/// so only what is really in the copied folders is offered.
///
/// On Unix the walk goes through directory handles: each folder is opened without
/// following a link, checked to be the one just looked at, and listed and descended
/// from that handle, so a folder swapped for a link meanwhile isn't followed.
#[cfg(unix)]
fn walk_roots(roots: &[PathBuf]) -> Vec<LocalFile> {
    let mut walk = Walk::default();
    for r in roots {
        walk.root(r);
    }
    if walk.skipped > 0 {
        tracing::debug!(
            skipped = walk.skipped,
            "not offering symbolic links or special files"
        );
    }
    walk.out
}

#[cfg(unix)]
#[derive(Default)]
struct Walk {
    out: Vec<LocalFile>,
    skipped: usize,
}

#[cfg(unix)]
impl Walk {
    /// A path the user copied. It's looked at by path, which needs only search
    /// permission on its folder (as any file the user picked); a folder is then opened
    /// without following a link, checked to be the one looked at, and walked from
    /// that handle.
    fn root(&mut self, path: &Path) {
        use rustix::fs::{Mode, OFlags};
        use std::os::unix::fs::MetadataExt;
        if self.out.len() >= clipboard::MAX_FILES {
            return;
        }
        let Some(name) = path.file_name() else {
            return;
        };
        let rel = name.to_string_lossy().into_owned();
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return;
        };
        let id = (meta.dev(), meta.ino());
        let ft = meta.file_type();
        if ft.is_file() {
            self.out.push(LocalFile {
                path: path.to_path_buf(),
                desc: FileDescriptor {
                    path: rel,
                    size: meta.len(),
                    is_dir: false,
                },
                id,
            });
        } else if ft.is_dir() {
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let fd = rustix::fs::open(path, flags, Mode::empty())
                .ok()
                .filter(|fd| rustix::fs::fstat(fd).is_ok_and(|st| stat_id(&st) == id));
            let Some(fd) = fd else {
                self.skipped += 1; // swapped for something else since
                return;
            };
            self.out.push(LocalFile {
                path: path.to_path_buf(),
                desc: FileDescriptor {
                    path: rel.clone(),
                    size: 0,
                    is_dir: true,
                },
                id,
            });
            self.dir(&fd, path, &rel);
        } else {
            self.skipped += 1;
        }
    }

    /// `name` in the directory open at `dir` (at `path`, offered as `rel`): a regular
    /// file is listed, a directory listed and walked, anything else skipped.
    fn entry(
        &mut self,
        dir: std::os::fd::BorrowedFd<'_>,
        name: &std::ffi::OsStr,
        path: PathBuf,
        rel: String,
    ) {
        use rustix::fs::{AtFlags, FileType};
        if self.out.len() >= clipboard::MAX_FILES {
            return;
        }
        let Ok(st) = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) else {
            return;
        };
        let id = stat_id(&st);
        match FileType::from_raw_mode(st.st_mode) {
            FileType::RegularFile => self.out.push(LocalFile {
                path,
                desc: FileDescriptor {
                    path: rel,
                    size: u64::try_from(st.st_size).unwrap_or(0),
                    is_dir: false,
                },
                id,
            }),
            FileType::Directory => {
                let Some(fd) = open_dir(dir, name, id) else {
                    self.skipped += 1; // swapped for something else since
                    return;
                };
                self.out.push(LocalFile {
                    path: path.clone(),
                    desc: FileDescriptor {
                        path: rel.clone(),
                        size: 0,
                        is_dir: true,
                    },
                    id,
                });
                self.dir(&fd, &path, &rel);
            }
            _ => self.skipped += 1,
        }
    }

    /// Walk the entries of the directory open at `fd` (at `path`, offered as `rel`).
    fn dir(&mut self, fd: &std::os::fd::OwnedFd, path: &Path, rel: &str) {
        use std::os::fd::AsFd;
        use std::os::unix::ffi::OsStrExt;
        let Ok(entries) = rustix::fs::Dir::read_from(fd) else {
            return;
        };
        let mut names: Vec<std::ffi::OsString> = entries
            .flatten()
            .map(|e| std::ffi::OsStr::from_bytes(e.file_name().to_bytes()).to_owned())
            .filter(|n| n != "." && n != "..")
            .collect();
        names.sort();
        for n in names {
            let rel = format!("{rel}/{}", n.to_string_lossy());
            self.entry(fd.as_fd(), &n, path.join(&n), rel);
        }
    }
}

/// Open the directory `name` in `dir` without following a link, if it's still the
/// directory `id` that was looked at.
#[cfg(unix)]
fn open_dir(
    dir: std::os::fd::BorrowedFd<'_>,
    name: &std::ffi::OsStr,
    id: (u64, u64),
) -> Option<std::os::fd::OwnedFd> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = rustix::fs::openat(dir, name, flags, Mode::empty()).ok()?;
    (stat_id(&rustix::fs::fstat(&fd).ok()?) == id).then_some(fd)
}

/// Device and inode, as std's `MetadataExt` gives them (what `open_offered` compares).
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)] // dev_t and ino_t differ between platforms
fn stat_id(st: &rustix::fs::Stat) -> (u64, u64) {
    (st.st_dev as u64, st.st_ino as u64)
}

/// [`walk_roots`] elsewhere (Windows), by path: links and junctions are skipped, but
/// a folder swapped for one between its check and its listing is followed.
#[cfg(not(unix))]
fn walk_roots(roots: &[PathBuf]) -> Vec<LocalFile> {
    fn walk(path: &Path, rel: String, out: &mut Vec<LocalFile>, skipped: &mut usize) {
        if out.len() >= clipboard::MAX_FILES {
            return;
        }
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return;
        };
        let kind = meta.file_type();
        if !kind.is_file() && !kind.is_dir() {
            *skipped += 1;
            return;
        }
        let is_dir = kind.is_dir();
        out.push(LocalFile {
            path: path.to_path_buf(),
            desc: FileDescriptor {
                path: rel.clone(),
                size: if is_dir { 0 } else { meta.len() },
                is_dir,
            },
        });
        if is_dir && let Ok(entries) = std::fs::read_dir(path) {
            let mut names: Vec<_> = entries.flatten().map(|e| e.file_name()).collect();
            names.sort();
            for n in names {
                walk(
                    &path.join(&n),
                    format!("{rel}/{}", n.to_string_lossy()),
                    out,
                    skipped,
                );
            }
        }
    }
    let mut out = Vec::new();
    let mut skipped = 0;
    for r in roots {
        if let Some(name) = r.file_name() {
            walk(
                r,
                name.to_string_lossy().into_owned(),
                &mut out,
                &mut skipped,
            );
        }
    }
    if skipped > 0 {
        tracing::debug!(skipped, "not offering symbolic links or special files");
    }
    out
}

/// The files on the local clipboard, expanded by [`walk_roots`].
async fn read_local_files(cb: &gdk::Clipboard) -> Option<Vec<LocalFile>> {
    let v = cb
        .read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT)
        .await
        .ok()?;
    let list = v.get::<gdk::FileList>().ok()?;
    let roots: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
    gio::spawn_blocking(move || walk_roots(&roots)).await.ok()
}

/// A content provider that downloads remote files only when something pastes.
mod remote_files {
    use std::cell::RefCell;
    use std::future::Future;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::rc::Rc;

    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk::{gdk, gio, glib};

    pub type Fetch = Rc<dyn Fn() -> Pin<Box<dyn Future<Output = Option<Vec<PathBuf>>>>>>;

    mod imp {
        use super::*;
        use gtk::gdk::subclass::prelude::ContentProviderImpl;

        #[derive(Default)]
        #[allow(missing_debug_implementations)] // holds a closure
        pub struct RemoteFiles {
            pub fetch: RefCell<Option<Fetch>>,
            pub done: RefCell<Option<Vec<PathBuf>>>,
        }

        #[glib::object_subclass]
        impl ObjectSubclass for RemoteFiles {
            const NAME: &'static str = "TransomRemoteFiles";
            type Type = super::RemoteFiles;
            type ParentType = gdk::ContentProvider;
        }

        impl ObjectImpl for RemoteFiles {}

        impl ContentProviderImpl for RemoteFiles {
            fn formats(&self) -> gdk::ContentFormats {
                gdk::ContentFormatsBuilder::new()
                    .add_mime_type("text/uri-list")
                    .add_mime_type("x-special/gnome-copied-files")
                    .build()
            }

            fn write_mime_type_future(
                &self,
                mime_type: &str,
                stream: &gio::OutputStream,
                io_priority: glib::Priority,
            ) -> Pin<Box<dyn Future<Output = Result<(), glib::Error>> + 'static>> {
                let obj = self.obj().clone();
                let (mime, stream) = (mime_type.to_string(), stream.clone());
                Box::pin(async move {
                    let paths = obj.ensure().await.ok_or_else(|| {
                        glib::Error::new(gio::IOErrorEnum::Failed, "remote file transfer failed")
                    })?;
                    let uris: Vec<String> = paths
                        .iter()
                        .map(|p| gio::File::for_path(p).uri().to_string())
                        .collect();
                    let text = if mime == "x-special/gnome-copied-files" {
                        format!("copy\n{}", uris.join("\n"))
                    } else {
                        uris.iter().map(|u| format!("{u}\r\n")).collect()
                    };
                    stream
                        .write_all_future(text.into_bytes(), io_priority)
                        .await
                        .map_err(|(_, e)| e)?;
                    Ok(())
                })
            }
        }
    }

    glib::wrapper! {
        pub struct RemoteFiles(ObjectSubclass<imp::RemoteFiles>) @extends gdk::ContentProvider;
    }

    impl RemoteFiles {
        pub fn new(fetch: Fetch) -> Self {
            let obj: Self = glib::Object::new();
            *obj.imp().fetch.borrow_mut() = Some(fetch);
            obj
        }

        /// Download once; later pastes reuse the staged copy.
        async fn ensure(&self) -> Option<Vec<PathBuf>> {
            if let Some(done) = self.imp().done.borrow().clone() {
                return Some(done);
            }
            let fetch = self.imp().fetch.borrow().clone()?;
            let paths = fetch().await?;
            *self.imp().done.borrow_mut() = Some(paths.clone());
            Some(paths)
        }
    }
}

fn provider_for(format: ClipFormat, data: Vec<u8>) -> Option<gdk::ContentProvider> {
    Some(match format {
        ClipFormat::Text => {
            gdk::ContentProvider::for_value(&clipboard::decode_text(&data).to_value())
        }
        ClipFormat::Html => gdk::ContentProvider::for_bytes(
            "text/html",
            &glib::Bytes::from_owned(clipboard::decode_html(&data).into_bytes()),
        ),
        // Passed on undecoded: GDK documents its image loaders as unsuitable for
        // untrusted data, and the application that pastes decodes it anyway. This
        // bridge doesn't read it back either (what we published isn't offered to the
        // remote), but another session that offers it to its server does: GDK's PNG
        // loader decodes it then.
        ClipFormat::Png if clipboard::png_acceptable(&data) => {
            gdk::ContentProvider::for_bytes("image/png", &glib::Bytes::from_owned(data))
        }
        ClipFormat::Png => return None,
        ClipFormat::Files => return None, // handled by files_provider
        ClipFormat::Dib => {
            let img = clipboard::dib_to_image(&data)?;
            let tex = gdk::MemoryTexture::new(
                img.width as i32,
                img.height as i32,
                gdk::MemoryFormat::R8g8b8a8,
                &glib::Bytes::from_owned(img.rgba),
                img.width as usize * 4,
            );
            gdk::ContentProvider::for_value(&tex.upcast::<gdk::Texture>().to_value())
        }
    })
}

async fn read_stream(stream: gio::InputStream) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let chunk = stream
            .read_bytes_future(64 * 1024, glib::Priority::DEFAULT)
            .await
            .ok()?;
        if chunk.is_empty() {
            return Some(out);
        }
        out.extend_from_slice(&chunk);
        if out.len() > MAX_CLIP_BYTES {
            return None;
        }
    }
}

async fn read_local(cb: &gdk::Clipboard, format: ClipFormat) -> Option<Vec<u8>> {
    match format {
        ClipFormat::Text => {
            let text = cb.read_text_future().await.ok()??;
            Some(clipboard::encode_text(&text))
        }
        ClipFormat::Html => {
            let (stream, _) = cb
                .read_future(&["text/html"], glib::Priority::DEFAULT)
                .await
                .ok()?;
            let bytes = read_stream(stream).await?;
            // Firefox (and some others) write UTF-16 with a BOM.
            let html = if bytes.starts_with(&[0xFF, 0xFE]) {
                let u: Vec<u16> = bytes[2..]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&c| u16::from_le_bytes(c))
                    .collect();
                String::from_utf16_lossy(&u)
            } else {
                String::from_utf8_lossy(&bytes).into_owned()
            };
            Some(clipboard::encode_html(&html))
        }
        ClipFormat::Png => {
            let tex = cb.read_texture_future().await.ok()??;
            Some(tex.save_to_png_bytes().to_vec())
        }
        ClipFormat::Files => None, // handled by read_local_files
        ClipFormat::Dib => {
            let tex = cb.read_texture_future().await.ok()??;
            let mut dl = gdk::TextureDownloader::new(&tex);
            dl.set_format(gdk::MemoryFormat::R8g8b8a8);
            let (bytes, stride) = dl.download_bytes();
            let (w, h) = (tex.width() as usize, tex.height() as usize);
            let mut rgba = Vec::with_capacity(w * h * 4);
            for row in 0..h {
                rgba.extend_from_slice(&bytes[row * stride..][..w * 4]);
            }
            Some(clipboard::image_to_dib(&Image {
                width: w as u32,
                height: h as u32,
                rgba,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn focused_and_announced() -> Gate {
        let mut g = Gate::default();
        g.reset(true);
        assert!(g.local_changed());
        g.announced();
        g
    }

    #[test]
    fn served_only_while_focused_and_announced() {
        let mut g = Gate::default();
        g.reset(false);
        assert!(
            !g.local_changed(),
            "a copy made while unfocused waits for focus"
        );
        assert!(!g.may_serve());
        assert_eq!(g.set_focus(true), FocusChange::Announce);
        assert!(!g.may_serve(), "not before it's announced");
        g.announced();
        assert!(g.may_serve());
    }

    #[test]
    fn losing_focus_stops_reads_even_without_a_change_notification() {
        // Finding: mutter tells only the focused client about a new selection, so a
        // copy made in another app may never reach us; a read would still return it.
        let mut g = focused_and_announced();
        let epoch = g.epoch;
        assert!(g.still_valid(epoch));
        assert_eq!(g.set_focus(false), FocusChange::Lost);
        assert!(!g.may_serve());
        assert!(!g.still_valid(epoch), "a read in flight isn't answered");
        // Back, with nothing changed: the announcement holds again, but the read
        // started before focus left still isn't answered.
        assert_eq!(g.set_focus(true), FocusChange::Restored);
        assert!(g.may_serve());
        assert!(!g.still_valid(epoch));
        assert!(g.still_valid(g.epoch));
    }

    #[test]
    fn a_copy_made_elsewhere_is_announced_on_return() {
        let mut g = focused_and_announced();
        g.set_focus(false);
        assert!(!g.local_changed());
        assert_eq!(g.set_focus(true), FocusChange::Announce);
        assert!(
            !g.may_serve(),
            "the old announcement doesn't cover the new copy"
        );
    }

    #[test]
    fn a_change_during_a_read_drops_it() {
        let mut g = focused_and_announced();
        let epoch = g.epoch;
        assert!(g.local_changed());
        assert!(!g.still_valid(epoch));
        g.announced();
        assert!(
            !g.still_valid(epoch),
            "not even after the new copy is announced"
        );
    }

    #[test]
    fn the_remotes_copy_supersedes_ours() {
        let mut g = focused_and_announced();
        g.remote_changed();
        assert!(!g.may_serve());
        g.set_focus(false);
        // Nothing copied locally since: on return, the remote's copy is applied, and
        // ours isn't announced over it.
        assert_eq!(g.set_focus(true), FocusChange::ApplyRemote);
        assert!(!g.may_serve());
        // A local copy after the remote's wins.
        g.set_focus(false);
        g.local_changed();
        assert_eq!(g.set_focus(true), FocusChange::Announce);
    }

    #[test]
    fn a_new_session_starts_unannounced() {
        let mut g = focused_and_announced();
        let epoch = g.epoch;
        g.reset(true);
        assert!(!g.may_serve());
        assert!(!g.still_valid(epoch));
        assert_eq!(g.set_focus(true), FocusChange::None);
    }

    #[test]
    fn file_reads_stop_at_the_offered_size() {
        assert_eq!(readable_len(100, 0, 10), 10);
        assert_eq!(readable_len(100, 95, 10), 5);
        assert_eq!(readable_len(100, 100, 10), 0);
        assert_eq!(readable_len(100, u64::MAX, u32::MAX), 0);
        assert_eq!(readable_len(0, 0, 4096), 0, "a /proc file offered as empty");
        assert_eq!(readable_len(u64::MAX, 0, u32::MAX), MAX_FILE_CHUNK);
    }

    /// A scratch directory under the system temp dir, removed on drop.
    #[cfg(unix)]
    struct Scratch(PathBuf);

    #[cfg(unix)]
    impl Scratch {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "transom-clip-test-{name}-{}-{:x}",
                std::process::id(),
                glib::monotonic_time()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    #[cfg(unix)]
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_walk_skips_links_and_special_files() {
        use std::os::unix::fs::symlink;
        let t = Scratch::new("walk");
        let outside = t.0.join("outside.txt");
        std::fs::write(&outside, "not offered").unwrap();
        let dir = t.0.join("copied");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "hello").unwrap();
        std::fs::write(dir.join("sub").join("b.txt"), "world!").unwrap();
        symlink(&outside, dir.join("link-out")).unwrap();
        symlink("/proc/self/environ", dir.join("environ")).unwrap();
        symlink(&t.0, dir.join("loop")).unwrap();
        // A FIFO would block a reader; skip that part where mkfifo isn't available.
        let fifo = std::process::Command::new("mkfifo")
            .arg(dir.join("fifo"))
            .status()
            .is_ok_and(|s| s.success());
        let root_link = t.0.join("root-link");
        symlink(&outside, &root_link).unwrap();

        let files = walk_roots(&[dir.clone(), root_link]);
        let names: Vec<(&str, u64, bool)> = files
            .iter()
            .map(|f| (f.desc.path.as_str(), f.desc.size, f.desc.is_dir))
            .collect();
        assert_eq!(
            names,
            [
                ("copied", 0, true),
                ("copied/a.txt", 5, false),
                ("copied/sub", 0, true),
                ("copied/sub/b.txt", 6, false),
            ]
        );
        if fifo {
            assert!(dir.join("fifo").exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_copy_from_a_folder_that_can_only_be_searched_is_offered() {
        use std::os::unix::fs::PermissionsExt;
        let t = Scratch::new("searchonly");
        let folder = t.0.join("folder");
        std::fs::create_dir_all(folder.join("sub")).unwrap();
        std::fs::write(folder.join("a.txt"), "offered").unwrap();
        std::fs::write(folder.join("sub").join("b.txt"), "offered").unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o711)).unwrap();
        let out = walk_roots(&[folder.join("a.txt"), folder.join("sub")]);
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        let names: Vec<_> = out.iter().map(|f| f.desc.path.as_str()).collect();
        assert_eq!(names, ["a.txt", "sub", "sub/b.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn the_walk_doesnt_follow_a_folder_swapped_for_a_link() {
        use std::os::fd::AsFd;
        use std::os::unix::fs::{MetadataExt, symlink};
        let t = Scratch::new("dirswap");
        let outside = t.0.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "not offered").unwrap();
        let copied = t.0.join("copied");
        std::fs::create_dir_all(copied.join("sub")).unwrap();
        std::fs::write(copied.join("sub").join("a.txt"), "offered").unwrap();
        let a_id = {
            let m = std::fs::metadata(copied.join("sub").join("a.txt")).unwrap();
            (m.dev(), m.ino())
        };
        let flags = rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY;
        let parent = rustix::fs::open(&copied, flags, rustix::fs::Mode::empty()).unwrap();
        let sub = std::ffi::OsStr::new("sub");
        let st =
            rustix::fs::statat(parent.as_fd(), sub, rustix::fs::AtFlags::SYMLINK_NOFOLLOW).unwrap();
        let id = stat_id(&st);
        let swap = |to: &Path| {
            std::fs::rename(copied.join("sub"), copied.join("sub-moved")).unwrap();
            symlink(to, copied.join("sub")).unwrap();
        };
        let unswap = || {
            std::fs::remove_file(copied.join("sub")).unwrap();
            std::fs::rename(copied.join("sub-moved"), copied.join("sub")).unwrap();
        };

        // Swapped for a link after it was looked at, before it's opened: refused, even
        // when the link leads back to the very same folder.
        swap(&copied.join("sub-moved"));
        assert!(open_dir(parent.as_fd(), sub, id).is_none());
        unswap();

        // Swapped after it was opened: the walk goes on from the handle, in the
        // folder that was copied, not where the link leads.
        let fd = open_dir(parent.as_fd(), sub, id).expect("the folder as it was");
        swap(&outside);
        let mut walk = Walk::default();
        walk.dir(&fd, &copied.join("sub"), "copied/sub");
        let found: Vec<(&str, (u64, u64))> = walk
            .out
            .iter()
            .map(|f| (f.desc.path.as_str(), f.id))
            .collect();
        assert_eq!(found, [("copied/sub/a.txt", a_id)]);
        unswap();
    }

    #[cfg(unix)]
    #[test]
    fn reads_refuse_a_file_swapped_after_the_copy() {
        use std::os::unix::fs::symlink;
        let t = Scratch::new("swap");
        let path = t.0.join("doc.txt");
        std::fs::write(&path, "0123456789").unwrap();
        let secret = t.0.join("secret.txt");
        std::fs::write(&secret, "SECRET-SECRET-SECRET").unwrap();
        let files = walk_roots(std::slice::from_ref(&path));
        let [f] = files.as_slice() else {
            panic!("one file expected")
        };
        // Keeps the original inode allocated, so a replacement can't reuse its number.
        let _original = std::fs::File::open(&path).unwrap();

        assert_eq!(read_range(f, 0, 4).as_deref(), Some(&b"0123"[..]));
        assert_eq!(read_range(f, 8, 100).as_deref(), Some(&b"89"[..]));
        assert_eq!(read_range(f, 10, 100).as_deref(), Some(&b""[..]));

        // Grown after the copy (same file): still only the size that was offered.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut w| std::io::Write::write_all(&mut w, b"ABCDEF"))
            .unwrap();
        assert_eq!(read_range(f, 8, 100).as_deref(), Some(&b"89"[..]));

        // Replaced by a symbolic link: not followed.
        std::fs::remove_file(&path).unwrap();
        symlink(&secret, &path).unwrap();
        assert_eq!(read_range(f, 0, 100), None);

        // Replaced by another regular file: a different inode.
        std::fs::remove_file(&path).unwrap();
        std::fs::copy(&secret, &path).unwrap();
        assert_eq!(read_range(f, 0, 100), None);

        // Replaced by a FIFO: refused without blocking.
        std::fs::remove_file(&path).unwrap();
        let fifo = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        if fifo {
            assert_eq!(read_range(f, 0, 100), None);
        }
    }
}
