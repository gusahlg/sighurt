//! WebKit's clipboard (`WPEClipboard`), which never holds page data and never reads the
//! desktop's clipboard by itself: both go through the browser's service requests.
//!
//! WebKit has one clipboard for every view (the primary display's) and uses it synchronously:
//! a page that copies sets its content (`changed`, local), and a paste reads it (`read`) inside
//! WebKit's handling of the web process's message. So:
//!
//! - A page's copy is handed to the engine as [`ClipboardCall::Written`], for a
//!   `ClipboardWrite` request, and WebKit keeps nothing: another page can't paste it unless the
//!   browser put it on the desktop's clipboard.
//! - The clipboard always offers plain text. A paste reads it: with nothing granted, the read
//!   finds nothing and the engine is told [`ClipboardCall::Wanted`], for a `ClipboardRead`
//!   request; once the browser answers, the engine [`grant`](Clipboard::grant)s the text for
//!   [`GRANT`] and pastes again, and that paste finds it. The page sees a first paste with nothing
//!   in it (a `paste` event without data), then the real one. The grant is the view's that
//!   asked: the runtime [`revoke`](Clipboard::revoke)s it when the user's input goes to another
//!   view, or that view closes.
//!
//! WebKit refuses the async clipboard API's reads in WPE itself (`requestDOMPasteAccess`), so a
//! read only comes from a paste the user started, in the view with the user's latest input.
//!
//! Must not own: policy (the core decides) or the desktop's clipboard (the host has it).

use std::cell::RefCell;
use std::ffi::{c_char, CStr};
use std::ptr;
use std::rc::Rc;
use std::time::{Duration, Instant};

use glib::ffi::{gboolean, GBytes, GPtrArray, GFALSE};
use glib::prelude::*;
use glib::subclass::prelude::*;
use glib::translate::*;
use sighurt_api::services::ClipboardData;

use crate::ffi;
use crate::platform::WpeDisplay;

/// How long text the browser handed over stays readable: the paste it was asked for.
pub const GRANT: Duration = Duration::from_secs(1);

/// The types the clipboard offers and pastes read.
pub const TEXT: [&CStr; 2] = [c"text/plain;charset=utf-8", c"text/plain"];

/// What the clipboard tells the engine, from inside WebKit's calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardCall {
    /// A page copied this, in each type WebKit had.
    Written(Vec<ClipboardData>),
    /// A paste found nothing granted.
    Wanted,
}

/// Where the clipboard's calls go. Only queues.
pub type ClipboardSink = Rc<dyn Fn(ClipboardCall)>;

glib::wrapper! {
    /// WebKit's `WPEClipboard`, the parent of [`Clipboard`].
    pub struct WpeClipboard(Object<ffi::WPEClipboard, ffi::WPEClipboardClass>);
    match fn { type_ => || ffi::wpe_clipboard_get_type(), }
}

/// The `WPEClipboard` virtual methods a subclass provides.
pub trait ClipboardImpl: ObjectImpl + ObjectSubclass<Type: IsA<WpeClipboard>> {
    /// The clipboard's content in `format`, if there is some to give.
    fn read(&self, format: &str) -> Option<Vec<u8>>;
    /// WebKit set the clipboard's content (a page copied): `content` may be null.
    fn changed(&self, content: *mut ffi::WPEClipboardContent);
}

unsafe impl<T: ClipboardImpl> IsSubclassable<T> for WpeClipboard {
    fn class_init(class: &mut glib::Class<Self>) {
        Self::parent_class_init::<T>(class);
        let klass = class.as_mut();
        klass.read = Some(clipboard_read::<T>);
        klass.changed = Some(clipboard_changed::<T>);
    }
}

unsafe extern "C" fn clipboard_read<T: ClipboardImpl>(
    clipboard: *mut ffi::WPEClipboard,
    format: *const c_char,
) -> *mut GBytes {
    let instance = unsafe { &*(clipboard as *mut T::Instance) };
    let format = unsafe { CStr::from_ptr(format) }.to_string_lossy();
    match instance.imp().read(&format) {
        Some(bytes) => unsafe { glib::ffi::g_bytes_new(bytes.as_ptr().cast(), bytes.len()) },
        None => ptr::null_mut(),
    }
}

