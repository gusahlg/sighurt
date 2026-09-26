//! Counters and timings for the report printed at exit.

use std::fmt;
use std::time::{Duration, Instant};

/// Durations of one kind of operation.
#[derive(Default)]
pub struct Samples(Vec<Duration>);

impl Samples {
    pub fn push(&mut self, duration: Duration) {
        self.0.push(duration);
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Runs `f` and records how long it took.
    pub fn time<T>(&mut self, f: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let result = f();
        self.push(start.elapsed());
        result
    }
}

impl fmt::Display for Samples {
    /// `n=120 min 0.41 med 0.52 p95 0.90 max 3.10 ms`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return write!(f, "n=0");
        }
        let mut sorted = self.0.clone();
        sorted.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
        write!(
            f,
            "n={} min {:.2} med {:.2} p95 {:.2} max {:.2} ms",
            sorted.len(),
            ms(sorted[0]),
            ms(at(0.5)),
            ms(at(0.95)),
            ms(sorted[sorted.len() - 1])
        )
    }
}

/// What woke the event loop: Servo's waker, a timer, Wayland events (any combination, or
/// none when the loop only ran again for work queued by the last iteration).
#[derive(Clone, Copy, Default)]
pub struct Wake {
    pub servo: bool,
    pub timer: bool,
    pub wayland: bool,
}

/// Everything the experiment counts.
#[derive(Default)]
pub struct Stats {
    /// When each event-loop iteration started, after the loop woke up, and what woke it.
    pub wakeups: Vec<(Instant, Wake)>,
    /// What woke the loop for the iteration in progress.
    pub wake: Wake,

    pub servo_spin: Samples,
    /// `WebView::paint`: WebRender drawing into the EGL back buffer.
    pub page_paint: Samples,
    /// `RenderingContext::present`: eglSwapBuffers, which commits the page surface.
    pub page_present: Samples,
    /// Presents by cause: Servo had a new frame, a layout transaction, a remap.
    pub page_frames_new: u64,
    pub page_frames_layout: u64,
    pub page_frames_remap: u64,
    /// Intervals between the page's frame callbacks, from the compositor's timestamps.
    pub frame_interval: Samples,
    pub last_frame_time: Option<u32>,
    /// Frame callbacks for frames given up on (hidden meanwhile) or superseded by a newer one.
    pub ignored_frame_callbacks: u64,
    /// Servo refresh-driver ticks started without a frame callback (nothing was in flight).
    pub bootstrap_ticks: u64,

    pub chrome_paint: Samples,
    pub chrome_paints_layout: u64,
    pub chrome_paints_focus: u64,
    pub chrome_paints_text: u64,
    /// Chrome repaints postponed because both buffers were still held by the compositor.
    pub chrome_buffer_waits: u64,

    pub layout: Samples,
    pub configures: u64,
    pub scale_changes: u64,
    pub hides: u64,
    pub shows: u64,
    /// Time from a remap to the page's first new frame being committed.
    pub remap: Samples,
}

impl Stats {
    /// Records one loop iteration.
    pub fn woke(&mut self) {
        let wake = std::mem::take(&mut self.wake);
        self.wakeups.push((Instant::now(), wake));
    }

    pub fn report(&self, start: Instant, end: Instant, idle_window: Duration) -> String {
        let window_start = end.checked_sub(idle_window).unwrap_or(start);
        // The last wakeup is the one that ended the run (the exit timer or the close request).
        let before_exit = &self.wakeups[..self.wakeups.len().saturating_sub(1)];
        let count = |wakeups: &mut dyn Iterator<Item = &(Instant, Wake)>| {
            let (mut n, mut servo, mut timer, mut wayland) = (0, 0, 0, 0);
            for (_, wake) in wakeups {
                n += 1;
                servo += u32::from(wake.servo);
                timer += u32::from(wake.timer);
                wayland += u32::from(wake.wayland);
            }
            format!("{n} (servo {servo}, timer {timer}, wayland {wayland})")
        };
        format!(
            "run: {:.1} s\n\
             loop wakeups: {}; in the {:.0} s before the one that ended the run: {}\n\
             servo spin: {}\n\
             page paint: {}\n\
             page present: {}\n\
             page frames: new {}, layout {}, remap {}; bootstrap ticks {}; ignored frame callbacks {}\n\
             frame callback interval: {}\n\
             chrome paints: {} (layout {}, focus {}, text {}); buffer waits {}\n\
             chrome paint: {}\n\
             layout transactions: {} (configures {}, scale changes {})\n\
             hide/show: {} hides, {} shows; remap to first frame: {}",
            (end - start).as_secs_f64(),
            count(&mut self.wakeups.iter()),
            idle_window.as_secs_f64(),
            count(&mut before_exit.iter().filter(|(t, _)| *t >= window_start)),
            self.servo_spin,
            self.page_paint,
            self.page_present,
            self.page_frames_new,
            self.page_frames_layout,
            self.page_frames_remap,
            self.bootstrap_ticks,
            self.ignored_frame_callbacks,
            self.frame_interval,
            self.chrome_paint.len(),
            self.chrome_paints_layout,
            self.chrome_paints_focus,
            self.chrome_paints_text,
            self.chrome_buffer_waits,
            self.chrome_paint,
            self.layout,
            self.configures,
            self.scale_changes,
            self.hides,
            self.shows,
            self.remap,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_uses_nearest_rank() {
        let mut samples = Samples::default();
        for ms in [5, 1, 3, 2, 4] {
            samples.push(Duration::from_millis(ms));
        }
        assert_eq!(
            samples.to_string(),
            "n=5 min 1.00 med 3.00 p95 5.00 max 5.00 ms"
        );
        assert_eq!(Samples::default().to_string(), "n=0");
    }
}
