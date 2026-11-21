// SPDX-License-Identifier: Apache-2.0
//! Clipboard formats and their RDP (Windows clipboard) encodings (MS-RDPECLIP).
//!
//! The toolkit side speaks MIME types; the wire speaks Windows clipboard formats.
//! These conversions are pure so they're tested on every platform.

/// Clipboard content kinds Transom exchanges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClipFormat {
    /// `CF_UNICODETEXT`: UTF-16LE, NUL-terminated.
    Text,
    /// Registered "HTML Format" (`CF_HTML`): UTF-8 with a header of byte offsets.
    Html,
    /// Registered "PNG" format: a PNG file.
    Png,
    /// `CF_DIB`: BITMAPINFOHEADER + pixels (what most Windows apps put/expect).
    Dib,
    /// Registered "FileGroupDescriptorW": a list of files; contents are fetched
    /// separately with FileContents requests.
    Files,
}

/// Standard format ids.
pub const CF_TEXT: u32 = 1;
pub const CF_DIB: u32 = 8;
pub const CF_UNICODETEXT: u32 = 13;
pub const CF_DIBV5: u32 = 17;
/// Registered format names.
pub const HTML_FORMAT_NAME: &str = "HTML Format";
pub const PNG_FORMAT_NAME: &str = "PNG";
pub const FILES_FORMAT_NAME: &str = "FileGroupDescriptorW";
/// Ids we use for registered formats in *our* format lists (any id ≥ 0xC000 works;
/// the name is what identifies it).
pub const LOCAL_HTML_ID: u32 = 0xD010;
pub const LOCAL_PNG_ID: u32 = 0xD011;
pub const LOCAL_FILES_ID: u32 = 0xD012;

impl ClipFormat {
    /// `(id, name)` to announce in a client format list.
    pub fn wire(self) -> (u32, Option<&'static str>) {
        match self {
            Self::Text => (CF_UNICODETEXT, None),
            Self::Html => (LOCAL_HTML_ID, Some(HTML_FORMAT_NAME)),
            Self::Png => (LOCAL_PNG_ID, Some(PNG_FORMAT_NAME)),
            Self::Dib => (CF_DIB, None),
            Self::Files => (LOCAL_FILES_ID, Some(FILES_FORMAT_NAME)),
        }
    }

    /// Recognise a server-announced format.
    pub fn from_wire(id: u32, name: Option<&str>) -> Option<Self> {
        match (id, name.unwrap_or("")) {
            (CF_UNICODETEXT, _) => Some(Self::Text),
            (CF_DIB, _) | (CF_DIBV5, _) => Some(Self::Dib),
            (_, HTML_FORMAT_NAME) => Some(Self::Html),
            (_, PNG_FORMAT_NAME) | (_, "image/png") => Some(Self::Png),
            (_, FILES_FORMAT_NAME) => Some(Self::Files),
            _ => None,
        }
    }
}

/// UTF-8 → `CF_UNICODETEXT` (CRLF line endings, as Windows expects).
pub fn encode_text(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 2 + 2);
    let mut prev_cr = false;
    for c in s.chars() {
        if c == '\n' && !prev_cr {
            out.extend_from_slice(&(b'\r' as u16).to_le_bytes());
        }
        prev_cr = c == '\r';
        let mut buf = [0u16; 2];
        for u in c.encode_utf16(&mut buf) {
            out.extend_from_slice(&u.to_le_bytes());
        }
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// `CF_UNICODETEXT` → UTF-8 (up to the first NUL; CRLF → LF; lossy on bad UTF-16).
pub fn decode_text(data: &[u8]) -> String {
    let units: Vec<u16> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units).replace("\r\n", "\n")
}

/// HTML fragment → `CF_HTML` (UTF-8 with the Version/StartHTML/… header).
pub fn encode_html(fragment: &str) -> Vec<u8> {
    const START: &str = "<html><body>\r\n<!--StartFragment-->";
    const END: &str = "<!--EndFragment-->\r\n</body></html>";
    // Fixed-width offsets so the header length doesn't depend on the values.
    let header_len = format!(
        "Version:0.9\r\nStartHTML:{0:010}\r\nEndHTML:{0:010}\r\nStartFragment:{0:010}\r\nEndFragment:{0:010}\r\n",
        0
    )
    .len();
    let start_html = header_len;
    let start_frag = start_html + START.len();
    let end_frag = start_frag + fragment.len();
    let end_html = end_frag + END.len();
    format!(
        "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start_frag:010}\r\nEndFragment:{end_frag:010}\r\n{START}{fragment}{END}"
    )
    .into_bytes()
}

