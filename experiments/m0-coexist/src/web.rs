//! The Servo side: one runtime without a window, and views on software rendering contexts.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use dpi::PhysicalSize;
use servo::{
    EventLoopWaker, JSValue, JavaScriptEvaluationError, Opts, Preferences, RenderingContext, Servo,
    ServoBuilder, ServoDelegate, ServoError, SoftwareRenderingContext, WebView, WebViewBuilder,
    WebViewDelegate,
};
use url::Url;

/// The one Servo runtime of the process and its views.
pub struct Web {
    pub servo: Servo,
    waker: Waker,
    runtime: Rc<RuntimeState>,
    pub views: Vec<View>,
}

/// One WebView with its own rendering context, as each page will have its own target.
pub struct View {
    pub webview: WebView,
    pub state: Rc<ViewState>,
    pub context: Rc<SoftwareRenderingContext>,
}

impl Web {
    /// Builds Servo with `profile` as its profile directory. `hard_fail` is off: Servo's default
    /// exits the whole process when a page panics. Pages get `ServoTestUtils`, so one can panic
    /// its script thread.
    pub fn new(profile: PathBuf) -> Self {
        let waker = Waker::default();
        let servo = ServoBuilder::default()
            .opts(Opts {
                hard_fail: false,
                config_dir: Some(profile),
                ..Opts::default()
            })
            .preferences(Preferences {
                dom_servo_helpers_enabled: true,
                ..Preferences::default()
            })
            .event_loop_waker(Box::new(waker.clone()))
            .build();
        let runtime = Rc::new(RuntimeState::default());
        servo.set_delegate(runtime.clone());
        Self {
            servo,
            waker,
            runtime,
            views: Vec::new(),
        }
    }

    /// Opens a view on `url` in a new 320x240 software rendering context.
    pub fn open(&mut self, url: Url) -> Result<(), String> {
        let context = SoftwareRenderingContext::new(PhysicalSize::new(320, 240))
            .map_err(|e| format!("no software rendering context: {e:?}"))?;
        let context = Rc::new(context);
        let state = Rc::new(ViewState::default());
        let webview = WebViewBuilder::new(&self.servo, context.clone())
            .url(url)
            .delegate(state.clone())
            .build();
        self.views.push(View {
            webview,
            state,
            context,
        });
        Ok(())
    }

    /// Spins Servo until `done` returns true, or gives up after `timeout`. Paints each view
    /// that has a new frame, and calls `between` once per iteration for other work on this
    /// thread.
    pub fn spin_until(
        &self,
        timeout: Duration,
        mut between: impl FnMut(),
        mut done: impl FnMut(&Self) -> bool,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            self.servo.spin_event_loop();
            for view in &self.views {
                if view.state.frame_ready.take() {
                    view.webview.paint();
                    view.context.present();
                    view.state.frames.set(view.state.frames.get() + 1);
                }
            }
            if done(self) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            between();
            self.waker.wait(Duration::from_millis(10));
        }
    }

    /// Evaluates `script` in `view` and spins until the result arrives.
    pub fn evaluate(
        &self,
        view: usize,
        script: &str,
        timeout: Duration,
    ) -> Result<JSValue, String> {
        let result = Rc::new(RefCell::new(None));
        self.start_evaluation(view, script, result.clone());
        self.spin_until(timeout, || {}, |_| result.borrow().is_some());
        finish_evaluation(&result)
    }

    /// Starts evaluating `script` in `view`; `result` is filled in while Servo spins.
    pub fn start_evaluation(&self, view: usize, script: &str, result: Evaluation) {
        self.views[view]
            .webview
            .evaluate_javascript(script, move |value| *result.borrow_mut() = Some(value));
    }

    /// Whether Servo reported losing its backend, which nothing in the process can recover.
    pub fn failed(&self) -> Option<String> {
        self.runtime.failed.borrow().clone()
    }
}

/// The result of a JavaScript evaluation, once it has arrived.
pub type Evaluation = Rc<RefCell<Option<Result<JSValue, JavaScriptEvaluationError>>>>;

/// The string an evaluation returned, or what went wrong.
pub fn finish_evaluation(result: &Evaluation) -> Result<JSValue, String> {
    match result.borrow_mut().take() {
        Some(Ok(value)) => Ok(value),
        Some(Err(e)) => Err(format!("evaluation failed: {e:?}")),
        None => Err("no result in time".into()),
    }
}

/// What Servo reported about one view. Delegate callbacks only record; the loop acts later.
#[derive(Default)]
pub struct ViewState {
    pub title: RefCell<Option<String>>,
    pub crashed: RefCell<Option<String>>,
    frame_ready: Cell<bool>,
    pub frames: Cell<u32>,
}

impl WebViewDelegate for ViewState {
    fn notify_page_title_changed(&self, _: WebView, title: Option<String>) {
        *self.title.borrow_mut() = title;
    }

    fn notify_new_frame_ready(&self, _: WebView) {
        self.frame_ready.set(true);
    }

    fn notify_crashed(&self, _: WebView, reason: String, _: Option<String>) {
        *self.crashed.borrow_mut() = Some(reason);
    }
}

#[derive(Default)]
struct RuntimeState {
    failed: RefCell<Option<String>>,
}

impl ServoDelegate for RuntimeState {
    fn notify_error(&self, error: ServoError) {
        // The old engine called `process::exit(1)` here. In one process this is a runtime
        // failure to report; the other engines keep running.
        if let ServoError::LostConnectionWithBackend = error {
            *self.failed.borrow_mut() = Some(format!("{error:?}"));
        }
    }
}

/// Wakes the owner loop from Servo's threads.
#[derive(Clone, Default)]
struct Waker(Arc<(Mutex<bool>, Condvar)>);

impl Waker {
    /// Waits until woken or `timeout` passes, and clears the wake.
    fn wait(&self, timeout: Duration) {
        let (woken, condvar) = &*self.0;
        let guard = woken.lock().unwrap();
        let (mut guard, _) = condvar
            .wait_timeout_while(guard, timeout, |woken| !*woken)
            .unwrap();
        *guard = false;
    }
}

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        let (woken, condvar) = &*self.0;
        *woken.lock().unwrap() = true;
        condvar.notify_one();
    }
}

/// The renderer and version of a view's GL context, for the record.
pub fn gl_renderer(context: &dyn RenderingContext) -> String {
    if let Err(e) = context.make_current() {
        return format!("context can't be made current: {e:?}");
    }
    let gl = context.gleam_gl_api();
    format!(
        "{} ({})",
        gl.get_string(gleam::gl::RENDERER),
        gl.get_string(gleam::gl::VERSION)
    )
}
