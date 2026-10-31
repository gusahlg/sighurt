//! Loading `libcef.so`, and the shapes the C API needs around its structs: strings and
//! reference-counted objects.
//!
//! The library is not linked: [`load`] opens it (`dlopen`, `RTLD_NOW | RTLD_LOCAL`) when the
//! runtime first starts, so a session that opens no CEF page maps none of it (as the WPE engine
//! does, decision 84). The library finds its own dependencies through the rpath patched into the
//! copy `nix/build-cef.sh` installs. Everything else CEF offers is reached through the vtables of
//! the objects it hands over ([`crate::capi`]).
//!
//! Must not own: any policy. Everything here is plumbing around raw pointers.

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use crate::capi::{
    CefApp, CefBaseRefCounted, CefBrowserSettings, CefClient, CefDictionaryValue, CefMainArgs,
    CefSettings, CefStringUtf16, CefWindowInfo,
};

/// The API version this crate's bindings are generated for (`CEF_API_VERSION`).
pub const API_VERSION: i32 = 15000;
/// `CEF_API_HASH_PLATFORM` of that version, for linux.
pub const API_HASH: &str = "210767725a6feb2e4becd3956b648cab6a006712";

type InitializeFn =
    unsafe extern "C" fn(*const CefMainArgs, *const CefSettings, *mut CefApp, *mut c_void) -> i32;
type CreateBrowserFn = unsafe extern "C" fn(
    *const CefWindowInfo,
    *mut CefClient,
    *const CefStringUtf16,
    *const CefBrowserSettings,
    *mut CefDictionaryValue,
    *mut c_void,
) -> i32;

/// The entry points of `libcef.so` that this crate calls, found with `dlsym`.
pub struct Api {
    /// `cef_initialize`.
    pub initialize: InitializeFn,
    /// `cef_shutdown`.
    pub shutdown: unsafe extern "C" fn(),
    /// `cef_browser_host_create_browser`.
    pub create_browser: CreateBrowserFn,
    /// `cef_string_userfree_utf16_free`.
    pub userfree_free: unsafe extern "C" fn(*mut CefStringUtf16),
    /// `cef_string_list_alloc`.
    pub string_list_alloc: unsafe extern "C" fn() -> *mut c_void,
    /// `cef_string_list_append`.
    pub string_list_append: unsafe extern "C" fn(*mut c_void, *const CefStringUtf16),
}

static API: OnceLock<Api> = OnceLock::new();

/// Opens `dist/libcef.so`, checks that it is the API version the bindings were made for, and
/// remembers its entry points. Idempotent.
pub fn load(dist: &Path) -> Result<&'static Api, String> {
    if let Some(api) = API.get() {
        return Ok(api);
    }
    let path = dist.join("libcef.so");
    let c_path = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| "the CEF path has a NUL".to_string())?;
    // SAFETY: dlopen with a valid C string; the handle is never closed (CEF cannot be unloaded).
    let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        // SAFETY: dlerror returns a valid C string after a failed dlopen.
        let why = unsafe { CStr::from_ptr(libc::dlerror()) }.to_string_lossy();
        return Err(format!("cannot open {}: {why}", path.display()));
    }
    let sym = |name: &str| -> Result<*mut c_void, String> {
        let c_name = CString::new(name).expect("no NUL in a symbol name");
        // SAFETY: handle is a live dlopen handle.
        let address = unsafe { libc::dlsym(handle, c_name.as_ptr()) };
        if address.is_null() {
            Err(format!("{} has no {name}", path.display()))
        } else {
            Ok(address)
        }
    };
    // SAFETY: each address is the function CEF documents under that name, and the function
    // pointer types of `Api` are written from the pinned headers.
    unsafe {
        let hash: unsafe extern "C" fn(i32, i32) -> *const c_char = cast(sym("cef_api_hash")?);
        let found = CStr::from_ptr(hash(API_VERSION, 0)).to_string_lossy();
        if found != API_HASH {
            return Err(format!(
                "libcef.so has API hash {found}, not {API_HASH}: the bindings are for CEF 150"
            ));
        }
        let api = Api {
            initialize: cast(sym("cef_initialize")?),
            shutdown: cast(sym("cef_shutdown")?),
            create_browser: cast(sym("cef_browser_host_create_browser")?),
            userfree_free: cast(sym("cef_string_userfree_utf16_free")?),
            string_list_alloc: cast(sym("cef_string_list_alloc")?),
            string_list_append: cast(sym("cef_string_list_append")?),
        };
        Ok(API.get_or_init(|| api))
    }
}

