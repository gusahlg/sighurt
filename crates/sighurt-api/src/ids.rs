//! Opaque identities, allocated in increasing order and never reused.
//!
//! Each kind of identity is its own type, so a [`TabId`] can't be passed where a [`ViewId`] is
//! expected, and none of them is an index: a tab keeps its id however the tab list is reordered.
//!
//! Every kind has one process-wide counter, and `next()` (such as [`ViewId::next`]) is the only
//! way to make an id. Uniqueness is therefore structural: no two calls anywhere in the process
//! return the same id, whoever makes them, so a late event from a closed view can't be mistaken
//! for one from a newer view, and nothing needs to hand counters around. A later id of a kind
//! compares greater than an earlier one.
//!
//! Who allocates is a convention, not a capability: the core allocates windows, tabs and views;
//! the host allocates targets, layout epochs and seats; engine adapters allocate:
//!
//! - [`NavigationId`], for a navigation its page starts itself (a link, a script, the history
//!   API); the core allocates the ones for navigations it requests.
//! - [`DocumentGeneration`], whenever a view commits a new document.
//! - [`RequestId`], for each service request and each navigation request its view makes, and
//!   for each site-data request the core makes.

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

macro_rules! ids {
    ($($(#[$doc:meta])* $name:ident;)*) => {$(
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(NonZeroU64);

        impl $name {
            /// A new id, greater than every id of this kind made before it in this process.
            ///
            /// # Panics
            ///
            /// After 2^64 - 1 ids of this kind, rather than reuse one.
            pub fn next() -> Self {
                static LAST: AtomicU64 = AtomicU64::new(0);
                Self(next_serial(&LAST))
            }

            /// The id's number, for diagnostics that print it. Nothing turns a number back
            /// into an id.
            pub fn serial(self) -> u64 {
                self.0.get()
            }
        }
    )*};
}

ids! {
    /// A browser window, from open to close.
    WindowId;
    /// A tab the user sees. It keeps its identity across navigations and engine switches.
    TabId;
    /// One engine-owned content view, until that view closes. Switching a tab to another
    /// engine gives it a new view.
    ViewId;
    /// One requested navigation or history traversal, until it commits, fails, is cancelled or
    /// is superseded. See the module documentation for who allocates it.
    NavigationId;
    /// The committed document of a view, which is the authority that service and permission
    /// requests act for. It changes when a view commits a new document, not on same-document
    /// navigation, so an answer meant for a replaced document can be recognised and dropped.
    DocumentGeneration;
    /// A host-managed presentation destination (a Wayland surface and what backs it). It may
    /// outlive a single document.
    TargetId;
    /// One authoritative placement, size and scale snapshot. The host allocates a new epoch
    /// whenever presentation geometry changes; frames rendered for an older epoch are stale.
    LayoutEpoch;
    /// An asynchronous request (a permission, clipboard transfer, file picker, ...) until it
    /// is answered or cancelled.
    RequestId;
    /// An input seat: a group of input devices the compositor treats as one user's.
    SeatId;
}

/// The next serial from `last`, which holds the last one handed out. At the end of the range
/// the counter stays put and every call panics, so no serial is ever handed out twice.
fn next_serial(last: &AtomicU64) -> NonZeroU64 {
    let previous = last
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .expect("id space exhausted");
    NonZeroU64::MIN.saturating_add(previous)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::thread;

    use super::*;

    #[test]
    fn allocation_increases() {
        let a = ViewId::next();
        let b = ViewId::next();
        let c = ViewId::next();
        assert!(a < b && b < c);
        assert!(a.serial() < b.serial() && b.serial() < c.serial());
    }

    #[test]
    fn independent_allocators_never_collide() {
        // The core and several engine adapters, each allocating on its own thread.
        let threads: Vec<_> = (0..8)
            .map(|_| thread::spawn(|| (0..1000).map(|_| NavigationId::next()).collect::<Vec<_>>()))
            .collect();
        let mut seen = HashSet::new();
        for thread in threads {
            for id in thread.join().unwrap() {
                assert!(seen.insert(id), "{id:?} allocated twice");
            }
        }
        assert_eq!(seen.len(), 8000);
    }
}
