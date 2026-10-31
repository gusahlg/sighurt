//! The shim's C ABI (`shim/sighurt_ladybird.h`) and the library's loading.
//!
//! The library is not linked: [`load`] opens it (`dlopen`, `RTLD_NOW | RTLD_LOCAL`) when the
//! runtime first starts, so a program with the engine compiled in maps Ladybird's libraries only
//! when its first Ladybird page opens. The library finds its own dependencies through its RPATH.
//! [`load`] also compares what the shim says it was built with against what it measures in the
//! libraries at run time (the ABI-skew guard, docs/rewrite/LADYBIRD.md L5): a mismatch is a load
//! error, never a crash later.
//!
//! Owns: the C declarations and `dlopen`. Must not own: what any call means for the browser.

use std::ffi::{c_char, c_double, c_int, c_void, CStr, CString};
use std::sync::OnceLock;

/// `LB_ABI_VERSION` of the header this crate was written against.
pub const ABI_VERSION: u32 = 1;

pub const EV_VIEW_READY: c_int = 1;
pub const EV_LOAD_START: c_int = 2;
pub const EV_URL_CHANGED: c_int = 3;
pub const EV_LOAD_FINISH: c_int = 4;
pub const EV_TITLE: c_int = 5;
pub const EV_LOADING: c_int = 6;
pub const EV_CURSOR: c_int = 7;
pub const EV_HOVER: c_int = 8;
pub const EV_OPEN_REQUESTED: c_int = 9;
pub const EV_FRAME: c_int = 10;
pub const EV_CRASHED: c_int = 11;
pub const EV_CLOSED: c_int = 12;
pub const EV_HISTORY: c_int = 13;
pub const EV_JS_RESULT: c_int = 14;
pub const EV_INPUT_METHOD: c_int = 15;
pub const EV_FIND_RESULT: c_int = 16;
pub const EV_CLIPBOARD_WRITE: c_int = 17;
pub const EV_CLIPBOARD_WANT: c_int = 18;
pub const EV_DOWNLOAD: c_int = 19;
pub const EV_PICK_FILES: c_int = 20;
pub const EV_OPEN_EXTERNAL: c_int = 21;
pub const EV_NAVIGATION_REQUESTED: c_int = 22;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Abi {
    pub version: u32,
    pub sizeof_shared_image_buffer: u32,
    pub sizeof_view_implementation: u32,
    pub sizeof_headless_web_view: u32,
    pub sizeof_bitmap: u32,
    pub vulkan_dmabuf: u32,
    pub probe_shared_image_buffer: u32,
    pub probe_view_implementation: u32,
}

#[repr(C)]
pub struct Event {
    pub kind: c_int,
    pub view: u64,
    pub i0: i64,
    pub i1: i64,
    pub s0: *const c_char,
    pub s1: *const c_char,
    pub data: *const c_void,
    pub len: usize,
}

#[repr(C)]
pub struct Frame {
    pub pixels: *const u8,
    pub width: c_int,
    pub height: c_int,
    pub stride: usize,
    pub dmabuf: c_int,
}

macro_rules! functions {
    ($(pub fn $name:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)?;)*) => {
        /// The shim's functions, resolved once.
        pub struct Functions {
            $($name: unsafe extern "C" fn($($ty),*) $(-> $ret)?,)*
        }

        impl Functions {
            fn resolve(library: *mut c_void) -> Result<Self, String> {
                Ok(Self {
                    $($name: unsafe {
                        std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($ty),*) $(-> $ret)?>(
                            symbol(library, concat!(stringify!($name), "\0"))?,
                        )
                    },)*
                })
            }
        }

        $(
            /// The shim's function of this name.
            ///
            /// # Safety
            ///
            /// As the C function's, on the thread that called `lb_init`; [`load`] must have succeeded.
            #[allow(clippy::too_many_arguments)] // C's signature.
            pub unsafe fn $name($($arg: $ty),*) $(-> $ret)? {
                let functions = FUNCTIONS.get().expect("the Ladybird shim is loaded (ffi::load)");
                unsafe { (functions.$name)($($arg),*) }
            }
        )*
    };
}

static FUNCTIONS: OnceLock<Functions> = OnceLock::new();

/// The shim as the build script found it (the `SIGHURT_LADYBIRD_SHIM` environment variable, at
/// run time, names another).
pub const DEFAULT_LIBRARY: &str = env!("SIGHURT_LADYBIRD_SHIM_DEFAULT");

fn dl_error() -> String {
    let error = unsafe { libc::dlerror() };
    match error.is_null() {
        true => "unknown error".into(),
        false => unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned(),
    }
}

fn symbol(library: *mut c_void, name: &str) -> Result<*mut c_void, String> {
    let address = unsafe { libc::dlsym(library, name.as_ptr().cast()) };
    match address.is_null() {
        true => Err(format!(
            "the Ladybird shim has no {}",
            name.trim_end_matches('\0')
        )),
        false => Ok(address),
    }
}

