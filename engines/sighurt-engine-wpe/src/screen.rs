//! The one `WPEScreen` Sighurt's display offers, and the vblank source WebKit's page animations
//! follow: a `WPEScreenSyncObserver` that ticks at the screen's refresh period, but only while
//! WebKit has a callback registered with it.
//!
//! WebKit paces `requestAnimationFrame` and its rendering with a per-screen vblank monitor
//! (`DisplayVBlankMonitorWPE`). Without a `WPEScreen` WebKit falls back to a 60 Hz sleeping
//! thread of its own, whatever the output is. The screen here carries the refresh rate of the
//! output the window is on (the host reports it through the target lease, and the runtime sets
//! it with [`Screen::set_refresh_mhz`]); its observer is the same kind of timer as WebKit's
//! fallback, at that rate: a thread that wakes once per period and runs the observer's `sync`
//! (every registered callback). It exists for the runtime's life but runs only between WebKit's
//! `start` (the first callback added) and `stop` (the last removed): no callback, no tick. The
//! engine contract forbids unconditional ticks, and this keeps to it. [`VblankStats`] counts
//! starts, stops and ticks.
//!
//! The ticks are WebKit's pacing, not the compositor's: frames still reach the compositor only
//! on the host's permits. This is the nominal rate of the output, not its vblank phase.
//!
//! Shutdown is explicit: [`Screen::shutdown`] ends and joins the thread, and the runtime calls
//! it before it drops the display (`WpeRuntime::shutdown`). `stop` never waits for the thread:
//! WebKit calls it with its monitor's lock held, and a tick may be waiting for that lock.
//!
//! Must not own: the view's state, presentation or any Wayland object.

use std::cell::OnceCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use glib::prelude::*;
use glib::subclass::prelude::*;
use glib::translate::*;

use crate::ffi;

/// The refresh rate, in mHz, until a window says otherwise.
pub const DEFAULT_REFRESH_MHZ: u32 = 60_000;

glib::wrapper! {
    /// WebKit's `WPEScreen`, the abstract parent of [`Screen`].
    pub struct WpeScreen(Object<ffi::WPEScreen, ffi::WPEScreenClass>);
    match fn { type_ => || ffi::wpe_screen_get_type(), }
}

glib::wrapper! {
    /// WebKit's `WPEScreenSyncObserver`, the abstract parent of [`SyncObserver`].
    pub struct WpeScreenSyncObserver(Object<ffi::WPEScreenSyncObserver, ffi::WPEScreenSyncObserverClass>);
    match fn { type_ => || ffi::wpe_screen_sync_observer_get_type(), }
}

/// What the observer's thread has done, for diagnostics and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VblankStats {
    /// Times WebKit started the observer (its first callback was added).
    pub starts: u64,
    /// Times WebKit stopped it (its last callback was removed).
    pub stops: u64,
    /// Ticks delivered (each ran every registered callback).
    pub ticks: u64,
    /// Whether it is started now.
    pub running: bool,
}

/// What the observer's thread and its owner share.
#[derive(Default)]
pub struct Shared {
    control: Mutex<Control>,
    wake: Condvar,
    /// The tick period, in nanoseconds; read again for every tick.
    period_ns: AtomicU64,
    starts: AtomicU64,
    stops: AtomicU64,
    ticks: AtomicU64,
    running: AtomicBool,
}

#[derive(Default)]
pub struct Control {
    running: bool,
    quit: bool,
}

