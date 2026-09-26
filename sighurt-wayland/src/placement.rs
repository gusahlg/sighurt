//! Applying a placement snapshot: where each target sits, as one layout transaction.
//!
//! Takes the chrome's layout (window-logical rectangles for chrome, page and overlays, bottom to
//! top), validates it, and applies it. Every live target of the window is listed exactly once;
//! a rectangle must lie inside the window unless it is empty, and an empty one means only that
//! the layout has no room for the target: it is unmapped and gets empty metrics, and nothing is
//! allocated for it. A hidden target (`visible: false`, a background tab) keeps its non-empty
//! metrics and is unmapped.
//!
//! A target whose size or scale changed gets metrics with a new [`LayoutEpoch`], reported as
//! [`HostEvent::MetricsChanged`] and invalidated, so its presenter renders a frame for the new
//! geometry. A target that only moved keeps its epoch: its frames are not stale.
//!
//! # Transactions
//!
//! A placement that shows a target at a new size, or maps one, starts a layout transaction:
//! those targets switch to synchronized commits, so their new frames wait in the compositor
//! until the root commits. The root commits (positions, stacking, the background, the window
//! geometry, and the children's cached frames, all at once) when every such target has
//! submitted a frame for its new epoch, checked at [`Host::flush`], or when the
//! [`RESIZE_GRACE`] period ends. A target that has no frame by then is unmapped, showing the
//! root's background there, so input never goes to a stale layout; the frame it submits for
//! its current epoch, however late, maps it again at the next [`Host::flush`], in a transaction
//! of its own that commits at once. After the root commits, leaves run desynchronized again, so
//! pages and chrome animate independently. Targets the placement hides are unmapped at that same
//! root commit, so a tab switch shows no gap.
//!
//! A placement that arrives while a transaction is open joins it: its targets are waited for
//! too, and the new epoch's first frame of each is not held back by the callback of the frame it
//! already submitted to the transaction (see [`crate::scheduler`]). The grace period runs from
//! the transaction's start and is not extended, so a window resized continuously still commits
//! its root at least every [`RESIZE_GRACE`]; a target that misses it maps with its frame.
//!
//! On a GL target, mapping creates the subsurface role before the permit is issued, so the
//! presenter's first swap is already held back by the synchronized role.
//!
//! Must not own: computing the layout (the chrome does) or any z-order the chrome didn't ask for.

use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt;
use std::rc::Rc;
use std::time::{Duration, Instant};

use calloop::RegistrationToken;
use sighurt_api::geometry::{LogicalSize, Metrics, SurfaceRect, WindowRect};
use sighurt_api::ids::{LayoutEpoch, TargetId, WindowId};

use crate::connection::{Host, HostEvent, TimerEvent};
use crate::targets::{PresentMode, Target};

/// How long a layout transaction waits for new frames before committing without them. Long
/// enough for a chrome repaint and an ordinary page frame, short enough that a slow page never
/// holds the window's resize back noticeably.
pub const RESIZE_GRACE: Duration = Duration::from_millis(150);

/// Where every target of one window goes, as the chrome laid it out.
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    /// The window.
    pub window: WindowId,
    /// The window size the layout was computed for: the size of the latest
    /// [`HostEvent::WindowConfigured`].
    pub size: LogicalSize,
    /// Every live target of the window, bottom to top.
    pub targets: Vec<TargetPlacement>,
}

/// Where one target goes.
#[derive(Clone, Debug, PartialEq)]
pub struct TargetPlacement {
    /// The target.
    pub target: TargetId,
    /// Its rectangle in the window. Empty when the layout has no room for it.
    pub rect: WindowRect,
    /// Whether it is shown. A hidden target keeps its metrics.
    pub visible: bool,
    /// Where it takes pointer input.
    pub input: InputRegion,
}

/// Where a surface takes pointer input. Transparent pixels alone don't let input through.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum InputRegion {
    /// All of it.
    #[default]
    Whole,
    /// These rectangles, in surface-local coordinates; none means no input at all.
    Rects(Vec<SurfaceRect>),
}

/// Why a placement was refused. Nothing of it was applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlacementError {
    /// No such window.
    UnknownWindow(WindowId),
    /// The window has not been configured yet; wait for [`HostEvent::WindowConfigured`].
    NotConfigured(WindowId),
    /// The layout was computed for another window size than the current one.
    StaleSize {
        /// The size the layout was computed for.
        placed: LogicalSize,
        /// The window's size now.
        current: LogicalSize,
    },
    /// Not a live target of this window.
    UnknownTarget(TargetId),
    /// Listed twice.
    Duplicate(TargetId),
    /// A live target of the window was left out.
    Missing(TargetId),
    /// The rectangle reaches outside the window.
    OutOfBounds(TargetId),
}

