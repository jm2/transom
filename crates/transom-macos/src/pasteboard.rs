// SPDX-License-Identifier: GPL-3.0-or-later
//! What the general pasteboard says about its content, for what GDK can't see: GDK
//! maps pasteboard types it doesn't know to `application/octet-stream`, so password
//! managers' markers are invisible through it, and it looks for changes only when the
//! application becomes active.
use objc2::msg_send;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject};
use objc2_foundation::NSString;

/// The markers password managers put on what they copy (nspasteboard.org): concealed
/// (a secret) and transient (not to be kept).
const MARKERS: [&str; 2] = [
    "org.nspasteboard.ConcealedType",
    "org.nspasteboard.TransientType",
];

/// The state of the general pasteboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pasteboard {
    /// `changeCount`: changes whenever any application writes the pasteboard.
    pub change_count: i64,
    /// The current content carries a concealed or transient marker.
    pub concealed: bool,
}

/// The general pasteboard's change count and markers, or `None` if AppKit isn't
/// loaded. Call it on the main thread.
pub fn pasteboard() -> Option<Pasteboard> {
    let class = AnyClass::get(c"NSPasteboard")?;
    autoreleasepool(|_| {
        // SAFETY: +[NSPasteboard generalPasteboard] takes no arguments and returns an
        // NSPasteboard (nonnull in the SDK; `Option` in case).
        let pb: Option<Retained<AnyObject>> = unsafe { msg_send![class, generalPasteboard] };
        let pb = pb?;
        // SAFETY: -[NSPasteboard changeCount] takes no arguments and returns NSInteger.
        let change_count: isize = unsafe { msg_send![&*pb, changeCount] };
        // SAFETY: -[NSPasteboard types] takes no arguments and returns a nullable
        // NSArray<NSPasteboardType> (NSArray of NSString).
        let types: Option<Retained<AnyObject>> = unsafe { msg_send![&*pb, types] };
        let concealed = types.is_some_and(|types| {
            MARKERS.iter().any(|m| {
                let m = NSString::from_str(m);
                // SAFETY: -[NSArray containsObject:(id)] returns BOOL; `m` is a live
                // NSString for the duration of the call.
                unsafe { msg_send![&*types, containsObject: &*m] }
            })
        });
        Some(Pasteboard {
            change_count: change_count as i64,
            concealed,
        })
    })
}