/// A symbol's address as the function pointer type `F`.
///
/// # Safety
/// `F` is a function pointer type with the signature of the symbol.
unsafe fn cast<F: Copy>(address: *mut c_void) -> F {
    // SAFETY: by the contract above; function and data pointers have the same size on Linux.
    unsafe { std::mem::transmute_copy::<*mut c_void, F>(&address) }
}

/// The loaded API. Only called after [`load`] succeeded (every CEF object comes from it).
pub fn api() -> &'static Api {
    API.get().expect("ffi::api before ffi::load")
}

/// A UTF-16 string for CEF to read. CEF copies a `const cef_string_t*` it is given, so this owns
/// the units itself and hands out a view with no destructor.
pub struct CefStr(Vec<u16>);

impl CefStr {
    /// `s` as UTF-16.
    pub fn new(s: &str) -> Self {
        Self(s.encode_utf16().collect())
    }

    /// The `cef_string_t` view; valid while `self` lives.
    pub fn raw(&self) -> CefStringUtf16 {
        CefStringUtf16 {
            str_: self.0.as_ptr().cast_mut(),
            length: self.0.len(),
            dtor: None,
        }
    }
}

/// An empty `cef_string_t`.
pub fn empty_string() -> CefStringUtf16 {
    CefStringUtf16 {
        str_: std::ptr::null_mut(),
        length: 0,
        dtor: None,
    }
}

/// Reads a `const cef_string_t*` CEF passes to a callback. Null is the empty string.
///
/// # Safety
/// `s` is null or points to a valid `cef_string_t`.
pub unsafe fn read(s: *const CefStringUtf16) -> String {
    if s.is_null() {
        return String::new();
    }
    // SAFETY: by the contract above; the units are `length` long.
    unsafe {
        let s = &*s;
        if s.str_.is_null() {
            return String::new();
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(s.str_, s.length))
    }
}

/// Reads and frees a `cef_string_userfree_t` that CEF returned to us.
///
/// # Safety
/// `s` is null or came from a CEF getter that returns a string we own.
pub unsafe fn take(s: *mut CefStringUtf16) -> String {
    if s.is_null() {
        return String::new();
    }
    // SAFETY: by the contract above.
    unsafe {
        let text = read(s);
        (api().userfree_free)(s);
        text
    }
}

/// Calls a method of a CEF object through its vtable: `call!(object, method, args...)`.
/// The slot is always filled in a live CEF object; a null one is a CEF bug and panics.
macro_rules! call {
    ($o:expr, $m:ident $(, $a:expr)*) => {{
        let o = $o;
        ((*o).$m.expect(concat!("cef slot ", stringify!($m))))(o $(, $a)*)
    }};
}
pub(crate) use call;

/// A struct whose first field is the `cef_base_ref_counted_t`, as every CEF interface is.
pub trait Vtable {
    /// Its base.
    fn base(&mut self) -> &mut CefBaseRefCounted;
}

macro_rules! vtables {
    ($($t:ident),* $(,)?) => {$(
        impl Vtable for crate::capi::$t {
            fn base(&mut self) -> &mut CefBaseRefCounted { &mut self.base }
        }
    )*};
}
vtables!(
    CefApp,
    CefBrowserProcessHandler,
    CefClient,
    CefContextMenuHandler,
    CefDialogHandler,
    CefDisplayHandler,
    CefDownloadHandler,
    CefJsdialogHandler,
    CefKeyboardHandler,
    CefLifeSpanHandler,
    CefLoadHandler,
    CefPermissionHandler,
    CefRenderHandler,
    CefRequestHandler,
    CefNavigationEntryVisitor,
);

/// A heap object implementing a CEF interface: the vtable first (so a pointer to it is a pointer
/// to the interface), then the count and the Rust data the callbacks need.
#[repr(C)]
struct Obj<V, D> {
    vt: V,
    count: AtomicUsize,
    data: D,
}

/// Makes a reference-counted CEF object with count 1, owned by the caller. The vtable's slots
/// the caller did not fill stay null; CEF skips a null slot. `data` is dropped with the last
/// reference.
pub fn new_object<V: Vtable, D>(mut vt: V, data: D) -> *mut V {
    let base = vt.base();
    base.size = std::mem::size_of::<V>();
    base.add_ref = Some(add_ref::<V, D>);
    base.release = Some(release::<V, D>);
    base.has_one_ref = Some(has_one_ref::<V, D>);
    base.has_at_least_one_ref = Some(has_at_least_one_ref::<V, D>);
    Box::into_raw(Box::new(Obj {
        vt,
        count: AtomicUsize::new(1),
        data,
    }))
    .cast()
}