impl fmt::Display for PlacementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownWindow(w) => write!(f, "no such window: {w:?}"),
            Self::NotConfigured(w) => write!(f, "{w:?} has not been configured yet"),
            Self::StaleSize { placed, current } => write!(
                f,
                "laid out for {}×{}, but the window is {}×{}",
                placed.width, placed.height, current.width, current.height
            ),
            Self::UnknownTarget(t) => write!(f, "{t:?} is not a live target of the window"),
            Self::Duplicate(t) => write!(f, "{t:?} is placed twice"),
            Self::Missing(t) => write!(f, "{t:?} is not placed"),
            Self::OutOfBounds(t) => write!(f, "{t:?} reaches outside the window"),
        }
    }
}

impl std::error::Error for PlacementError {}

/// Checks `placement` against the window's `current` size and its `live` targets.
pub(crate) fn validate(
    placement: &Placement,
    current: LogicalSize,
    live: &[TargetId],
) -> Result<(), PlacementError> {
    if placement.size != current {
        return Err(PlacementError::StaleSize {
            placed: placement.size,
            current,
        });
    }
    let mut seen = HashSet::new();
    for entry in &placement.targets {
        if !live.contains(&entry.target) {
            return Err(PlacementError::UnknownTarget(entry.target));
        }
        if !seen.insert(entry.target) {
            return Err(PlacementError::Duplicate(entry.target));
        }
        if !entry.rect.size().is_empty() && !inside(entry.rect, current) {
            return Err(PlacementError::OutOfBounds(entry.target));
        }
    }
    match live.iter().find(|t| !seen.contains(t)) {
        Some(&missing) => Err(PlacementError::Missing(missing)),
        None => Ok(()),
    }
}

fn inside(rect: WindowRect, size: LogicalSize) -> bool {
    let right = i64::from(rect.x) + i64::from(rect.width);
    let bottom = i64::from(rect.y) + i64::from(rect.height);
    rect.x >= 0 && rect.y >= 0 && right <= i64::from(size.width) && bottom <= i64::from(size.height)
}

/// A layout transaction of one window: the targets whose new frames the root commit waits for.
pub(crate) struct Transaction {
    pub waiting: Vec<TargetId>,
    pub timer: Option<RegistrationToken>,
}

impl Host {
    /// Applies `placement`: positions, stacking, input regions and metrics of every target of
    /// the window, as one layout transaction (see the module documentation). New metrics are
    /// reported as [`HostEvent::MetricsChanged`]. Answer every
    /// [`HostEvent::WindowConfigured`] with a placement for that size, even one with no
    /// targets: the window shows nothing until the first placement.
    pub fn apply_placement(&mut self, placement: Placement) -> Result<(), PlacementError> {
        // A target whose lease was just dropped is no longer live, flush or not.
        self.release_retired_targets();
        let window_id = placement.window;
        let window = self
            .windows
            .get(&window_id)
            .ok_or(PlacementError::UnknownWindow(window_id))?;
        if !window.configured {
            return Err(PlacementError::NotConfigured(window_id));
        }
        validate(&placement, window.size, &window.targets)?;
        let scale = window.scale.effective(self.globals.viewporter.is_some());
        let epoch = LayoutEpoch::next();
        let mut waiting = Vec::new();
        for entry in &placement.targets {
            let target = Rc::clone(&self.targets[&entry.target]);
            let mut t = target.borrow_mut();
            let resized = !t.placed
                || t.metrics.rect().size() != entry.rect.size()
                || t.metrics.scale() != scale;
            let metrics = Metrics::new(
                if resized { epoch } else { t.metrics.epoch() },
                entry.rect,
                scale,
            );
            if metrics != t.metrics || !t.placed {
                t.metrics = metrics;
                self.events.push_back(HostEvent::MetricsChanged {
                    target: entry.target,
                    metrics,
                });
            }
            if let Some(role) = &t.role {
                role.set_position(entry.rect.x, entry.rect.y);
            }
            if entry.input != t.input {
                let compositor = self.compositor.wl_compositor().clone();
                t.set_input_region(&compositor, &entry.input);
                if t.mode == PresentMode::Shm && t.role.is_some() && !resized {
                    // No new frame is coming to carry it; the host commits SHM leaves.
                    t.surface.commit();
                }
            }
            if t.mode == PresentMode::Gl && resized && !metrics.is_empty() {
                // Pending state for the presenter's next swap, which brings the new size.
                crate::scale::apply(
                    &t.surface,
                    t.viewport.as_ref(),
                    scale,
                    metrics.rect().size(),
                );
            }
            t.placed = true;
            t.want_shown = entry.visible && !metrics.is_empty();
            if t.want_shown && (resized || t.role.is_none()) {
                if resized {
                    t.clock.relayout();
                } else {
                    t.clock.invalidate();
                }
                if let Some(role) = &t.role {
                    role.set_sync();
                    t.synced = true;
                }
                waiting.push(entry.target);
            }
        }
        let window = self.windows.get_mut(&window_id).expect("checked above");
        let order: Vec<TargetId> = placement.targets.iter().map(|e| e.target).collect();
        if order != window.targets {
            window.targets = order;
            window.restack = true;
        }
        window.update_background(&self.env, self.globals.viewporter.is_some());
        self.join_transaction(window_id, waiting);
        // An input method's caret follows its target to its new place.
        for index in 0..self.seats.len() {
            self.sync_text_input(index);
        }
        Ok(())
    }

