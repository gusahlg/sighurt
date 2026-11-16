//! The one `WPEScreen` Sighurt's display offers, and the vblank source WebKit's page animations
//! follow: a `WPEScreenSyncObserver` that ticks at the refresh period of the output the window
//! is on, but only while WebKit has a callback registered with it.
//!
//! WebKit paces `requestAnimationFrame` and its rendering with a per-screen vblank monitor
//! (`DisplayVBlankMonitorWPE`). Without a `WPEScreen` WebKit falls back to a 60 Hz sleeping
//! thread of its own, whatever the output is. The observer here ticks at the output's rate (the
//! host reports it through the target lease, and the runtime sets it with
//! [`Screen::set_refresh_mhz`] whenever it changes), running the observer's `sync` (every
//! registered callback) once per period between WebKit's `start` (the first callback added) and
//! `stop` (the last removed): no callback, no tick. The engine contract forbids unconditional
//! ticks, and this keeps to it. [`VblankStats`] counts starts, stops and ticks.
//!
//! **WebKit is told 60 Hz whatever the output's rate** (the screen's `refresh-rate`,
//! [`NOMINAL_MHZ`]), and the ticks carry the real rate. WebKit reads a screen's rate once, when
//! it makes the display link of the screen's id (`DisplayLink::m_displayNominalFramesPerSecond`;
//! `displayPropertiesChanged` is a FIXME in 2.52.6), and a page updates at the rate nearest 60
//! that divides it (`PreferPageRenderingUpdatesNear60FPSEnabled`, on by default and not in 2.52.6's
//! feature list, so it can't be turned off): told 120 Hz, a page takes every other tick, and a
//! display link made at 120 Hz on a window that then moves to a 60 Hz output gives its pages
//! 30. Told 60, every tick is one rendering update, so pages follow the output's rate (60, 120,
//! 144) and a rate change needs nothing of WebKit's; the cost is that WebKit's half-speed
//! throttling (low power, a hidden frame) is half the output's rate rather than 30. Before
//! this was deliberate, it held by accident (the screen's rate reached WebKit only after its
//! display link was made) and broke when it didn't (Astra's review B, 16 November 2026).
//!
//! **The ticks run on the owner thread, from GLib's main context, not on a thread of ours.**
//! WebKit's `sync` copies each callback's function and raw user data under the observer's lock,
//! releases it and then calls them; the WPE monitor's user data is its raw `this`, and the
//! monitor's `invalidate` removes its callback without waiting for a `sync` that already copied
//! it. A display link, and its monitor, is destroyed with its `WebProcessPool` on WebKit's main
//! thread, whenever the pool's last reference goes (which the last view's close starts, and
//! which may finish in a later GLib dispatch). A `sync` on another thread can therefore call a
//! freed monitor, and nothing an observer does in `stop` can wait it out: WebKit calls `stop`
//! with the monitor's lock held, which the running callback takes too, and from inside `sync`
//! itself (a display link stops when it fired with no observers). WebKit's own DRM observer has
//! the same race. Here every tick is a one-shot GLib timeout on the main context the screen was
//! made on (the owner's, which WebKit's UI process runs on), dispatched only by the owner's
//! [`crate::glib_loop::GlibLoop`], so a `sync` and the destruction of any monitor are never at
//! the same time: callbacks are copied and called within one dispatch, and no callback a
//! monitor registers destroys another monitor. WebKit's display link code says it is called off
//! the main thread (an `ASSERT`, compiled out of the release build), but what it does there
//! (send `DisplayDidRefresh` to the web process, under the display link's own lock) works on any
//! thread. The cost is that a tick waits for the owner loop, which pumps WebKit's context at its
//! deadlines, to within a millisecond (GLib's timeouts).
//!
//! Shutdown is explicit: [`Screen::shutdown`] removes the timeout, and nothing ticks after it.
//!
//! The ticks are WebKit's pacing, not the compositor's: frames still reach the compositor only
//! on the host's permits. This is the nominal rate of the output, not its vblank phase.
//!
//! Must not own: the view's state, presentation or any Wayland object.

