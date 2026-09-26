//! When the page may present: one frame at a time, paced by its frame callbacks (§10).

/// Tracks the page frame whose frame callback the host waits for.
///
/// A page frame is presented only when no earlier one waits for its callback, so the page
/// renders at the rate the compositor shows it. Hiding the page gives up on the callback in
/// flight, because the compositor may never send one for an unmapped surface; if it still
/// arrives, it is stale and changes nothing.
#[derive(Debug, Default)]
pub struct FramePacer {
    in_flight: Option<u64>,
    last: u64,
}

impl FramePacer {
    /// Whether a frame may be presented now.
    pub fn ready(&self) -> bool {
        self.in_flight.is_none()
    }

    /// Records a presented frame and returns its id, for its frame callback. A frame presented
    /// while another is in flight (a layout transaction or a remap doesn't wait) replaces it:
    /// only the newest frame's callback counts.
    pub fn presented(&mut self) -> u64 {
        self.last += 1;
        self.in_flight = Some(self.last);
        self.last
    }

    /// Handles frame `id`'s callback: `true` if it is the one being waited for.
    pub fn done(&mut self, id: u64) -> bool {
        let current = self.in_flight == Some(id);
        if current {
            self.in_flight = None;
        }
        current
    }

    /// Stops waiting, when the page is hidden. Returns the frame given up on.
    pub fn abandon(&mut self) -> Option<u64> {
        self.in_flight.take()
    }

    /// The last frame presented.
    pub fn last(&self) -> u64 {
        self.last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_frame_per_callback() {
        let mut pacer = FramePacer::default();
        assert!(pacer.ready());
        let frame = pacer.presented();
        assert!(
            !pacer.ready(),
            "the next frame waits for this one's callback"
        );
        assert!(pacer.done(frame));
        assert!(pacer.ready());
    }

    #[test]
    fn a_remapped_page_does_not_wait_for_the_old_callback() {
        let mut pacer = FramePacer::default();
        let before_hide = pacer.presented();
        assert_eq!(pacer.abandon(), Some(before_hide));
        assert!(pacer.ready(), "the remap presents at once");
        let remap = pacer.presented();
        // The compositor sends the old callback once the surface is visible again.
        assert!(!pacer.done(before_hide), "a stale callback is ignored");
        assert!(!pacer.ready(), "and doesn't release the remap frame's wait");
        assert!(pacer.done(remap));
        assert!(pacer.ready());
    }

    #[test]
    fn only_the_newest_frame_counts() {
        let mut pacer = FramePacer::default();
        let animation = pacer.presented();
        let layout = pacer.presented();
        assert!(!pacer.done(animation));
        assert!(!pacer.ready());
        assert!(pacer.done(layout));
        assert!(!pacer.done(layout), "a callback counts once");
    }
}