impl Shared {
    fn control(&self) -> MutexGuard<'_, Control> {
        self.control.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The observer's address, for its thread.
struct ObserverPtr(*mut ffi::WPEScreenSyncObserver);

// SAFETY: the thread only calls the observer class's `sync`, which WebKit documents as called
// from a secondary thread (its callbacks are guarded by the observer's own lock). The thread
// never outlives the object: `Screen::shutdown` and the observer's `dispose` join it first.
unsafe impl Send for ObserverPtr {}

fn period(mhz: u32) -> u64 {
    1_000_000_000_000 / u64::from(mhz.max(1))
}

/// Runs `sync` once per period while started, parked otherwise, until `quit`.
fn ticker(shared: &Shared, observer: &ObserverPtr) {
    let mut control = shared.control();
    loop {
        while !control.running && !control.quit {
            control = shared.wake.wait(control).unwrap_or_else(|e| e.into_inner());
        }
        if control.quit {
            return;
        }
        let mut next =
            Instant::now() + Duration::from_nanos(shared.period_ns.load(Ordering::Relaxed));
        while control.running && !control.quit {
            let now = Instant::now();
            if now < next {
                control = shared
                    .wake
                    .wait_timeout(control, next - now)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
                continue;
            }
            drop(control);
            // SAFETY: see `ObserverPtr`. The class is the observer's own; its `sync` is the
            // base's (this type does not override it) and runs every registered callback.
            unsafe {
                let class = (*observer.0.cast::<glib::gobject_ffi::GTypeInstance>()).g_class
                    as *const ffi::WPEScreenSyncObserverClass;
                if let Some(sync) = (*class).sync {
                    sync(observer.0);
                }
            }
            shared.ticks.fetch_add(1, Ordering::Relaxed);
            control = shared.control();
            let period = Duration::from_nanos(shared.period_ns.load(Ordering::Relaxed));
            next += period;
            // Never a burst to catch up after a slow tick.
            let now = Instant::now();
            if next < now {
                next = now + period;
            }
        }
    }
}

/// The `WPEScreenSyncObserver` virtual methods a subclass provides.
pub trait SyncObserverImpl: ObjectImpl + ObjectSubclass<Type: IsA<WpeScreenSyncObserver>> {
    /// The first callback was added: ticks are wanted.
    fn start(&self);
    /// The last callback was removed: no more ticks are wanted.
    fn stop(&self);
}

unsafe impl<T: SyncObserverImpl> IsSubclassable<T> for WpeScreenSyncObserver {
    fn class_init(class: &mut glib::Class<Self>) {
        Self::parent_class_init::<T>(class);
        let klass = class.as_mut();
        klass.start = Some(observer_start::<T>);
        klass.stop = Some(observer_stop::<T>);
    }
}

unsafe extern "C" fn observer_start<T: SyncObserverImpl>(
    observer: *mut ffi::WPEScreenSyncObserver,
) {
    // SAFETY: WebKit passes the instance of this class.
    let instance = unsafe { &*(observer as *mut T::Instance) };
    instance.imp().start();
}

unsafe extern "C" fn observer_stop<T: SyncObserverImpl>(observer: *mut ffi::WPEScreenSyncObserver) {
    // SAFETY: as above.
    let instance = unsafe { &*(observer as *mut T::Instance) };
    instance.imp().stop();
}

/// The `WPEScreen` virtual methods a subclass provides.
pub trait ScreenImpl: ObjectImpl + ObjectSubclass<Type: IsA<WpeScreen>> {
    /// The screen's sync observer, with a reference for the caller (WebKit adopts it).
    fn sync_observer(&self) -> *mut ffi::WPEScreenSyncObserver;
}

unsafe impl<T: ScreenImpl> IsSubclassable<T> for WpeScreen {
    fn class_init(class: &mut glib::Class<Self>) {
        Self::parent_class_init::<T>(class);
        class.as_mut().get_sync_observer = Some(screen_sync_observer::<T>);
    }
}

unsafe extern "C" fn screen_sync_observer<T: ScreenImpl>(
    screen: *mut ffi::WPEScreen,
) -> *mut ffi::WPEScreenSyncObserver {
    // SAFETY: WebKit passes the instance of this class.
    let instance = unsafe { &*(screen as *mut T::Instance) };
    instance.imp().sync_observer()
}

mod imp {
    use super::*;

    pub struct SyncObserver {
        pub shared: Arc<Shared>,
        pub thread: Mutex<Option<JoinHandle<()>>>,
    }

