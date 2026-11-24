// SPDX-License-Identifier: Apache-2.0
//! Bridges the GDK clipboard and the RDP clipboard channel.
//!
//! Local → remote: when the local clipboard changes *while the session has focus*,
//! announce what it offers (a change made elsewhere is announced when the session
//! gets focus back); when the remote asks, read and convert it, but only content
//! that was announced, and never what a password manager marked as concealed.
//! Remote → local: when the remote clipboard changes, fetch every format we
//! understand and publish them as one GDK content provider, again only while the
//! session has focus, so a background session can't replace the local clipboard.
//! Images are passed on as PNG bytes without being decoded here. The provider we
//! published is remembered so our own change notification doesn't bounce back.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use transom_view_core::clipboard::{self, ClipFormat, FileDescriptor, FileRange, Image};
use transom_view_core::config::ClipboardMode;
use transom_view_core::engine::EngineCommand;

use crate::TransomView;

/// Upper bound for any clipboard payload we read or accept (bytes).
const MAX_CLIP_BYTES: usize = 64 * 1024 * 1024;
/// Most bytes of a local file served per remote request (the remote asks again).
const MAX_FILE_CHUNK: u32 = 8 * 1024 * 1024;
/// How long to wait for the remote to answer one file request.
const FILE_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// Clipboard types password managers set on secrets (KDE/KeePassXC, macOS).
const CONCEALED_TYPES: &[&str] = &[
    "x-kde-passwordManagerHint",
    "org.nspasteboard.ConcealedType",
    "application/x-nspasteboard-concealed-type",
];

#[derive(Default)]
struct State {
    mode: Option<ClipboardMode>,
    clipboard: Option<gdk::Clipboard>,
    handler: Option<glib::SignalHandlerId>,
    /// The view and its `notify::has-focus` handler.
    focus_handler: Option<(glib::WeakRef<TransomView>, glib::SignalHandlerId)>,
    /// Remote formats still to fetch, and what we've fetched so far.
    fetch: VecDeque<ClipFormat>,
    fetched: Vec<(ClipFormat, Vec<u8>)>,
    /// The provider we last put on the local clipboard.
    ours: Option<gdk::ContentProvider>,
    /// Local files offered to the remote (index = position in our descriptor).
    local_files: Vec<(PathBuf, FileDescriptor)>,
    /// Outstanding remote file requests by stream id.
    file_waiters: HashMap<u32, async_channel::Sender<Option<Vec<u8>>>>,
    next_stream: u32,
    /// Where remote files are staged for this session (removed on stop).
    staging: Option<PathBuf>,
    /// Whether the session view has keyboard focus.
    focused: bool,
    /// Local clipboard generation, and the generation last announced to the remote.
    local_gen: u64,
    announced_gen: u64,
    /// A remote clipboard change that arrived while unfocused, applied on focus.
    pending_remote: Option<Vec<ClipFormat>>,
}

/// Per-view clipboard bridge.
#[derive(Clone, Default)]
pub struct ClipboardBridge(Rc<RefCell<State>>);

impl std::fmt::Debug for ClipboardBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardBridge").finish_non_exhaustive()
    }
}

fn concealed(cb: &gdk::Clipboard) -> bool {
    let f = cb.formats();
    CONCEALED_TYPES.iter().any(|t| f.contain_mime_type(t))
}