functions! {
    pub fn lb_abi_describe(out: *mut Abi);
    pub fn lb_init(argv: *const *const c_char, argc: c_int, profile: *const c_char, prefix: *const c_char, err: *mut c_char, err_len: usize) -> c_int;
    pub fn lb_loop_fd() -> c_int;
    pub fn lb_pump(more: *mut c_int) -> i64;
    pub fn lb_next_event(out: *mut Event) -> c_int;
    pub fn lb_wake_count() -> u64;
    pub fn lb_view_open(width: c_int, height: c_int, scale: c_double, visible: c_int, zoom: c_double) -> u64;
    pub fn lb_view_close(view: u64);
    pub fn lb_navigate(view: u64, url: *const c_char);
    pub fn lb_reload(view: u64);
    pub fn lb_stop(view: u64);
    pub fn lb_traverse(view: u64, delta: c_int);
    pub fn lb_set_viewport(view: u64, width: c_int, height: c_int, scale: c_double);
    pub fn lb_set_visible(view: u64, visible: c_int);
    pub fn lb_set_focus(view: u64, focused: c_int);
    pub fn lb_set_zoom(view: u64, zoom: c_double);
    pub fn lb_run_js(view: u64, script: *const c_char, request: i64);
    pub fn lb_input_key(view: u64, code: *const c_char, key: *const c_char, pressed: c_int, repeat: c_int, modifiers: c_int, code_point: u32);
    pub fn lb_input_pointer_move(view: u64, x: c_int, y: c_int, modifiers: c_int);
    pub fn lb_input_pointer_leave(view: u64);
    pub fn lb_input_pointer_button(view: u64, x: c_int, y: c_int, button: c_int, pressed: c_int, click_count: c_int, modifiers: c_int);
    pub fn lb_input_scroll(view: u64, x: c_int, y: c_int, dx: c_double, dy: c_double, precise: c_int, modifiers: c_int);
    pub fn lb_input_commit_text(view: u64, text: *const c_char);
    pub fn lb_input_preedit(view: u64, text: *const c_char);
    pub fn lb_clipboard_grant(view: u64, data: *const u8, len: usize);
    pub fn lb_clipboard_deny(view: u64);
    pub fn lb_pick_reply(token: i64, paths: *const c_char);
    pub fn lb_open_external_reply(token: i64, allowed: c_int);
    pub fn lb_navigation_answer(token: i64, allow: c_int);
    pub fn lb_frame_get(view: u64, out: *mut Frame) -> c_int;
    pub fn lb_shutdown();
}

/// Whether [`load`] has succeeded.
pub fn is_loaded() -> bool {
    FUNCTIONS.get().is_some()
}

/// Opens the shim at `path` (or the default, or `SIGHURT_LADYBIRD_SHIM`), resolves every function
/// and runs the ABI guard. Idempotent.
pub fn load(path: Option<&str>) -> Result<(), String> {
    if is_loaded() {
        return Ok(());
    }
    let from_env = std::env::var("SIGHURT_LADYBIRD_SHIM").ok();
    let path = path.or(from_env.as_deref()).unwrap_or(DEFAULT_LIBRARY);
    let c_path = CString::new(path).map_err(|_| "the shim's path has a NUL".to_string())?;
    let library = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if library.is_null() {
        return Err(format!(
            "the Ladybird shim {path} can't be loaded: {} (build it with nix/build-ladybird-shim.sh)",
            dl_error()
        ));
    }
    let functions = Functions::resolve(library)?;
    let mut abi = Abi::default();
    unsafe { (functions.lb_abi_describe)(&mut abi) };
    check_abi(&abi)?;
    let _ = FUNCTIONS.set(functions);
    Ok(())
}

/// The ABI guard: what the shim was compiled against, and what it measured in the libraries.
pub fn check_abi(abi: &Abi) -> Result<(), String> {
    if abi.version != ABI_VERSION {
        return Err(format!(
            "the Ladybird shim speaks ABI {}, this engine {ABI_VERSION}",
            abi.version
        ));
    }
    // The shim read a buffer its libraries made with the layout of the headers it was built
    // with: a zero means they disagree.
    if abi.probe_shared_image_buffer != 1 {
        return Err(
            "the Ladybird shim was built against headers that do not match its libraries \
                    (the shared-buffer probe failed); rebuild it with nix/build-ladybird-shim.sh"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guard_refuses_a_skewed_shim() {
        let mut abi = Abi {
            version: ABI_VERSION,
            probe_shared_image_buffer: 1,
            ..Abi::default()
        };
        assert!(check_abi(&abi).is_ok());
        abi.probe_shared_image_buffer = 0;
        assert!(check_abi(&abi).unwrap_err().contains("do not match"));
        abi.probe_shared_image_buffer = 1;
        abi.version += 1;
        assert!(check_abi(&abi).unwrap_err().contains("ABI"));
    }
}
