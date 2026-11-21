// SPDX-License-Identifier: Apache-2.0
//! Bridges the GDK clipboard and the RDP clipboard channel.
//!
//! Local → remote: when the local clipboard changes, announce what it offers;
//! when the remote asks, read and convert it. Remote → local: when the remote
//! clipboard changes, fetch every format we understand and publish them as one
//! GDK content provider. The provider we published is remembered so our own
//! change notification doesn't bounce back to the remote.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use transom_view_core::clipboard::{self, ClipFormat, Image};
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
        // Text, HTML, and the best image format.
        let mut want: VecDeque<ClipFormat> = [ClipFormat::Text, ClipFormat::Html]
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
            .filter_map(|(f, d)| provider_for(f, d))
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