use std::cell::OnceCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use glib::prelude::*;
use glib::subclass::prelude::*;
use glib::translate::*;

use crate::ffi;

/// The refresh rate, in mHz, until a window says otherwise.
pub const DEFAULT_REFRESH_MHZ: u32 = 60_000;

/// The rate WebKit is told the screen has, in mHz, whatever the output's (module docs).
pub const NOMINAL_MHZ: u32 = 60_000;

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

/// What the observer has done, for diagnostics and tests.
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

/// When the observer ticks.
#[derive(Default)]
struct Control {
    running: bool,
    /// Ended ([`Screen::shutdown`]): never ticks or starts again.
    ended: bool,
    /// When the next tick is due, while running.
    next: Option<Instant>,
    /// The one-shot timeout of that tick; `None` while a tick runs.
    source: Option<glib::Source>,
}

/// The observer, for its timeout's closure, which `glib` requires to be `Send`.
struct ObserverRef(SyncObserver);

// SAFETY: the closure runs only where its source is dispatched, the owner thread that iterates
// the observer's main context, and is dropped there or wherever the source is destroyed; a
// GObject's reference count is atomic, and the observer's state is behind a mutex.
unsafe impl Send for ObserverRef {}

impl ObserverRef {
    /// Through the whole wrapper, so the closure captures it and not just its field.
    fn tick(&self) {
        self.0.imp().tick();
    }
}

fn period(mhz: u32) -> Duration {
    Duration::from_nanos(1_000_000_000_000 / u64::from(mhz.max(1)))
}

