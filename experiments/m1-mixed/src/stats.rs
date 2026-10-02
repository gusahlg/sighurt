//! Counters and timings for the report printed at exit. Everything here is bounded however
//! long the run (§26): counters, the last [`RECENT`] durations of each kind, and the wakeups of
//! the idle window only.

use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

/// How many recent durations [`Samples`] keeps for its median and 95th percentile.
const RECENT: usize = 8192;

/// Durations of one kind of operation: the count, minimum and maximum of all of them, and the
/// most recent [`RECENT`] for the median and 95th percentile.
#[derive(Default)]
pub struct Samples {
    count: u64,
    min: Option<Duration>,
    max: Duration,
    recent: VecDeque<Duration>,
}

impl Samples {
    pub fn push(&mut self, duration: Duration) {
        self.count += 1;
        self.min = Some(self.min.map_or(duration, |min| min.min(duration)));
        self.max = self.max.max(duration);
        if self.recent.len() == RECENT {
            self.recent.pop_front();
        }
        self.recent.push_back(duration);
    }

    pub fn count(&self) -> u64 {
        self.count
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
    /// `n=120 min 0.41 med 0.52 p95 0.90 max 3.10 ms`, with "(of the last 8192)" after the
    /// median and 95th percentile once older samples were dropped.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(min) = self.min else {
            return write!(f, "n=0");
        };
        let mut sorted = Vec::from(self.recent.clone());
        sorted.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
        let recent = if self.count > sorted.len() as u64 {
            format!(" (of the last {})", sorted.len())
        } else {
            String::new()
        };
        write!(
            f,
            "n={} min {:.2} med {:.2} p95 {:.2}{recent} max {:.2} ms",
            self.count,
            ms(min),
            ms(at(0.5)),
            ms(at(0.95)),
            ms(self.max)
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

/// Loop wakeups, and how many of them each cause took part in.
#[derive(Clone, Copy, Default)]
struct WakeCounts {
    all: u64,
    servo: u64,
    timer: u64,
    wayland: u64,
}

impl WakeCounts {
    fn add(&mut self, wake: Wake) {
        self.all += 1;
        self.servo += u64::from(wake.servo);
        self.timer += u64::from(wake.timer);
        self.wayland += u64::from(wake.wayland);
    }
}

impl fmt::Display for WakeCounts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (servo {}, timer {}, wayland {})",
            self.all, self.servo, self.timer, self.wayland
        )
    }
}

/// Everything the experiment counts.
#[derive(Default)]
pub struct Stats {
    /// How far back the report counts wakeups for the idle check.
    idle_window: Duration,
    wakeups: WakeCounts,
    /// The wakeups of the last `idle_window`: when each loop iteration started and what woke it.
    recent_wakeups: VecDeque<(Instant, Wake)>,
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
    /// Counts the loop's wakeups in the last `idle_window` before the run ends.
    pub fn new(idle_window: Duration) -> Self {
        Self {
            idle_window,
            ..Self::default()
        }
    }

    /// Records one loop iteration.
    pub fn woke(&mut self) {
        self.woke_at(Instant::now());
    }

    fn woke_at(&mut self, now: Instant) {
        let wake = std::mem::take(&mut self.wake);
        self.wakeups.add(wake);
        while self
            .recent_wakeups
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > self.idle_window)
        {
            self.recent_wakeups.pop_front();
        }
        self.recent_wakeups.push_back((now, wake));
    }

    pub fn report(&self, start: Instant, end: Instant) -> String {
        let window_start = end.checked_sub(self.idle_window).unwrap_or(start);
        // The last wakeup is the one that ended the run (the exit timer or the close request).
        let before_exit = self.recent_wakeups.len().saturating_sub(1);
        let mut idle = WakeCounts::default();
        for (_, wake) in self
            .recent_wakeups
            .iter()
            .take(before_exit)
            .filter(|(t, _)| *t >= window_start)
        {
            idle.add(*wake);
        }
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
            self.wakeups,
            self.idle_window.as_secs_f64(),
            idle,
            self.servo_spin,
            self.page_paint,
            self.page_present,
            self.page_frames_new,
            self.page_frames_layout,
            self.page_frames_remap,
            self.bootstrap_ticks,
            self.ignored_frame_callbacks,
            self.frame_interval,
            self.chrome_paint.count(),
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

    #[test]
    fn samples_keep_a_bounded_window_but_the_extremes_of_all() {
        let mut samples = Samples::default();
        samples.push(Duration::from_millis(900));
        for _ in 0..RECENT {
            samples.push(Duration::from_millis(2));
        }
        assert_eq!(samples.recent.len(), RECENT);
        assert_eq!(
            samples.to_string(),
            "n=8193 min 2.00 med 2.00 p95 2.00 (of the last 8192) max 900.00 ms"
        );
    }

    #[test]
    fn only_the_idle_window_of_wakeups_is_kept() {
        let mut stats = Stats::new(Duration::from_millis(10));
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        for ms in 0..100 {
            stats.wake.servo = ms % 2 == 0;
            stats.woke_at(at(ms));
        }
        assert_eq!(stats.recent_wakeups.len(), 11, "89 ms to 99 ms");
        let report = stats.report(start, at(99));
        assert!(
            report.contains(
                "loop wakeups: 100 (servo 50, timer 0, wayland 0); in the 0 s before the one \
                 that ended the run: 10 (servo 5, timer 0, wayland 0)"
            ),
            "{report}"
        );
    }
}