/// `CF_HTML` → the HTML fragment (falls back to the whole HTML part, then to the
/// raw text if there's no valid header).
pub fn decode_html(data: &[u8]) -> String {
    let data = data.split(|&b| b == 0).next().unwrap_or(data);
    let text = String::from_utf8_lossy(data);
    let field = |name: &str| -> Option<usize> {
        let i = text.find(name)? + name.len();
        let rest = &text[i..];
        let end = rest.find(['\r', '\n']).unwrap_or(rest.len());
        rest[..end].trim().parse().ok()
    };
    let slice = |a: Option<usize>, b: Option<usize>| -> Option<String> {
        let (a, b) = (a?, b?);
        (a <= b && b <= data.len()).then(|| String::from_utf8_lossy(&data[a..b]).into_owned())
    };
    slice(field("StartFragment:"), field("EndFragment:"))
        .or_else(|| slice(field("StartHTML:"), field("EndHTML:")))
        .unwrap_or_else(|| text.into_owned())
}

/// A decoded image: straight-alpha RGBA, top-down rows, stride = width × 4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Largest image we'll decode from the remote (guards allocations).
pub const MAX_IMAGE_PIXELS: u64 = 64 * 1024 * 1024;

fn u32le(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}
fn i32le(d: &[u8], at: usize) -> Option<i32> {
    u32le(d, at).map(|v| v as i32)
}
fn u16le(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

/// `CF_DIB` / `CF_DIBV5` → RGBA. Supports uncompressed 24/32 bpp and 32 bpp
/// BI_BITFIELDS with the standard masks (what Windows actually produces).
pub fn dib_to_image(d: &[u8]) -> Option<Image> {
    let header_size = u32le(d, 0)? as usize;
    let width = i32le(d, 4)?;
    let height = i32le(d, 8)?;
    let bpp = u16le(d, 14)?;
    let compression = u32le(d, 16)?;
    let colors_used = u32le(d, 32)? as usize;
    if header_size < 40 || width <= 0 || height == 0 {
        return None;
    }
    let (w, h) = (width as u32, height.unsigned_abs());
    if (w as u64) * (h as u64) > MAX_IMAGE_PIXELS {
        return None;
    }
    let bottom_up = height > 0;
    // Masks: in the V4/V5 header, or right after a 40-byte header for BI_BITFIELDS.
    let mut offset = header_size;
    let (rmask, gmask, bmask, amask) = match compression {
        0 => (0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0),
        3 if header_size >= 56 => (u32le(d, 40)?, u32le(d, 44)?, u32le(d, 48)?, u32le(d, 52)?),
        3 if header_size >= 52 => (u32le(d, 40)?, u32le(d, 44)?, u32le(d, 48)?, 0),
        3 => {
            offset += 12;
            (u32le(d, 40)?, u32le(d, 44)?, u32le(d, 48)?, 0)
        }
        _ => return None,
    };
    if bpp <= 8 {
        return None; // palettes: not produced by modern apps
    }
    offset += colors_used * 4;
    let bytes_pp = match bpp {
        24 => 3,
        32 => 4,
        _ => return None,
    };
    let stride = (w as usize * bytes_pp).div_ceil(4) * 4;
    let pixels = d.get(offset..offset + stride * h as usize)?;
    let chan = |px: u32, mask: u32| -> u8 {
        if mask == 0 {
            return 255;
        }
        let shift = mask.trailing_zeros();
        let bits = (mask >> shift).count_ones();
        let v = (px & mask) >> shift;
        if bits >= 8 {
            (v >> (bits - 8)) as u8
        } else {
            (v * 255 / ((1 << bits) - 1)) as u8
        }
    };
    let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
    let mut any_alpha = false;
    for row in 0..h as usize {
        let src_row = if bottom_up { h as usize - 1 - row } else { row };
        let line = &pixels[src_row * stride..][..w as usize * bytes_pp];
        for px in line.chunks_exact(bytes_pp) {
            if bytes_pp == 3 {
                rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
            } else {
                let v = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
                let a = if amask == 0 && compression == 0 {
                    px[3]
                } else {
                    chan(v, amask)
                };
                any_alpha |= a != 0;
                rgba.extend_from_slice(&[chan(v, rmask), chan(v, gmask), chan(v, bmask), a]);
            }
        }
    }
    // A 32-bit BI_RGB DIB whose "alpha" byte is all zero means opaque.
    if bytes_pp == 4 && !any_alpha {
        for px in rgba.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
    }
    Some(Image {
        width: w,
        height: h,
        rgba,
    })
}

/// RGBA → `CF_DIB`: 32 bpp BI_RGB, bottom-up (the most compatible form).
pub fn image_to_dib(img: &Image) -> Vec<u8> {
    let (w, h) = (img.width as usize, img.height as usize);
    let mut d = Vec::with_capacity(40 + w * h * 4);
    d.extend_from_slice(&40u32.to_le_bytes());
    d.extend_from_slice(&(img.width as i32).to_le_bytes());
    d.extend_from_slice(&(img.height as i32).to_le_bytes()); // positive = bottom-up
    d.extend_from_slice(&1u16.to_le_bytes()); // planes
    d.extend_from_slice(&32u16.to_le_bytes()); // bpp
    d.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    d.extend_from_slice(&((w * h * 4) as u32).to_le_bytes());
    d.extend_from_slice(&2835i32.to_le_bytes()); // 72 dpi
    d.extend_from_slice(&2835i32.to_le_bytes());
    d.extend_from_slice(&0u32.to_le_bytes());
    d.extend_from_slice(&0u32.to_le_bytes());
    for row in (0..h).rev() {
        for px in img.rgba[row * w * 4..][..w * 4].as_chunks::<4>().0 {
            d.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    d
}

/// What a FileContents request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRange {
    /// The file's size (reply: 8 bytes, little-endian u64).
    Size,
    /// `len` bytes at `offset`.
    Bytes { offset: u64, len: u32 },
}

/// One entry of a `FileGroupDescriptorW`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDescriptor {
    /// Relative path with `/` separators (converted to/from `\` on the wire).
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
}

const FD_ATTRIBUTES: u32 = 0x0000_0004;
const FD_FILESIZE: u32 = 0x0000_0040;
const FD_SHOWPROGRESSUI: u32 = 0x0000_4000;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const FILEDESCRIPTORW_LEN: usize = 592;
/// Upper bound on entries we accept from a remote descriptor.
pub const MAX_FILES: usize = 100_000;

/// Make a remote-supplied relative path safe to create under a staging directory:
/// `/` separators, no empty, `.` or `..` components, no drive letters, no NUL or
/// control characters. Returns `None` if nothing usable remains.
pub fn sanitize_relative_path(raw: &str) -> Option<String> {
    let parts: Vec<&str> = raw
        .split(['\\', '/'])
        .filter(|p| !p.is_empty() && *p != "." && *p != "..")
        .collect();
    if parts.is_empty()
        || parts
            .iter()
            .any(|p| p.contains(':') || p.chars().any(|c| c.is_control()))
    {
        return None;
    }
    Some(parts.join("/"))
}

/// Encode a `FileGroupDescriptorW` (MS-RDPECLIP 2.2.5.2.3.1).
pub fn encode_file_group(files: &[FileDescriptor]) -> Vec<u8> {
    let mut d = Vec::with_capacity(4 + files.len() * FILEDESCRIPTORW_LEN);
    d.extend_from_slice(&(files.len() as u32).to_le_bytes());
    for f in files {
        let start = d.len();
        d.extend_from_slice(&(FD_ATTRIBUTES | FD_FILESIZE | FD_SHOWPROGRESSUI).to_le_bytes());
        d.extend_from_slice(&[0; 16 + 8 + 8]); // clsid, sizel, pointl
        let attrs = if f.is_dir {
            FILE_ATTRIBUTE_DIRECTORY
        } else {
            FILE_ATTRIBUTE_NORMAL
        };
        d.extend_from_slice(&attrs.to_le_bytes());
        d.extend_from_slice(&[0; 24]); // creation, access, write times
        d.extend_from_slice(&((f.size >> 32) as u32).to_le_bytes());
        d.extend_from_slice(&(f.size as u32).to_le_bytes());
        let name: Vec<u16> = f.path.replace('/', "\\").encode_utf16().take(259).collect();
        for u in &name {
            d.extend_from_slice(&u.to_le_bytes());
        }
        d.resize(start + FILEDESCRIPTORW_LEN, 0);
    }
    d
}

/// Decode a `FileGroupDescriptorW`. Entries with unsafe names are dropped.
pub fn decode_file_group(d: &[u8]) -> Option<Vec<FileDescriptor>> {
    let count = u32le(d, 0)? as usize;
    if count > MAX_FILES || d.len() < 4 + count * FILEDESCRIPTORW_LEN {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let e = &d[4 + i * FILEDESCRIPTORW_LEN..][..FILEDESCRIPTORW_LEN];
        let flags = u32le(e, 0)?;
        let attrs = if flags & FD_ATTRIBUTES != 0 {
            u32le(e, 36)?
        } else {
            0
        };
        let size = if flags & FD_FILESIZE != 0 {
            ((u32le(e, 64)? as u64) << 32) | u32le(e, 68)? as u64
        } else {
            0
        };
        let units: Vec<u16> = e[72..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .take_while(|&u| u != 0)
            .collect();
        if let Some(path) = sanitize_relative_path(&String::from_utf16_lossy(&units)) {
            out.push(FileDescriptor {
                path,
                size,
                is_dir: attrs & FILE_ATTRIBUTE_DIRECTORY != 0,
            });
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn text_round_trip_and_crlf() {
        let e = encode_text("a\nb\r\nc ✓ 𝄞");
        assert_eq!(&e[e.len() - 2..], &[0, 0]);
        assert_eq!(decode_text(&e), "a\nb\nc ✓ 𝄞");
        // Windows text: CRLF on the wire.
        assert_eq!(&e[2..6], &[b'\r', 0, b'\n', 0]);
        assert_eq!(decode_text(&[b'h', 0, b'i', 0, 0, 0, b'x', 0]), "hi");
    }

    #[test]
    fn html_round_trip() {
        let frag = "<b>bold</b> & <i>ünïcode</i>";
        let enc = encode_html(frag);
        assert_eq!(decode_html(&enc), frag);
        let text = String::from_utf8(enc).unwrap();
        assert!(text.starts_with("Version:0.9\r\nStartHTML:"));
        // Offsets point at the right bytes.
        assert!(decode_html(b"no header <p>x</p>").contains("<p>x</p>"));
    }

    #[test]
    fn dib_windows_24bpp_bottom_up() {
        // 2x2, 24 bpp, rows padded to 8 bytes. Bottom row first: blue, green; top: red, white.
        let mut d = Vec::new();
        for v in [40u32, 2, 2] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&24u16.to_le_bytes());
        d.extend_from_slice(&[0; 24]);
        d.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0]); // bottom: blue, green (BGR)
        d.extend_from_slice(&[0, 0, 255, 255, 255, 255, 0, 0]); // top: red, white
        let img = dib_to_image(&d).unwrap();
        assert_eq!(
            img.rgba,
            vec![
                255, 0, 0, 255, 255, 255, 255, 255, 0, 0, 255, 255, 0, 255, 0, 255
            ]
        );
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(dib_to_image(&[]), None);
        assert_eq!(dib_to_image(&[40, 0, 0, 0]), None);
        let mut huge = image_to_dib(&Image {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        });
        huge[4..8].copy_from_slice(&100_000i32.to_le_bytes());
        huge[8..12].copy_from_slice(&100_000i32.to_le_bytes());
        assert_eq!(dib_to_image(&huge), None);
    }

    #[test]
    fn wire_mapping() {
        for f in [
            ClipFormat::Text,
            ClipFormat::Html,
            ClipFormat::Png,
            ClipFormat::Dib,
        ] {
            let (id, name) = f.wire();
            assert_eq!(ClipFormat::from_wire(id, name), Some(f));
        }
        assert_eq!(ClipFormat::from_wire(CF_DIBV5, None), Some(ClipFormat::Dib));
        assert_eq!(
            ClipFormat::from_wire(0xC123, Some("Rich Text Format")),
            None
        );
    }

    #[test]
    fn file_group_round_trip() {
        let files = vec![
            FileDescriptor {
                path: "dir".into(),
                size: 0,
                is_dir: true,
            },
            FileDescriptor {
                path: "dir/räksmörgås.txt".into(),
                size: 5_000_000_000,
                is_dir: false,
            },
        ];
        let enc = encode_file_group(&files);
        assert_eq!(enc.len(), 4 + 2 * 592);
        assert_eq!(decode_file_group(&enc).unwrap(), files);
        assert_eq!(decode_file_group(&enc[..100]), None);
    }

    #[test]
    fn hostile_paths_are_neutralised() {
        assert_eq!(
            sanitize_relative_path("..\\..\\etc\\passwd").as_deref(),
            Some("etc/passwd")
        );
        assert_eq!(sanitize_relative_path("/abs/x").as_deref(), Some("abs/x"));
        assert_eq!(sanitize_relative_path("C:\\Windows\\x"), None);
        assert_eq!(sanitize_relative_path("a\\.\\b").as_deref(), Some("a/b"));
        assert_eq!(sanitize_relative_path(".."), None);
        assert_eq!(sanitize_relative_path("bad\u{1}name"), None);
        let evil = encode_file_group(&[FileDescriptor {
            path: "../../x".into(),
            size: 1,
            is_dir: false,
        }]);
        assert_eq!(decode_file_group(&evil).unwrap()[0].path, "x");
        assert_eq!(
            ClipFormat::from_wire(0xC0FF, Some(FILES_FORMAT_NAME)),
            Some(ClipFormat::Files)
        );
    }

    proptest! {
        #[test]
        fn dib_round_trip(w in 1u32..40, h in 1u32..40, seed in any::<u64>()) {
            let mut x = seed | 1;
            let rgba: Vec<u8> = (0..w * h * 4).map(|_| { x ^= x << 13; x ^= x >> 7; x ^= x << 17; x as u8 }).collect();
            let img = Image { width: w, height: h, rgba };
            prop_assert_eq!(dib_to_image(&image_to_dib(&img)).unwrap(), img);
        }

        #[test]
        fn text_round_trip_any(s in "[^\r\0]*") {
            prop_assert_eq!(decode_text(&encode_text(&s)), s);
        }
    }
}
