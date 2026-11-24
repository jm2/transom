// SPDX-License-Identifier: GPL-3.0-or-later
use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager};

/// `kInternetEventClass` and `kAEGetURL` (both 'GURL'), `keyDirectObject` ('----').
const GURL: u32 = u32::from_be_bytes(*b"GURL");
const KEY_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

struct Ivars {
    callback: Box<dyn Fn(String)>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the class adds no Drop impl.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TransomURLEventHandler"]
    #[ivars = Ivars]
    struct UrlEventHandler;

    impl UrlEventHandler {
        // SAFETY: the signature matches what NSAppleEventManager calls:
        // - (void)handle:(NSAppleEventDescriptor *)event withReplyEvent:(NSAppleEventDescriptor *)reply
        #[unsafe(method(handleGetURLEvent:withReplyEvent:))]
        fn handle_get_url(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            // Typed bindings for this need the CoreServices crate (AEKeyword); a plain
            // message send avoids that dependency.
            // SAFETY: -[NSAppleEventDescriptor paramDescriptorForKeyword:(AEKeyword)]
            // returns a nullable descriptor; AEKeyword is a 32-bit FourCharCode.
            let param: Option<Retained<NSAppleEventDescriptor>> =
                unsafe { msg_send![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT] };
            let url = param.and_then(|d| d.stringValue());
            if let Some(url) = url {
                (self.ivars().callback)(url.to_string());
            }
        }
    }
);

/// Call `f` with every URL macOS asks Transom to open (`rdp://` links), including the
/// one an app launch was started for. `f` runs on the main thread. Call it once, on the
/// main thread, before the application runs.
pub fn on_open_url(f: impl Fn(String) + 'static) {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("transom-macos: on_open_url must be called on the main thread");
        return;
    };
    let this = UrlEventHandler::alloc(mtm).set_ivars(Ivars {
        callback: Box::new(f),
    });
    // SAFETY: NSObject's init on a freshly allocated instance.
    let handler: Retained<UrlEventHandler> = unsafe { msg_send![super(this), init] };
    let manager = NSAppleEventManager::sharedAppleEventManager();
    // SAFETY: the selector names the method defined above, with the signature the
    // event manager expects. The manager doesn't retain its handlers, so the handler
    // is kept for the life of the process below.
    // AEEventClass and AEEventID are 32-bit FourCharCodes.
    unsafe {
        let _: () = msg_send![
            &manager,
            setEventHandler: &*handler,
            andSelector: sel!(handleGetURLEvent:withReplyEvent:),
            forEventClass: GURL,
            andEventID: GURL,
        ];
    }
    std::mem::forget(handler);
}