/// `wait` rounded up to GLib's milliseconds, so a timeout never fires before it is due.
fn whole_millis(wait: Duration) -> Duration {
    Duration::from_millis(wait.as_micros().div_ceil(1_000) as u64)
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
        /// The tick period, read again for every tick.
        pub period_ns: AtomicU64,
        /// The main context the ticks are dispatched from: the thread-default context of the
        /// thread that made the observer (the owner's, GLib's default one, in the runtime).
        context: glib::MainContext,
        control: Mutex<Control>,
        pub starts: AtomicU64,
        pub stops: AtomicU64,
        pub ticks: AtomicU64,
    }

    impl Default for SyncObserver {
        fn default() -> Self {
            Self {
                period_ns: AtomicU64::new(period(DEFAULT_REFRESH_MHZ).as_nanos() as u64),
                context: glib::MainContext::ref_thread_default(),
                control: Mutex::default(),
                starts: AtomicU64::new(0),
                stops: AtomicU64::new(0),
                ticks: AtomicU64::new(0),
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
        fn control(&self) -> MutexGuard<'_, Control> {
            self.control.lock().unwrap_or_else(|e| e.into_inner())
        }

        fn period(&self) -> Duration {
            Duration::from_nanos(self.period_ns.load(Ordering::Relaxed))
        }

        pub fn running(&self) -> bool {
            self.control().running
        }

        /// A one-shot timeout on the observer's context that ticks at `at`.
        fn arm(&self, at: Instant) -> glib::Source {
            let wait = whole_millis(at.saturating_duration_since(Instant::now()));
            let observer = ObserverRef(self.obj().clone());
            let source = glib::timeout_source_new(
                wait,
                Some("sighurt-vblank"),
                glib::Priority::DEFAULT,
                move || {
                    observer.tick();
                    glib::ControlFlow::Break
                },
            );
            source.attach(Some(&self.context));
            source
        }

        /// One tick: every registered callback, then the next tick's timeout while still
        /// running. Runs in the dispatch of the timeout `arm` made.
        pub(super) fn tick(&self) {
            {
                let mut control = self.control();
                // This timeout ends with this dispatch.
                control.source = None;
                if !control.running || control.ended {
                    return;
                }
            }
            // SAFETY: a live observer of this class. Its `sync` is the base's (this type does
            // not override it) and runs every registered callback; no lock of ours is held, as
            // a callback may stop or start the observer.
            unsafe {
                let observer = self.obj();
                let observer = observer.as_ptr();
                let class = (*observer.cast::<glib::gobject_ffi::GTypeInstance>()).g_class
                    as *const ffi::WPEScreenSyncObserverClass;
                if let Some(sync) = (*class).sync {
                    sync(observer);
                }
            }
            self.ticks.fetch_add(1, Ordering::Relaxed);
            let mut control = self.control();
            // A callback that stopped and started the observer again armed it already.
            if control.running && !control.ended && control.source.is_none() {
                let now = Instant::now();
                let period = self.period();
                let mut next = control.next.unwrap_or(now) + period;
                // Never a burst to catch up after a slow tick.
                if next < now {
                    next = now + period;
                }
                control.next = Some(next);
                control.source = Some(self.arm(next));
            }
        }

        /// Never ticks again: the timeout is removed.
        pub fn end(&self) {
            let source = {
                let mut control = self.control();
                control.ended = true;
                control.running = false;
                control.source.take()
            };
            if let Some(source) = source {
                source.destroy();
            }
        }
    }

    impl SyncObserverImpl for SyncObserver {
        fn start(&self) {
            self.starts.fetch_add(1, Ordering::Relaxed);
            log::debug!("wpe: vblank observer started");
            let mut control = self.control();
            if control.ended {
                return;
            }
            control.running = true;
            if control.source.is_none() {
                let next = Instant::now() + self.period();
                control.next = Some(next);
                control.source = Some(self.arm(next));
            }
        }

        fn stop(&self) {
            self.stops.fetch_add(1, Ordering::Relaxed);
            log::debug!("wpe: vblank observer stopped");
            let source = {
                let mut control = self.control();
                control.running = false;
                control.source.take()
            };
            if let Some(source) = source {
                source.destroy();
            }
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
    /// Sighurt's `WPEScreen`: the output the windows are on, as far as pages may know it, at
    /// 60 Hz as far as WebKit knows.
    pub struct Screen(ObjectSubclass<imp::Screen>) @extends WpeScreen;
}

impl SyncObserver {
    fn as_ptr(&self) -> *mut ffi::WPEScreenSyncObserver {
        self.upcast_ref::<WpeScreenSyncObserver>().to_glib_none().0
    }
}

impl Screen {
    /// A screen whose rect is `size` (logical pixels) at 96 dpi, whose observer ticks at `mhz`
    /// from the calling thread's default main context.
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
            .property("refresh-rate", NOMINAL_MHZ as i32)
            .build();
        let _ = screen.imp().observer.set(glib::Object::new());
        screen.set_refresh_mhz(mhz);
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

    /// The observer's tick rate from its next tick, in mHz (clamped to 1 to 1000 Hz): the
    /// output's rate. WebKit's own idea of the rate stays [`NOMINAL_MHZ`].
    pub fn set_refresh_mhz(&self, mhz: u32) {
        let mhz = mhz.clamp(1_000, 1_000_000);
        self.observer()
            .imp()
            .period_ns
            .store(period(mhz).as_nanos() as u64, Ordering::Relaxed);
    }

    /// The observer's tick rate, in mHz.
    pub fn refresh_mhz(&self) -> u32 {
        let ns = self
            .observer()
            .imp()
            .period_ns
            .load(Ordering::Relaxed)
            .max(1);
        (1_000_000_000_000 / u128::from(ns)) as u32
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
        let observer = self.observer().imp();
        VblankStats {
            starts: observer.starts.load(Ordering::Relaxed),
            stops: observer.stops.load(Ordering::Relaxed),
            ticks: observer.ticks.load(Ordering::Relaxed),
            running: observer.running(),
        }
    }

    /// Ends the observer: its timeout is removed, and nothing ticks after this.
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
    use std::ffi::c_void;
    use std::sync::Arc;
    use std::thread::ThreadId;

    use super::*;

    #[test]
    fn the_period_follows_the_rate() {
        assert_eq!(period(60_000), Duration::from_nanos(16_666_666));
        assert_eq!(period(120_000), Duration::from_nanos(8_333_333));
        assert_eq!(period(0), Duration::from_secs(1_000));
        assert_eq!(
            whole_millis(Duration::from_micros(16_667)),
            Duration::from_millis(17)
        );
        assert_eq!(whole_millis(Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn physical_size_is_never_zero() {
        assert_eq!(millimetres(0), millimetres(1));
        assert!(millimetres(1280) > 300);
    }

    /// Where a callback ran, for the test below.
    #[derive(Default)]
    struct Calls(Mutex<Vec<ThreadId>>);

    unsafe extern "C" fn record(_: *mut ffi::WPEScreenSyncObserver, data: glib::ffi::gpointer) {
        let calls = unsafe { &*(data as *const Calls) };
        calls.0.lock().unwrap().push(std::thread::current().id());
    }

    /// Runs `context` for `total`, sleeping a millisecond whenever nothing was ready.
    fn run(context: &glib::MainContext, total: Duration) {
        let end = Instant::now() + total;
        while Instant::now() < end {
            if !context.iteration(false) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    /// The callbacks WebKit registers run only on the thread that runs the screen's main
    /// context, inside its dispatch: never while that thread is elsewhere (where WebKit destroys
    /// the monitors those callbacks point to), and never after the callback was removed or the
    /// screen shut down. Starting again after a stop resumes the ticks.
    #[test]
    fn callbacks_run_only_in_the_owners_dispatch() {
        ffi::load().expect("WPE WebKit's library");
        let context = glib::MainContext::new();
        let _owner = context.acquire().unwrap();
        context
            .with_thread_default(|| {
                let screen = Screen::new(250_000, (800, 600));
                assert_eq!(screen.refresh_mhz(), 250_000);
                let nominal = unsafe { ffi::wpe_screen_get_refresh_rate(screen.as_ptr()) };
                assert_eq!(nominal, NOMINAL_MHZ as i32, "WebKit is told 60 Hz");
                let observer = screen.observer().as_ptr();
                let calls = Arc::new(Calls::default());
                let data = Arc::as_ptr(&calls) as *mut c_void;
                let add = || unsafe {
                    ffi::wpe_screen_sync_observer_add_callback(observer, Some(record), data, None)
                };
                let count = || calls.0.lock().unwrap().len();

                let id = add();
                assert!(screen.stats().running);
                // A thread of the observer's own would tick here, while the owner sleeps.
                std::thread::sleep(Duration::from_millis(60));
                assert_eq!(count(), 0, "no tick outside the owner's dispatch");
                run(&context, Duration::from_millis(60));
                let ticked = count();
                assert!(ticked > 3, "it ticks at the screen's rate: {ticked}");
                let me = std::thread::current().id();
                assert!(calls.0.lock().unwrap().iter().all(|t| *t == me));

                // The last callback removed: stopped, and nothing more runs.
                unsafe { ffi::wpe_screen_sync_observer_remove_callback(observer, id) };
                assert!(!screen.stats().running);
                run(&context, Duration::from_millis(40));
                assert_eq!(count(), ticked, "no tick after the callback was removed");

                // Another view's display link starts it again.
                let id = add();
                run(&context, Duration::from_millis(40));
                assert!(count() > ticked, "it resumes");
                let stats = screen.stats();
                assert_eq!((stats.starts, stats.stops), (2, 1));

                // Shut down with a callback still registered: nothing more runs.
                screen.shutdown();
                let after = count();
                run(&context, Duration::from_millis(40));
                assert_eq!(count(), after, "no tick after shutdown");
                unsafe { ffi::wpe_screen_sync_observer_remove_callback(observer, id) };
                drop(screen);
            })
            .unwrap();
    }
}