    impl Default for SyncObserver {
        fn default() -> Self {
            let shared = Shared::default();
            shared
                .period_ns
                .store(period(DEFAULT_REFRESH_MHZ), Ordering::Relaxed);
            Self {
                shared: Arc::new(shared),
                thread: Mutex::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SyncObserver {
        const NAME: &'static str = "SighurtWPEScreenSyncObserver";
        type Type = super::SyncObserver;
        type ParentType = WpeScreenSyncObserver;
    }

    impl ObjectImpl for SyncObserver {
        fn dispose(&self) {
            self.end();
        }
    }

    impl SyncObserver {
        /// Ends the thread and waits for it.
        pub fn end(&self) {
            self.shared.control().quit = true;
            self.shared.wake.notify_all();
            let handle = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(handle) = handle {
                let _ = handle.join();
            }
        }
    }

    impl SyncObserverImpl for SyncObserver {
        fn start(&self) {
            self.shared.starts.fetch_add(1, Ordering::Relaxed);
            self.shared.running.store(true, Ordering::Relaxed);
            log::debug!("wpe: vblank observer started");
            {
                let mut thread = self.thread.lock().unwrap_or_else(|e| e.into_inner());
                if thread.is_none() && !self.shared.control().quit {
                    let shared = Arc::clone(&self.shared);
                    let observer = ObserverPtr(self.obj().as_ptr().cast());
                    *thread = std::thread::Builder::new()
                        .name("sighurt-vblank".into())
                        .spawn(move || ticker(&shared, &observer))
                        .ok();
                }
            }
            self.shared.control().running = true;
            self.shared.wake.notify_all();
        }

        fn stop(&self) {
            self.shared.stops.fetch_add(1, Ordering::Relaxed);
            self.shared.running.store(false, Ordering::Relaxed);
            log::debug!("wpe: vblank observer stopped");
            self.shared.control().running = false;
            self.shared.wake.notify_all();
        }
    }

    #[derive(Default)]
    pub struct Screen {
        pub observer: OnceCell<super::SyncObserver>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Screen {
        const NAME: &'static str = "SighurtWPEScreen";
        type Type = super::Screen;
        type ParentType = WpeScreen;
    }

    impl ObjectImpl for Screen {}

    impl ScreenImpl for Screen {
        fn sync_observer(&self) -> *mut ffi::WPEScreenSyncObserver {
            let observer = self.observer.get().expect("set when the screen is made");
            // SAFETY: a live observer. WebKit adopts what it gets (`adoptGRef`), so each
            // answer carries a reference; the screen keeps its own.
            unsafe {
                let ptr = observer.as_ptr().cast::<glib::gobject_ffi::GObject>();
                glib::gobject_ffi::g_object_ref(ptr);
                ptr.cast()
            }
        }
    }
}

glib::wrapper! {
    /// Sighurt's `WPEScreenSyncObserver`: ticks at the screen's refresh period while started.
    pub struct SyncObserver(ObjectSubclass<imp::SyncObserver>) @extends WpeScreenSyncObserver;
}

glib::wrapper! {
    /// Sighurt's `WPEScreen`: the output the windows are on, as far as pages may know it.
    pub struct Screen(ObjectSubclass<imp::Screen>) @extends WpeScreen;
}

impl SyncObserver {
    fn as_ptr(&self) -> *mut ffi::WPEScreenSyncObserver {
        self.upcast_ref::<WpeScreenSyncObserver>().to_glib_none().0
    }
}

impl Screen {
    /// A screen whose rect is `size` (logical pixels) at 96 dpi, refreshing at `mhz`.
    pub fn new(mhz: u32, size: (i32, i32)) -> Self {
        let screen: Self = glib::Object::builder()
            .property("id", 1u32)
            .property("x", 0)
            .property("y", 0)
            .property("width", size.0)
            .property("height", size.1)
            .property("physical-width", millimetres(size.0))
            .property("physical-height", millimetres(size.1))
            .property("scale", 1.0f64)
            .property("refresh-rate", mhz as i32)
            .build();
        let observer: SyncObserver = glib::Object::new();
        observer
            .imp()
            .shared
            .period_ns
            .store(period(mhz), Ordering::Relaxed);
        let _ = screen.imp().observer.set(observer);
        screen
    }

    pub fn as_ptr(&self) -> *mut ffi::WPEScreen {
        self.upcast_ref::<WpeScreen>().to_glib_none().0
    }

    fn observer(&self) -> &SyncObserver {
        self.imp()
            .observer
            .get()
            .expect("set when the screen is made")
    }

    /// The refresh rate, in mHz: WebKit's nominal rate for pages that start after this, and
    /// the observer's tick period from its next tick.
    pub fn set_refresh_mhz(&self, mhz: u32) {
        let mhz = mhz.clamp(1_000, 1_000_000);
        // SAFETY: a live screen.
        unsafe { ffi::wpe_screen_set_refresh_rate(self.as_ptr(), mhz as i32) };
        self.observer()
            .imp()
            .shared
            .period_ns
            .store(period(mhz), Ordering::Relaxed);
    }

    /// Sets the rect (logical pixels, at 96 dpi). Pages already started keep what they were
    /// told: WebKit sends the screens' properties to its processes when they start.
    pub fn set_size(&self, size: (i32, i32)) {
        // SAFETY: a live screen.
        unsafe {
            ffi::wpe_screen_set_size(self.as_ptr(), size.0, size.1);
            ffi::wpe_screen_set_physical_size(
                self.as_ptr(),
                millimetres(size.0),
                millimetres(size.1),
            );
        }
    }

    /// The observer's counters.
    pub fn stats(&self) -> VblankStats {
        let shared = &self.observer().imp().shared;
        VblankStats {
            starts: shared.starts.load(Ordering::Relaxed),
            stops: shared.stops.load(Ordering::Relaxed),
            ticks: shared.ticks.load(Ordering::Relaxed),
            running: shared.running.load(Ordering::Relaxed),
        }
    }

    /// Ends the observer's thread and waits for it. Nothing ticks after this.
    pub fn shutdown(&self) {
        self.observer().imp().end();
    }
}

/// Millimetres of `pixels` at 96 dpi (WebKit divides by the physical diagonal for the dpi).
fn millimetres(pixels: i32) -> i32 {
    (f64::from(pixels.max(1)) * 25.4 / 96.0).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_period_follows_the_rate() {
        assert_eq!(period(60_000), 16_666_666);
        assert_eq!(period(120_000), 8_333_333);
        assert_eq!(period(0), 1_000_000_000_000);
    }

    #[test]
    fn physical_size_is_never_zero() {
        assert_eq!(millimetres(0), millimetres(1));
        assert!(millimetres(1280) > 300);
    }
}
