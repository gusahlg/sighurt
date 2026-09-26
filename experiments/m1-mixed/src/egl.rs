//! The few EGL calls the experiment makes itself, through the libEGL surfman already loaded.
//!
//! surfman offers no swap-interval control and never terminates an EGL display it didn't open,
//! so these two, and the diagnostic strings, go straight to EGL.

use std::ffi::{c_char, c_void, CStr};

type EglDisplay = *mut c_void;

const EGL_VENDOR: i32 = 0x3053;
const EGL_VERSION: i32 = 0x3054;
const EGL_CLIENT_APIS: i32 = 0x308D;

type GetCurrentDisplay = unsafe extern "C" fn() -> EglDisplay;
type SwapInterval = unsafe extern "C" fn(EglDisplay, i32) -> u32;
type QueryString = unsafe extern "C" fn(EglDisplay, i32) -> *const c_char;
type Terminate = unsafe extern "C" fn(EglDisplay) -> u32;
type GetError = unsafe extern "C" fn() -> i32;

/// Entry points resolved from libEGL.
pub struct Egl {
    get_current_display: GetCurrentDisplay,
    swap_interval: SwapInterval,
    query_string: QueryString,
    terminate: Terminate,
    get_error: GetError,
}

/// The display of the context current on this thread, with the calls that need it.
pub struct Display {
    egl: Egl,
    display: EglDisplay,
}

impl Egl {
    pub fn load() -> Result<Self, String> {
        // SAFETY: dlopen and dlsym with valid C strings. Each symbol is cast to the signature
        // the EGL 1.5 specification gives it.
        unsafe {
            let mut lib =
                libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_LAZY | libc::RTLD_NOLOAD);
            if lib.is_null() {
                lib = libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_LAZY);
            }
            if lib.is_null() {
                return Err("libEGL.so.1 is not loaded".into());
            }
            let symbol = |name: &CStr| {
                let f = libc::dlsym(lib, name.as_ptr());
                if f.is_null() {
                    Err(format!("libEGL has no {name:?}"))
                } else {
                    Ok(f)
                }
            };
            Ok(Self {
                get_current_display: std::mem::transmute::<*mut c_void, GetCurrentDisplay>(symbol(
                    c"eglGetCurrentDisplay",
                )?),
                swap_interval: std::mem::transmute::<*mut c_void, SwapInterval>(symbol(
                    c"eglSwapInterval",
                )?),
                query_string: std::mem::transmute::<*mut c_void, QueryString>(symbol(
                    c"eglQueryString",
                )?),
                terminate: std::mem::transmute::<*mut c_void, Terminate>(symbol(c"eglTerminate")?),
                get_error: std::mem::transmute::<*mut c_void, GetError>(symbol(c"eglGetError")?),
            })
        }
    }

    /// The display of the context current on this thread.
    pub fn current_display(self) -> Result<Display, String> {
        // SAFETY: no arguments; returns EGL_NO_DISPLAY (null) when nothing is current.
        let display = unsafe { (self.get_current_display)() };
        if display.is_null() {
            return Err("no EGL context is current".into());
        }
        Ok(Display { egl: self, display })
    }
}

impl Display {
    /// Sets the swap interval of the current context's draw surface. 0 makes eglSwapBuffers
    /// return without waiting for the compositor's frame callback, which it may withhold forever
    /// for a surface that isn't visible; the host paces frames with its own callbacks instead.
    pub fn set_swap_interval(&self, interval: i32) -> Result<(), String> {
        // SAFETY: a valid, initialized display with a current context.
        if unsafe { (self.egl.swap_interval)(self.display, interval) } == 0 {
            return Err(format!("eglSwapInterval failed: {:#x}", unsafe {
                (self.egl.get_error)()
            }));
        }
        Ok(())
    }

    /// `(vendor, version, client APIs)`.
    pub fn strings(&self) -> (String, String, String) {
        let query = |name| {
            // SAFETY: a valid display; the result is a static NUL-terminated string or null.
            let s = unsafe { (self.egl.query_string)(self.display, name) };
            if s.is_null() {
                String::from("?")
            } else {
                unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned()
            }
        };
        (
            query(EGL_VENDOR),
            query(EGL_VERSION),
            query(EGL_CLIENT_APIS),
        )
    }

    /// Terminates the display, releasing the driver's objects on the Wayland connection. Call
    /// it once every context and surface on it is destroyed and before the connection closes:
    /// otherwise the driver tears them down at process exit, after the connection is gone.
    pub fn terminate(self) -> Result<(), String> {
        // SAFETY: a valid display that nothing uses any more.
        if unsafe { (self.egl.terminate)(self.display) } == 0 {
            return Err(format!("eglTerminate failed: {:#x}", unsafe {
                (self.egl.get_error)()
            }));
        }
        Ok(())
    }
}