unsafe extern "C" fn clipboard_changed<T: ClipboardImpl>(
    clipboard: *mut ffi::WPEClipboard,
    _formats: *mut GPtrArray,
    _is_local: gboolean,
    content: *mut ffi::WPEClipboardContent,
) {
    let instance = unsafe { &*(clipboard as *mut T::Instance) };
    instance.imp().changed(content);
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Clipboard {
        pub sink: RefCell<Option<ClipboardSink>>,
        /// Text the browser handed over, and when.
        pub granted: RefCell<Option<(String, Instant)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Clipboard {
        const NAME: &'static str = "SighurtWPEClipboard";
        type Type = super::Clipboard;
        type ParentType = WpeClipboard;
    }

    impl ObjectImpl for Clipboard {}

    impl ClipboardImpl for Clipboard {
        fn read(&self, _format: &str) -> Option<Vec<u8>> {
            let granted = self.granted.borrow();
            match granted.as_ref() {
                Some((text, at)) if at.elapsed() < GRANT => Some(text.clone().into_bytes()),
                _ => {
                    drop(granted);
                    self.call(ClipboardCall::Wanted);
                    None
                }
            }
        }

        fn changed(&self, content: *mut ffi::WPEClipboardContent) {
            if !content.is_null() {
                let items = unsafe { items(content) };
                if !items.is_empty() {
                    self.call(ClipboardCall::Written(items));
                }
            }
            // WebKit keeps no page data: it only learns that text can be pasted.
            self.obj().offer_text();
        }
    }

    impl Clipboard {
        fn call(&self, call: ClipboardCall) {
            let sink = self.sink.borrow().clone();
            if let Some(sink) = sink {
                sink(call);
            }
        }
    }
}

glib::wrapper! {
    /// Sighurt's `WPEClipboard`. See the module documentation.
    pub struct Clipboard(ObjectSubclass<imp::Clipboard>) @extends WpeClipboard;
}

impl Clipboard {
    pub fn new(display: &WpeDisplay, sink: ClipboardSink) -> Self {
        let clipboard: Self = glib::Object::builder().property("display", display).build();
        clipboard.imp().sink.replace(Some(sink));
        clipboard.offer_text();
        clipboard
    }

    /// Makes `text` what pastes read for the next [`GRANT`].
    pub fn grant(&self, text: String) {
        self.imp().granted.replace(Some((text, Instant::now())));
    }

    /// Forgets granted text: the view it was for no longer has the user's input.
    pub fn revoke(&self) {
        self.imp().granted.replace(None);
    }

    /// Forgets granted text that is too old to read.
    pub fn expire(&self) {
        let mut granted = self.imp().granted.borrow_mut();
        if granted
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= GRANT)
        {
            *granted = None;
        }
    }

    /// Tells WebKit, through `WPEClipboard`'s own `changed`, that plain text can be read and that
    /// it isn't local, so reads come to [`ClipboardImpl::read`].
    fn offer_text(&self) {
        unsafe {
            let formats = glib::ffi::g_ptr_array_new();
            for format in TEXT {
                let interned = glib::ffi::g_intern_static_string(format.as_ptr());
                glib::ffi::g_ptr_array_add(formats, interned as *mut _);
            }
            glib::ffi::g_ptr_array_add(formats, ptr::null_mut());
            let parent = imp::Clipboard::type_data().as_ref().parent_class()
                as *const ffi::WPEClipboardClass;
            if let Some(changed) = (*parent).changed {
                let raw = self.upcast_ref::<WpeClipboard>().to_glib_none().0;
                changed(raw, formats, GFALSE, ptr::null_mut());
            }
            glib::ffi::g_ptr_array_unref(formats);
        }
    }
}

/// What a page copied, in the types the browser takes: text, and HTML if WebKit has it.
///
/// # Safety
///
/// `content` must be a live `WPEClipboardContent`.
unsafe fn items(content: *mut ffi::WPEClipboardContent) -> Vec<ClipboardData> {
    let mut items = Vec::new();
    let text = unsafe { ffi::wpe_clipboard_content_get_text(content) };
    if !text.is_null() {
        items.push(ClipboardData {
            mime_type: TEXT[0].to_string_lossy().into_owned(),
            data: unsafe { CStr::from_ptr(text) }.to_bytes().to_vec(),
        });
    }
    let html = unsafe { ffi::wpe_clipboard_content_get_bytes(content, c"text/html".as_ptr()) };
    if !html.is_null() {
        let bytes: glib::Bytes = unsafe { from_glib_none(html) };
        items.push(ClipboardData {
            mime_type: "text/html".into(),
            data: bytes.to_vec(),
        });
    }
    items
}