    /// Adds `waiting` targets to `window`'s transaction, starting one if needed; commits right
    /// away when nothing needs to be waited for.
    fn join_transaction(&mut self, window_id: WindowId, waiting: Vec<TargetId>) {
        let Some(window) = self.windows.get_mut(&window_id) else {
            return;
        };
        match (&mut window.transaction, waiting.is_empty()) {
            (Some(transaction), _) => {
                for id in waiting {
                    if !transaction.waiting.contains(&id) {
                        transaction.waiting.push(id);
                    }
                }
            }
            (None, true) => self.commit_layout(window_id),
            (None, false) => {
                let timer = self.loop_ops.timer(
                    Instant::now() + RESIZE_GRACE,
                    TimerEvent::LayoutGrace(window_id),
                );
                window.transaction = Some(Transaction { waiting, timer });
            }
        }
    }

    /// Maps `target`, which is shown but has no role, ahead of its first permit: a synchronized
    /// subsurface at its place in the stack, in a transaction that commits the root once its
    /// frame is in. Returns false if its window is gone.
    pub(crate) fn begin_map(&mut self, target: &Rc<RefCell<Target>>) -> bool {
        let (id, window_id) = {
            let t = target.borrow();
            (t.id, t.window)
        };
        let Some(window) = self.windows.get_mut(&window_id) else {
            return false;
        };
        let mut t = target.borrow_mut();
        let role = self
            .subcompositor
            .get_subsurface(&t.surface, window.root(), &self.qh, ());
        role.set_sync();
        let rect = t.metrics.rect();
        role.set_position(rect.x, rect.y);
        t.role = Some(role);
        t.synced = true;
        t.clock.unmapped();
        drop(t);
        window.restack = true;
        self.join_transaction(window_id, vec![id]);
        true
    }

    /// Commits `window`'s layout: unmaps what the placement hid and what has no frame for its
    /// geometry, restacks, commits the root, and lets the leaves run desynchronized again.
    pub(crate) fn commit_layout(&mut self, window_id: WindowId) {
        let Some(window) = self.windows.get_mut(&window_id) else {
            return;
        };
        if let Some(transaction) = window.transaction.take() {
            if let Some(timer) = transaction.timer {
                self.loop_ops.remove(timer);
            }
        }
        let targets: Vec<Rc<RefCell<Target>>> = window
            .targets
            .iter()
            .filter_map(|id| self.targets.get(id).cloned())
            .collect();
        for target in &targets {
            let mut t = target.borrow_mut();
            if t.role.is_some() && (!t.want_shown || !t.has_current_frame()) {
                if t.want_shown {
                    t.stats.late += 1;
                }
                t.unmap();
                window.restack = true;
            }
        }
        if window.restack {
            let mut below = window.root().clone();
            for target in &targets {
                let t = target.borrow();
                if let Some(role) = &t.role {
                    role.place_above(&below);
                    below = t.surface.clone();
                }
            }
            window.restack = false;
        }
        window.root().commit();
        for target in &targets {
            let mut t = target.borrow_mut();
            if let (Some(role), true) = (&t.role, t.synced) {
                role.set_desync();
                t.synced = false;
            }
        }
    }

