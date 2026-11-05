//! Input methods for WebKit views: each view's `WPEInputMethodContext`, between WebKit's editor
//! and the host's text input (`zwp_text_input_v3`).
//!
//! WebKit tells the context when an editable element takes or loses focus, where its caret is
//! (in the view's logical coordinates, the target's surface-local ones) and the text around the
//! cursor (UTF-8 byte offsets, as the api has them); the context hands that to the view's sink as
//! [`Ime`], and the engine reports it as `TextInputChanged`. The host's updates go the other
//! way through [`InputMethod::apply`], as the signals WebKit's input-method filter listens to:
//! delete around the cursor, commit, then the new preedit. Key events are never filtered here:
//! an input method that wants keys takes them from the compositor, so text never arrives twice.
//!
//! Must not own: the input-method protocol (the host) or focus policy (the core).

use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_uint};
use std::ptr;

use glib::ffi::{gboolean, GList, GFALSE};
use glib::prelude::*;
use glib::subclass::prelude::*;
use glib::translate::*;
use sighurt_api::geometry::SurfaceRect;
use sighurt_api::input::{DeleteSurrounding, SurroundingText, TextInputEvent, TextPurpose};

use crate::ffi;
use crate::platform::{Call, View, WpeView};

/// What a view's input-method context reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ime {
    /// An editable element took (`true`) or lost focus.
    Focus(bool),
    /// The caret moved.
    Caret(SurfaceRect),
    /// The text around the cursor changed.
    Surrounding(SurroundingText),
}

glib::wrapper! {
    /// WebKit's `WPEInputMethodContext`, the parent of [`InputMethod`].
    pub struct WpeInputMethodContext(Object<ffi::WPEInputMethodContext, ffi::WPEInputMethodContextClass>);
    match fn { type_ => || ffi::wpe_input_method_context_get_type(), }
}

/// The `WPEInputMethodContext` virtual methods a subclass provides.
pub trait InputMethodImpl: ObjectImpl + ObjectSubclass<Type: IsA<WpeInputMethodContext>> {
    /// The composition shown, and its cursor in characters.
    fn preedit(&self) -> (String, u32);
    fn report(&self, ime: Ime);
    fn reset(&self);
}

unsafe impl<T: InputMethodImpl> IsSubclassable<T> for WpeInputMethodContext {
    fn class_init(class: &mut glib::Class<Self>) {
        Self::parent_class_init::<T>(class);
        let klass = class.as_mut();
        klass.get_preedit_string = Some(preedit_string::<T>);
        klass.filter_key_event = Some(filter_key_event);
        klass.focus_in = Some(focus_in::<T>);
        klass.focus_out = Some(focus_out::<T>);
        klass.set_cursor_area = Some(cursor_area::<T>);
        klass.set_surrounding = Some(surrounding::<T>);
        klass.reset = Some(reset::<T>);
    }
}

unsafe fn imp<'a, T: InputMethodImpl>(context: *mut ffi::WPEInputMethodContext) -> &'a T {
    unsafe { (*(context as *mut T::Instance)).imp() }
}

unsafe extern "C" fn preedit_string<T: InputMethodImpl>(
    context: *mut ffi::WPEInputMethodContext,
    text: *mut *mut c_char,
    underlines: *mut *mut GList,
    cursor: *mut c_uint,
) {
    let (preedit, offset) = unsafe { imp::<T>(context) }.preedit();
    unsafe {
        if !text.is_null() {
            *text = preedit.to_glib_full();
        }
        // None: WebKit underlines the whole composition.
        if !underlines.is_null() {
            *underlines = ptr::null_mut();
        }
        if !cursor.is_null() {
            *cursor = offset;
        }
    }
}

unsafe extern "C" fn filter_key_event(
    _: *mut ffi::WPEInputMethodContext,
    _: *mut ffi::WPEEvent,
) -> gboolean {
    GFALSE
}

unsafe extern "C" fn focus_in<T: InputMethodImpl>(context: *mut ffi::WPEInputMethodContext) {
    unsafe { imp::<T>(context) }.report(Ime::Focus(true));
}

unsafe extern "C" fn focus_out<T: InputMethodImpl>(context: *mut ffi::WPEInputMethodContext) {
    let imp = unsafe { imp::<T>(context) };
    imp.reset();
    imp.report(Ime::Focus(false));
}

unsafe extern "C" fn cursor_area<T: InputMethodImpl>(
    context: *mut ffi::WPEInputMethodContext,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
) {
    let caret = SurfaceRect {
        x,
        y,
        width: width.max(0) as u32,
        height: height.max(0) as u32,
    };
    unsafe { imp::<T>(context) }.report(Ime::Caret(caret));
}