/// An all-zero vtable: every slot null.
pub fn zeroed<V: Vtable>() -> V {
    // SAFETY: the generated vtables are repr(C) structs of `Option<fn>`, raw pointers and
    // integers, all valid when zero.
    unsafe { std::mem::zeroed() }
}

unsafe extern "C" fn add_ref<V, D>(p: *mut CefBaseRefCounted) {
    // SAFETY: CEF calls this with the pointer we gave it: an `Obj<V, D>`.
    unsafe { &*(p as *mut Obj<V, D>) }
        .count
        .fetch_add(1, Ordering::Relaxed);
}

unsafe extern "C" fn release<V, D>(p: *mut CefBaseRefCounted) -> i32 {
    // SAFETY: as above; the last release frees the allocation made by `new_object`.
    unsafe {
        let object = p as *mut Obj<V, D>;
        if (*object).count.fetch_sub(1, Ordering::AcqRel) == 1 {
            drop(Box::from_raw(object));
            1
        } else {
            0
        }
    }
}

unsafe extern "C" fn has_one_ref<V, D>(p: *mut CefBaseRefCounted) -> i32 {
    // SAFETY: as above.
    i32::from(
        unsafe { &*(p as *mut Obj<V, D>) }
            .count
            .load(Ordering::Acquire)
            == 1,
    )
}

unsafe extern "C" fn has_at_least_one_ref<V, D>(p: *mut CefBaseRefCounted) -> i32 {
    // SAFETY: as above.
    i32::from(
        unsafe { &*(p as *mut Obj<V, D>) }
            .count
            .load(Ordering::Acquire)
            >= 1,
    )
}

/// The Rust data of an object made by [`new_object`].
///
/// # Safety
/// `p` came from `new_object::<V, D>` and is still referenced.
pub unsafe fn data<'a, V, D>(p: *mut V) -> &'a D {
    // SAFETY: by the contract above.
    unsafe { &(*(p as *mut Obj<V, D>)).data }
}

/// Adds a reference to a CEF object.
///
/// # Safety
/// `p` points to a live CEF interface.
pub unsafe fn add_ref_raw<T>(p: *mut T) {
    let base = p as *mut CefBaseRefCounted;
    // SAFETY: every interface starts with its base.
    unsafe { ((*base).add_ref.expect("add_ref"))(base) }
}

/// Drops a reference to a CEF object.
///
/// # Safety
/// `p` points to a live CEF interface we hold a reference to.
pub unsafe fn release_raw<T>(p: *mut T) {
    let base = p as *mut CefBaseRefCounted;
    // SAFETY: every interface starts with its base.
    unsafe { ((*base).release.expect("release"))(base) };
}

/// A reference we hold to a CEF object, released on drop.
///
/// CEF documents its browser, host, frame and navigation objects as usable from any thread
/// (they forward to the UI thread themselves), which is what makes the `Send` and `Sync`
/// implementations sound for the types this crate wraps in it.
pub struct Ref<T>(NonNull<T>);

// SAFETY: see the type's documentation; only used for CEF's thread-safe interfaces.
unsafe impl<T> Send for Ref<T> {}
// SAFETY: as above.
unsafe impl<T> Sync for Ref<T> {}

impl<T> Ref<T> {
    /// Takes over a reference CEF returned (a new reference, or `add_ref`ed by the caller).
    /// Null gives `None`.
    pub fn from_owned(p: *mut T) -> Option<Self> {
        NonNull::new(p).map(Self)
    }

    /// A new reference to a pointer CEF lent for the length of a callback.
    ///
    /// # Safety
    /// `p` is null or points to a live CEF interface.
    pub unsafe fn from_borrowed(p: *mut T) -> Option<Self> {
        let p = NonNull::new(p)?;
        // SAFETY: by the contract above.
        unsafe { add_ref_raw(p.as_ptr()) };
        Some(Self(p))
    }

    /// The raw pointer, for calling its methods.
    pub fn as_ptr(&self) -> *mut T {
        self.0.as_ptr()
    }
}

impl<T> Clone for Ref<T> {
    fn clone(&self) -> Self {
        // SAFETY: we hold a reference, so the object is live.
        unsafe { add_ref_raw(self.0.as_ptr()) };
        Self(self.0)
    }
}

impl<T> Drop for Ref<T> {
    fn drop(&mut self) {
        // SAFETY: we hold the reference being released.
        unsafe { release_raw(self.0.as_ptr()) };
    }
}