fn local_formats(cb: &gdk::Clipboard) -> Vec<ClipFormat> {
    if concealed(cb) {
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

impl ClipboardBridge {
    /// Start bridging for a new session.
    pub fn start(&self, view: &TransomView, mode: ClipboardMode) {
        self.stop();
        let cb = view.clipboard();
        let mut s = self.0.borrow_mut();
        s.mode = Some(mode);
        s.clipboard = Some(cb.clone());
        // "Focus" = the view is the focus widget of the active window (has-focus), so
        // switching to another window counts as leaving the session.
        s.focused = view.has_focus();
        let bridge = self.clone();
        let id = view.connect_has_focus_notify(move |v| bridge.focus_changed(v, v.has_focus()));
        s.focus_handler = Some((view.downgrade(), id));
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
        if let Some((view, id)) = s.focus_handler.take()
            && let Some(view) = view.upgrade()
        {
            glib::prelude::ObjectExt::disconnect(&view, id);
        }
        s.mode = None;
        s.fetch.clear();
        s.fetched.clear();
        s.local_files.clear();
        s.file_waiters.clear(); // wakes pending downloads with an error
        s.pending_remote = None;
        if let Some(dir) = s.staging.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    fn local_changed(&self, view: &TransomView, cb: &gdk::Clipboard) {
        // Ignore our own publication of remote content.
        let ours = self.0.borrow().ours.clone();
        if ours.is_some() && cb.content() == ours {
            return;
        }
        let focused = {
            let mut s = self.0.borrow_mut();
            s.local_gen += 1;
            s.focused
        };
        // Copies made in other apps while this session is in the background aren't
        // offered to the remote until the user comes back to it.
        if focused {
            self.announce(view, cb);
        } else {
            tracing::debug!("local clipboard changed; announced when the session gets focus");
        }
    }

    fn announce(&self, view: &TransomView, cb: &gdk::Clipboard) {
        let formats = local_formats(cb);
        {
            let mut s = self.0.borrow_mut();
            s.announced_gen = s.local_gen;
        }
        tracing::debug!(?formats, "announcing local clipboard");
        view.send_command(EngineCommand::ClipboardLocalFormats(formats));
    }

    /// The session view gained or lost keyboard focus.
    fn focus_changed(&self, view: &TransomView, focused: bool) {
        tracing::debug!(focused, "session focus");
        let (announce, pending) = {
            let mut s = self.0.borrow_mut();
            s.focused = focused;
            if !focused || s.mode.is_none() {
                return;
            }
            let announce = s.mode.is_some_and(|m| m.to_remote()) && s.local_gen != s.announced_gen;
            (
                announce.then(|| s.clipboard.clone()).flatten(),
                s.pending_remote.take(),
            )
        };
        if let Some(cb) = announce {
            self.announce(view, &cb);
        }
        if let Some(formats) = pending {
            self.remote_formats(view, formats);
        }
    }

    /// Whether the local clipboard still holds what we last announced (and isn't a
    /// password manager's secret): only then may the remote read it.
    fn may_serve(&self, cb: &gdk::Clipboard) -> bool {
        let s = self.0.borrow();
        s.local_gen == s.announced_gen && !concealed(cb)
    }

    /// The remote asks for our clipboard in `format`.
    pub fn data_requested(&self, view: &TransomView, format: ClipFormat) {
        let (mode, cb, generation) = {
            let s = self.0.borrow();
            (s.mode, s.clipboard.clone(), s.local_gen)
        };
        let (Some(mode), Some(cb)) = (mode, cb) else {
            view.send_command(EngineCommand::ClipboardReply { format, data: None });
            return;
        };
        if !mode.to_remote() || !self.may_serve(&cb) {
            view.send_command(EngineCommand::ClipboardReply { format, data: None });
            return;
        }
        let view = view.clone();
        let bridge = self.clone();
        if format == ClipFormat::Files {
            glib::spawn_future_local(async move {
                let mut files = read_local_files(&cb).await.unwrap_or_default();
                if !bridge.unchanged_since(&cb, generation) {
                    files.clear();
                }
                let descriptors: Vec<FileDescriptor> =
                    files.iter().map(|(_, d)| d.clone()).collect();
                tracing::debug!(count = descriptors.len(), "offering local files");
                bridge.0.borrow_mut().local_files = files;
                let data =
                    (!descriptors.is_empty()).then(|| clipboard::encode_file_group(&descriptors));
                view.send_command(EngineCommand::ClipboardReply { format, data });
            });
            return;
        }
        glib::spawn_future_local(async move {
            let mut data = read_local(&cb, format).await;
            if data.is_none() {
                tracing::debug!(?format, "could not read local clipboard");
            } else if !bridge.unchanged_since(&cb, generation) {
                data = None;
            }
            view.send_command(EngineCommand::ClipboardReply { format, data });
        });
    }

    /// The local clipboard hasn't changed since `generation` and isn't concealed.
    /// Reads are asynchronous: what they return may already be a newer copy (a
    /// password the user copied meanwhile), which must not reach the remote.
    fn unchanged_since(&self, cb: &gdk::Clipboard, generation: u64) -> bool {
        let unchanged = self.0.borrow().local_gen == generation && !concealed(cb);
        if !unchanged {
            tracing::debug!("local clipboard changed during the read; not sending it");
        }
        unchanged
    }

    /// The remote clipboard changed.
    pub fn remote_formats(&self, view: &TransomView, formats: Vec<ClipFormat>) {
        let mut s = self.0.borrow_mut();
        if !s.mode.is_some_and(|m| m.from_remote()) || formats.is_empty() {
            return;
        }
        if !s.focused {
            // Applied when the session gets focus back (latest change wins).
            s.pending_remote = Some(formats);
            return;
        }
        // Files, text, HTML, and the best image format.
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
        s.fetch = want;
        s.fetched.clear();
        let next = s.fetch.front().copied();
        drop(s);
        if let Some(f) = next {
            view.send_command(EngineCommand::ClipboardRequest(f));
        }
    }

    /// A requested remote format arrived.
    pub fn remote_data(&self, view: &TransomView, format: ClipFormat, data: Option<Vec<u8>>) {
        let mut s = self.0.borrow_mut();
        if s.fetch.front() != Some(&format) {
            return; // stale (the remote clipboard changed meanwhile)
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
        let Some(cb) = s.clipboard.clone() else {
            return;
        };
        let providers: Vec<gdk::ContentProvider> = fetched
            .into_iter()
            .filter_map(|(f, d)| match f {
                ClipFormat::Files => self.files_provider(view, &d),
                _ => provider_for(f, d),
            })
            .collect();
        if providers.is_empty() {
            return;
        }
        let union = gdk::ContentProvider::new_union(&providers);
        s.ours = Some(union.clone());
        drop(s);
        if let Err(e) = cb.set_content(Some(&union)) {
            tracing::warn!("setting local clipboard: {e}");
        }
    }
}

impl ClipboardBridge {
    /// The remote asks for (part of) one of the local files we offered.
    pub fn file_requested(&self, view: &TransomView, stream_id: u32, index: u32, range: FileRange) {
        // Only files from the clipboard content that was announced.
        let current = {
            let s = self.0.borrow();
            s.local_gen == s.announced_gen
        };
        let entry = current
            .then(|| self.0.borrow().local_files.get(index as usize).cloned())
            .flatten();
        let view = view.clone();
        glib::spawn_future_local(async move {
            let data = match (entry, range) {
                (Some((_, d)), FileRange::Size) => Some(d.size.to_le_bytes().to_vec()),
                (Some((path, d)), FileRange::Bytes { offset, len }) if !d.is_dir => {
                    let len = len.min(MAX_FILE_CHUNK);
                    gio::spawn_blocking(move || read_range(&path, offset, len))
                        .await
                        .ok()
                        .flatten()
                }
                _ => None,
            };
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
        index: u32,
        range: FileRange,
    ) -> Option<Vec<u8>> {
        let (tx, rx) = async_channel::bounded(1);
        let stream_id = {
            let mut s = self.0.borrow_mut();
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

    fn files_provider(&self, view: &TransomView, data: &[u8]) -> Option<gdk::ContentProvider> {
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
                bridge.download(&view, &files).await
            })
        });
        Some(remote_files::RemoteFiles::new(fetch).upcast())
    }

    /// Download the remote files into this session's staging directory and return
    /// the top-level paths.
    async fn download(
        &self,
        view: &TransomView,
        files: &[(u32, FileDescriptor)],
    ) -> Option<Vec<PathBuf>> {
        const CHUNK: u32 = 4 * 1024 * 1024;
        let root = {
            let mut s = self.0.borrow_mut();
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
                let b = self.request_file(view, i, FileRange::Size).await?;
                u64::from_le_bytes(b.get(..8)?.try_into().ok()?)
            };
            let mut out = std::fs::File::create(&path).ok()?;
            let mut offset = 0u64;
            while offset < size {
                // In u64 first: (size - offset) as u32 wraps for files over 4 GiB.
                let len = u64::from(CHUNK).min(size - offset) as u32;
                let chunk = self
                    .request_file(view, i, FileRange::Bytes { offset, len })
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

fn read_range(path: &Path, offset: u64, len: u32) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = Vec::with_capacity(len.min(MAX_FILE_CHUNK) as usize);
    f.take(len as u64).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Expand the local clipboard's file list into descriptors (directories walked,
/// symlinked directories not followed).
async fn read_local_files(cb: &gdk::Clipboard) -> Option<Vec<(PathBuf, FileDescriptor)>> {
    let v = cb
        .read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT)
        .await
        .ok()?;
    let list = v.get::<gdk::FileList>().ok()?;
    let roots: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
    gio::spawn_blocking(move || {
        fn walk(path: &Path, rel: String, out: &mut Vec<(PathBuf, FileDescriptor)>) {
            if out.len() >= clipboard::MAX_FILES {
                return;
            }
            let Ok(meta) = std::fs::symlink_metadata(path) else {
                return;
            };
            let is_dir = meta.is_dir();
            let size = if meta.file_type().is_symlink() {
                std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
            } else {
                meta.len()
            };
            out.push((
                path.to_path_buf(),
                FileDescriptor {
                    path: rel.clone(),
                    size: if is_dir { 0 } else { size },
                    is_dir,
                },
            ));
            if is_dir && let Ok(entries) = std::fs::read_dir(path) {
                let mut names: Vec<_> = entries.flatten().map(|e| e.file_name()).collect();
                names.sort();
                for n in names {
                    walk(
                        &path.join(&n),
                        format!("{rel}/{}", n.to_string_lossy()),
                        out,
                    );
                }
            }
        }
        let mut out = Vec::new();
        for r in roots {
            if let Some(name) = r.file_name() {
                walk(&r, name.to_string_lossy().into_owned(), &mut out);
            }
        }
        out
    })
    .await
    .ok()
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
        // untrusted data, and the application that pastes decodes it anyway.
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