    /// Maps the shown targets that have no role but a frame for their current layout on their
    /// surface: those a transaction unmapped for lacking one, whose frame came late. Their
    /// transactions need no more frames, so they complete at this flush.
    pub(crate) fn map_late_frames(&mut self) {
        let late: Vec<Rc<RefCell<Target>>> = self
            .targets
            .values()
            .filter(|t| {
                let t = t.borrow();
                t.leased && t.want_shown && t.role.is_none() && t.has_current_frame()
            })
            .cloned()
            .collect();
        for target in late {
            self.begin_map(&target);
        }
    }

    /// Commits the layouts whose transactions have every frame they waited for.
    pub(crate) fn complete_transactions(&mut self) {
        let ready: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, w)| {
                w.transaction.as_ref().is_some_and(|tx| {
                    tx.waiting.iter().all(|id| {
                        self.targets.get(id).is_none_or(|t| {
                            let t = t.borrow();
                            !t.want_shown || t.has_current_frame()
                        })
                    })
                })
            })
            .map(|(&id, _)| id)
            .collect();
        for window in ready {
            self.commit_layout(window);
        }
    }

    /// The grace period of `window`'s transaction ended.
    pub(crate) fn layout_grace_over(&mut self, window: WindowId) {
        if let Some(tx) = self
            .windows
            .get_mut(&window)
            .and_then(|w| w.transaction.as_mut())
        {
            tx.timer = None; // It fired; it must not be removed again.
            self.commit_layout(window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: u32, height: u32) -> WindowRect {
        WindowRect {
            x,
            y,
            width,
            height,
        }
    }

    fn entry(target: TargetId, rect: WindowRect) -> TargetPlacement {
        TargetPlacement {
            target,
            rect,
            visible: true,
            input: InputRegion::Whole,
        }
    }

    const SIZE: LogicalSize = LogicalSize {
        width: 800,
        height: 600,
    };

    fn placement(targets: Vec<TargetPlacement>) -> Placement {
        Placement {
            window: WindowId::next(),
            size: SIZE,
            targets,
        }
    }

    #[test]
    fn a_complete_layout_is_accepted() {
        let (chrome, page) = (TargetId::next(), TargetId::next());
        let p = placement(vec![
            entry(chrome, rect(0, 0, 800, 40)),
            entry(page, rect(0, 40, 800, 560)),
        ]);
        assert_eq!(validate(&p, SIZE, &[page, chrome]), Ok(()));
    }

    #[test]
    fn a_zero_size_means_no_room_wherever_it_is() {
        let (chrome, page) = (TargetId::next(), TargetId::next());
        // The window is too short for a page below a 600 px chrome.
        let p = placement(vec![
            entry(chrome, rect(0, 0, 800, 600)),
            entry(page, rect(0, 600, 800, 0)),
        ]);
        assert_eq!(validate(&p, SIZE, &[chrome, page]), Ok(()));
    }

    #[test]
    fn every_live_target_is_placed_once() {
        let (chrome, page, other) = (TargetId::next(), TargetId::next(), TargetId::next());
        let whole = rect(0, 0, 800, 600);
        let missing = placement(vec![entry(chrome, whole)]);
        assert_eq!(
            validate(&missing, SIZE, &[chrome, page]),
            Err(PlacementError::Missing(page))
        );
        let twice = placement(vec![entry(chrome, whole), entry(chrome, whole)]);
        assert_eq!(
            validate(&twice, SIZE, &[chrome]),
            Err(PlacementError::Duplicate(chrome))
        );
        let foreign = placement(vec![entry(chrome, whole), entry(other, whole)]);
        assert_eq!(
            validate(&foreign, SIZE, &[chrome]),
            Err(PlacementError::UnknownTarget(other))
        );
    }

    #[test]
    fn subsurfaces_stay_inside_the_window() {
        let page = TargetId::next();
        for outside in [
            rect(0, 40, 800, 561),
            rect(-1, 0, 10, 10),
            rect(795, 0, 10, 10),
        ] {
            let p = placement(vec![entry(page, outside)]);
            assert_eq!(
                validate(&p, SIZE, &[page]),
                Err(PlacementError::OutOfBounds(page))
            );
        }
    }

    #[test]
    fn a_layout_for_an_old_size_is_stale() {
        let page = TargetId::next();
        let p = placement(vec![entry(page, rect(0, 0, 800, 600))]);
        let current = LogicalSize {
            width: 1024,
            height: 768,
        };
        assert_eq!(
            validate(&p, current, &[page]),
            Err(PlacementError::StaleSize {
                placed: SIZE,
                current
            })
        );
    }
}
