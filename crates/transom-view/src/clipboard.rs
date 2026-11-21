// SPDX-License-Identifier: Apache-2.0
//! Bridges the GDK clipboard and the RDP clipboard channel.
//!
//! Local → remote: when the local clipboard changes, announce what it offers;
//! when the remote asks, read and convert it. Remote → local: when the remote
//! clipboard changes, fetch every format we understand and publish them as one
//! GDK content provider. The provider we published is remembered so our own
//! change notification doesn't bounce back to the remote.

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

#[derive(Default)]
struct State {
    mode: Option<ClipboardMode>,
    clipboard: Option<gdk::Clipboard>,
    handler: Option<glib::SignalHandlerId>,
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
}

/// Per-view clipboard bridge.
#[derive(Clone, Default)]
pub struct ClipboardBridge(Rc<RefCell<State>>);

impl std::fmt::Debug for ClipboardBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardBridge").finish_non_exhaustive()
    }
}

fn local_formats(cb: &gdk::Clipboard) -> Vec<ClipFormat> {
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
        s.fetch.clear();
        s.fetched.clear();
        s.local_files.clear();
        s.file_waiters.clear(); // wakes pending downloads with an error
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
        let formats = local_formats(cb);
        tracing::debug!(?formats, "local clipboard changed");
        view.send_command(EngineCommand::ClipboardLocalFormats(formats));
    }

    /// The remote asks for our clipboard in `format`.
    pub fn data_requested(&self, view: &TransomView, format: ClipFormat) {
        let (mode, cb) = {
            let s = self.0.borrow();
            (s.mode, s.clipboard.clone())
        };
        let (Some(mode), Some(cb)) = (mode, cb) else {
            view.send_command(EngineCommand::ClipboardReply { format, data: None });
            return;
        };
        if !mode.to_remote() {
            view.send_command(EngineCommand::ClipboardReply { format, data: None });
            return;
        }
        let view = view.clone();
        if format == ClipFormat::Files {
            let bridge = self.clone();
            glib::spawn_future_local(async move {
                let files = read_local_files(&cb).await.unwrap_or_default();
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
            let data = read_local(&cb, format).await;
            if data.is_none() {
                tracing::debug!(?format, "could not read local clipboard");
            }
            view.send_command(EngineCommand::ClipboardReply { format, data });
        });
    }

    /// The remote clipboard changed.
    pub fn remote_formats(&self, view: &TransomView, formats: Vec<ClipFormat>) {
        let mut s = self.0.borrow_mut();
        if !s.mode.is_some_and(|m| m.from_remote()) || formats.is_empty() {
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
        let entry = self.0.borrow().local_files.get(index as usize).cloned();
        let view = view.clone();
        glib::spawn_future_local(async move {
            let data = match (entry, range) {
                (Some((_, d)), FileRange::Size) => Some(d.size.to_le_bytes().to_vec()),
                (Some((path, d)), FileRange::Bytes { offset, len }) if !d.is_dir => {
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
        rx.recv().await.ok().flatten()
    }

    fn files_provider(&self, view: &TransomView, data: &[u8]) -> Option<gdk::ContentProvider> {
        let files = clipboard::decode_file_group(data)?;
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
    async fn download(&self, view: &TransomView, files: &[FileDescriptor]) -> Option<Vec<PathBuf>> {
        const CHUNK: u32 = 4 * 1024 * 1024;
        let root = {
            let mut s = self.0.borrow_mut();
            let base = s
                .staging
                .get_or_insert_with(|| {
                    glib::user_cache_dir()
                        .join("transom")
                        .join("clipboard")
                        .join(format!(
                            "{}-{:x}",
                            std::process::id(),
                            glib::monotonic_time()
                        ))
                })
                .clone();
            base.join(format!("{}", s.next_stream))
        };
        std::fs::create_dir_all(&root).ok()?;
        let mut top = Vec::new();
        for (i, d) in files.iter().enumerate() {
            let path = root.join(&d.path);
            let first = root.join(d.path.split('/').next().unwrap_or(&d.path));
            if !top.contains(&first) {
                top.push(first);
            }
            if d.is_dir {
                std::fs::create_dir_all(&path).ok()?;
                continue;
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok()?;
            }
            let size = if d.size > 0 {
                d.size
            } else {
                let b = self.request_file(view, i as u32, FileRange::Size).await?;
                u64::from_le_bytes(b.get(..8)?.try_into().ok()?)
            };
            let mut out = std::fs::File::create(&path).ok()?;
            let mut offset = 0u64;
            while offset < size {
                let len = CHUNK.min((size - offset) as u32);
                let chunk = self
                    .request_file(view, i as u32, FileRange::Bytes { offset, len })
                    .await?;
                if chunk.is_empty() {
                    break;
                }
                std::io::Write::write_all(&mut out, &chunk).ok()?;
                offset += chunk.len() as u64;
            }
        }
        tracing::debug!(root = %root.display(), "remote files staged");
        Some(top)
    }
}

fn read_range(path: &Path, offset: u64, len: u32) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = Vec::with_capacity(len as usize);
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
        ClipFormat::Png => {
            let tex = gdk::Texture::from_bytes(&glib::Bytes::from_owned(data)).ok()?;
            gdk::ContentProvider::for_value(&tex.to_value())
        }
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