unsafe extern "C" fn surrounding<T: InputMethodImpl>(
    context: *mut ffi::WPEInputMethodContext,
    text: *const c_char,
    length: c_uint,
    cursor: c_uint,
    anchor: c_uint,
) {
    if text.is_null() {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(text.cast::<u8>(), length as usize) };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    let (cursor, anchor) = (cursor as usize, anchor as usize);
    if !text.is_char_boundary(cursor) || !text.is_char_boundary(anchor) {
        return;
    }
    let surrounding = SurroundingText {
        text: text.to_owned(),
        cursor,
        anchor,
    };
    unsafe { imp::<T>(context) }.report(Ime::Surrounding(surrounding));
}

unsafe extern "C" fn reset<T: InputMethodImpl>(context: *mut ffi::WPEInputMethodContext) {
    unsafe { imp::<T>(context) }.reset();
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct InputMethod {
        /// The composition shown, and its cursor in characters.
        pub preedit: RefCell<Option<(String, u32)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for InputMethod {
        const NAME: &'static str = "SighurtWPEInputMethod";
        type Type = super::InputMethod;
        type ParentType = WpeInputMethodContext;
    }

    impl ObjectImpl for InputMethod {}

    impl InputMethodImpl for InputMethod {
        fn preedit(&self) -> (String, u32) {
            self.preedit.borrow().clone().unwrap_or_default()
        }

        fn report(&self, ime: Ime) {
            let view: Option<WpeView> = self.obj().property("view");
            if let Some(view) = view.and_then(|v| v.downcast::<View>().ok()) {
                view.call(Call::Ime(ime));
            }
        }

        fn reset(&self) {
            self.preedit.replace(None);
        }
    }
}

glib::wrapper! {
    /// Sighurt's `WPEInputMethodContext`, one per view. See the module documentation.
    pub struct InputMethod(ObjectSubclass<imp::InputMethod>) @extends WpeInputMethodContext;
}

impl InputMethod {
    pub fn new(view: &WpeView) -> Self {
        glib::Object::builder().property("view", view).build()
    }

    /// What the focused field is for.
    pub fn purpose(&self) -> TextPurpose {
        let raw = self.upcast_ref::<WpeInputMethodContext>().to_glib_none().0;
        purpose(unsafe { ffi::wpe_input_method_context_get_input_purpose(raw) })
    }

    /// Carries out the host's update: deletes around the cursor (as characters of `surrounding`,
    /// the text WebKit reported last), commits, then shows the new composition.
    pub fn apply(&self, event: &TextInputEvent, surrounding: Option<&SurroundingText>) {
        if let Some((offset, count)) = event
            .delete_surrounding
            .and_then(|d| surrounding.and_then(|s| deletion(s, d)))
        {
            self.emit_by_name::<()>("delete-surrounding", &[&offset, &count]);
        }
        if let Some(text) = &event.commit {
            // WebKit's filter ends the composition with the commit.
            self.imp().preedit.replace(None);
            self.emit_by_name::<()>("committed", &[text]);
        }
        let had = self.imp().preedit.borrow().is_some();
        match event.preedit.as_ref().filter(|p| !p.text.is_empty()) {
            Some(preedit) => {
                let end = preedit
                    .cursor
                    .as_ref()
                    .map_or(preedit.text.len(), |c| c.start);
                let cursor = preedit.text.get(..end).map_or(0, |s| s.chars().count());
                self.imp()
                    .preedit
                    .replace(Some((preedit.text.clone(), cursor as u32)));
                if !had {
                    self.emit_by_name::<()>("preedit-started", &[]);
                }
                self.emit_by_name::<()>("preedit-changed", &[]);
            }
            None if had => {
                self.imp().preedit.replace(None);
                self.emit_by_name::<()>("preedit-changed", &[]);
                self.emit_by_name::<()>("preedit-finished", &[]);
            }
            None => {}
        }
    }
}

/// WPE's input purpose as the api's.
fn purpose(raw: c_uint) -> TextPurpose {
    match raw {
        ffi::WPE_INPUT_PURPOSE_PASSWORD | ffi::WPE_INPUT_PURPOSE_PIN => TextPurpose::Password,
        ffi::WPE_INPUT_PURPOSE_DIGITS | ffi::WPE_INPUT_PURPOSE_NUMBER => TextPurpose::Number,
        ffi::WPE_INPUT_PURPOSE_PHONE => TextPurpose::Phone,
        ffi::WPE_INPUT_PURPOSE_URL => TextPurpose::Url,
        ffi::WPE_INPUT_PURPOSE_EMAIL => TextPurpose::Email,
        _ => TextPurpose::Normal,
    }
}

/// The host's deletion in UTF-8 bytes around the cursor as WebKit's: an offset from the cursor
/// and a count, in characters. `None` when it doesn't fall on characters of `surrounding`.
fn deletion(surrounding: &SurroundingText, delete: DeleteSurrounding) -> Option<(i32, u32)> {
    let text = &surrounding.text;
    let cursor = surrounding.cursor;
    let start = cursor.checked_sub(delete.before as usize)?;
    let end = cursor.checked_add(delete.after as usize)?;
    let before = text.get(start..cursor)?.chars().count();
    let after = text.get(cursor..end)?.chars().count();
    Some((-(before as i32), (before + after) as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deletions_in_bytes_become_characters() {
        let text = |cursor| SurroundingText {
            text: "aåäö b".into(),
            cursor,
            anchor: cursor,
        };
        let delete = |before, after| DeleteSurrounding { before, after };
        // "ä" and "ö" before a cursor after "ö" (byte 7): 4 bytes, 2 characters.
        assert_eq!(deletion(&text(7), delete(4, 0)), Some((-2, 2)));
        // One character each side.
        assert_eq!(deletion(&text(7), delete(2, 1)), Some((-1, 2)));
        // Through the middle of "ä": not characters.
        assert_eq!(deletion(&text(7), delete(1, 0)), None);
        // Past either end.
        assert_eq!(deletion(&text(1), delete(2, 0)), None);
        assert_eq!(deletion(&text(7), delete(0, 9)), None);
    }

    #[test]
    fn purposes() {
        assert_eq!(
            purpose(ffi::WPE_INPUT_PURPOSE_PASSWORD),
            TextPurpose::Password
        );
        assert_eq!(purpose(ffi::WPE_INPUT_PURPOSE_PIN), TextPurpose::Password);
        assert_eq!(purpose(ffi::WPE_INPUT_PURPOSE_EMAIL), TextPurpose::Email);
        assert_eq!(purpose(0), TextPurpose::Normal);
    }

    /// The signals WebKit's input-method filter would see, on WebKit's own context class.
    #[test]
    fn host_updates_become_webkits_signals_in_order() {
        use sighurt_api::input::Preedit;
        use std::rc::Rc;

        ffi::load().expect("WPE WebKit");
        let context: InputMethod = glib::Object::new();
        let seen = Rc::new(RefCell::new(Vec::<String>::new()));
        for signal in ["preedit-started", "preedit-changed", "preedit-finished"] {
            let seen = Rc::clone(&seen);
            context.connect_local(signal, false, move |_| {
                seen.borrow_mut().push(signal.into());
                None
            });
        }
        let s = Rc::clone(&seen);
        context.connect_local("committed", false, move |args| {
            s.borrow_mut()
                .push(format!("committed {}", args[1].get::<String>().unwrap()));
            None
        });
        let s = Rc::clone(&seen);
        context.connect_local("delete-surrounding", false, move |args| {
            let (offset, count) = (args[1].get::<i32>().unwrap(), args[2].get::<u32>().unwrap());
            s.borrow_mut().push(format!("delete {offset} {count}"));
            None
        });
        let preedit = |text: &str, cursor| TextInputEvent {
            delete_surrounding: None,
            commit: None,
            preedit: Some(Preedit {
                text: text.into(),
                cursor,
            }),
            paste: false,
        };

        context.apply(&preedit("åä", Some(2..2)), None);
        assert_eq!(context.imp().preedit(), ("åä".into(), 1));
        context.apply(&preedit("åäö", None), None);
        assert_eq!(context.imp().preedit(), ("åäö".into(), 3));
        let surrounding = SurroundingText {
            text: "xyz".into(),
            cursor: 3,
            anchor: 3,
        };
        let commit = TextInputEvent {
            delete_surrounding: Some(DeleteSurrounding {
                before: 1,
                after: 0,
            }),
            commit: Some("ö".into()),
            preedit: None,
            paste: false,
        };
        context.apply(&commit, Some(&surrounding));
        assert_eq!(context.imp().preedit(), (String::new(), 0));
        // A composition cancelled without text.
        context.apply(&preedit("a", None), None);
        context.apply(&TextInputEvent::default(), None);
        assert_eq!(
            *seen.borrow(),
            [
                "preedit-started",
                "preedit-changed",
                "preedit-changed",
                "delete -1 1",
                "committed ö",
                "preedit-started",
                "preedit-changed",
                "preedit-changed",
                "preedit-finished",
            ]
        );
    }
}
