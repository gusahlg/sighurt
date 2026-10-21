//! Session history of a Wasm view.
//!
//! A [`NavigationStack`] is one document's history: a linear list of URLs with associated
//! state, which guests move through with `history_back` / `history_forward` and extend with
//! `push_state`. Pushing a new entry while in the middle of the stack truncates forward
//! history, matching standard browser semantics. The stack counts its changes so the engine can
//! tell when the guest moved through it.
//!
//! A [`Session`] is the view's history across documents: the documents it showed before and
//! after the one on screen, each with the entries it had when it was left. The one on screen
//! keeps its entries in its own stack, where the guest's host functions change them.
//!
//! Everything a view keeps of its history is bounded together: at most [`MAX_ENTRIES`] entries
//! and [`MAX_BYTES`] bytes of URLs and states. The entries farthest from the current one go
//! first; the current entry always stays.

/// Most entries a view's history keeps.
pub const MAX_ENTRIES: usize = 1000;
/// Most bytes of URLs and states a view's history keeps.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

/// A single entry in the navigation history.
#[derive(Clone, Debug)]
pub struct HistoryEntry {
    /// The fully-resolved URL for this history point.
    pub url: String,
    /// Opaque binary state attached by the guest via `push_state` /
    /// `replace_state`.  The guest can read this back on re-entry.
    pub state: Vec<u8>,
}

impl HistoryEntry {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            state: Vec::new(),
        }
    }

    pub fn with_state(mut self, state: Vec<u8>) -> Self {
        self.state = state;
        self
    }

    fn bytes(&self) -> usize {
        self.url.len() + self.state.len()
    }
}

/// A linear stack of [`HistoryEntry`] items with a movable cursor.
#[derive(Clone, Debug)]
pub struct NavigationStack {
    entries: Vec<HistoryEntry>,
    /// Points to the current entry.  `-1` when the stack is empty.
    index: isize,
    /// Counts changes: pushes, replacements and moves.
    version: u64,
    /// Bytes of URLs and states in `entries`.
    bytes: usize,
}

impl NavigationStack {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            index: -1,
            version: 0,
            bytes: 0,
        }
    }

    /// Push a new entry, discarding any forward history beyond the cursor.
    pub fn push(&mut self, entry: HistoryEntry) {
        self.truncate_forward();
        self.bytes += entry.bytes();
        self.entries.push(entry);
        self.index += 1;
        self.trim(MAX_ENTRIES, MAX_BYTES);
        self.version += 1;
    }

    /// Replace the current entry in-place (no forward-history truncation).
    pub fn replace_current(&mut self, entry: HistoryEntry) {
        if self.index >= 0 && (self.index as usize) < self.entries.len() {
            let old = std::mem::replace(&mut self.entries[self.index as usize], entry);
            self.bytes = self.bytes - old.bytes() + self.entries[self.index as usize].bytes();
            self.trim(MAX_ENTRIES, MAX_BYTES);
            self.version += 1;
        } else {
            self.push(entry);
        }
    }

    /// Move the cursor backward.  Returns the new current entry.
    pub fn go_back(&mut self) -> Option<&HistoryEntry> {
        if self.index > 0 {
            self.index -= 1;
            self.version += 1;
            Some(&self.entries[self.index as usize])
        } else {
            None
        }
    }

    /// Move the cursor forward.  Returns the new current entry.
    pub fn go_forward(&mut self) -> Option<&HistoryEntry> {
        if self.index + 1 < self.entries.len() as isize {
            self.index += 1;
            self.version += 1;
            Some(&self.entries[self.index as usize])
        } else {
            None
        }
    }

    /// Makes entry `index` the current one, if there is one.
    pub fn go_to(&mut self, index: usize) {
        if index < self.entries.len() {
            self.index = index as isize;
            self.version += 1;
        }
    }

    pub fn can_go_back(&self) -> bool {
        self.index > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.index + 1 < self.entries.len() as isize
    }

    pub fn current(&self) -> Option<&HistoryEntry> {
        if self.index >= 0 && (self.index as usize) < self.entries.len() {
            Some(&self.entries[self.index as usize])
        } else {
            None
        }
    }

    /// The index of the current entry (0 when the stack is empty).
    pub fn index(&self) -> usize {
        self.index.max(0) as usize
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Bytes of URLs and states the stack holds.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Changes so far: it differs after every push, replacement or move.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Appends each entry's URL to `urls`, oldest first.
    pub fn urls(&self, urls: &mut Vec<String>) {
        urls.extend(self.entries.iter().map(|entry| entry.url.clone()));
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drops the entries after the current one.
    fn truncate_forward(&mut self) {
        let keep = (self.index + 1) as usize;
        for entry in self.entries.drain(keep..) {
            self.bytes -= entry.bytes();
        }
    }

    /// Drops the entries farthest from the current one, oldest first, until at most `entries`
    /// entries and `bytes` bytes are left, or only the current entry.
    fn trim(&mut self, entries: usize, bytes: usize) {
        while (self.entries.len() > entries || self.bytes > bytes) && self.entries.len() > 1 {
            let dropped = if self.index > 0 {
                self.index -= 1;
                self.entries.remove(0)
            } else {
                self.entries.pop().expect("more than one entry")
            };
            self.bytes -= dropped.bytes();
        }
    }

    /// Drops the oldest entry, or the newest when `newest`; for [`Session::trim`].
    fn drop_one(&mut self, newest: bool) {
        let dropped = if newest {
            self.entries.pop()
        } else {
            (!self.entries.is_empty()).then(|| self.entries.remove(0))
        };
        if let Some(dropped) = dropped {
            self.bytes -= dropped.bytes();
            self.index = (self.index - isize::from(!newest)).clamp(-1, self.len() as isize - 1);
        }
    }
}

impl Default for NavigationStack {
    fn default() -> Self {
        Self::new()
    }
}

/// A document a view showed and left: the URL its module was loaded from and its entries.
#[derive(Clone, Debug)]
pub struct PastDocument {
    pub url: String,
    pub history: NavigationStack,
}

/// Where a traversal leads in a [`Session`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// Within the document on screen: the index of the entry in its stack.
    Current(usize),
    /// Entry `entry` of past document `document` before the current one (0 is the oldest).
    Back { document: usize, entry: usize },
    /// Entry `entry` of past document `document` after the current one (0 is the nearest).
    Forward { document: usize, entry: usize },
}

/// A view's history beyond the document on screen.
#[derive(Debug, Default)]
pub struct Session {
    /// Documents before the one on screen, oldest first.
    back: Vec<PastDocument>,
    /// Documents after the one on screen, nearest first.
    forward: Vec<PastDocument>,
}

impl Session {
    /// Where `delta` entries from the current entry of `live` (the document on screen) lead, if
    /// there is an entry there.
    pub fn place(&self, live: &NavigationStack, delta: i32) -> Option<Place> {
        let before: usize = self.back.iter().map(|past| past.history.len()).sum();
        let target = (before + live.index()) as i64 + i64::from(delta);
        let mut target = usize::try_from(target).ok()?;
        for (document, past) in self.back.iter().enumerate() {
            if target < past.history.len() {
                return Some(Place::Back {
                    document,
                    entry: target,
                });
            }
            target -= past.history.len();
        }
        if target < live.len() {
            return Some(Place::Current(target));
        }
        target -= live.len();
        for (document, past) in self.forward.iter().enumerate() {
            if target < past.history.len() {
                return Some(Place::Forward {
                    document,
                    entry: target,
                });
            }
            target -= past.history.len();
        }
        None
    }

    /// The past document at `place`, with its stack at the entry `place` names; `None` for
    /// [`Place::Current`] or a place that isn't there.
    pub fn document(&self, place: Place) -> Option<PastDocument> {
        let (past, entry) = match place {
            Place::Current(_) => return None,
            Place::Back { document, entry } => (self.back.get(document)?, entry),
            Place::Forward { document, entry } => (self.forward.get(document)?, entry),
        };
        let mut past = past.clone();
        past.history.go_to(entry);
        Some(past)
    }

    /// The document on screen, loaded from `url` with history `live`, was left for a new one:
    /// it goes back, without its forward entries, and the documents after it are gone.
    pub fn leave(&mut self, url: String, mut live: NavigationStack) {
        live.truncate_forward();
        self.back.push(PastDocument { url, history: live });
        self.forward.clear();
    }

    /// The document at `place` replaced the one on screen (`url`, `live`) by a traversal: the
    /// documents between them move to the other side, and the one at `place` leaves the
    /// session, since it is on screen now.
    pub fn enter(&mut self, place: Place, url: String, live: NavigationStack) {
        let left = PastDocument { url, history: live };
        match place {
            Place::Current(_) => {}
            Place::Back { document, .. } if document < self.back.len() => {
                let mut after: Vec<_> = self.back.drain(document..).skip(1).collect();
                after.push(left);
                after.append(&mut self.forward);
                self.forward = after;
            }
            Place::Forward { document, .. } if document < self.forward.len() => {
                self.back.push(left);
                self.back.extend(self.forward.drain(..document));
                self.forward.remove(0);
            }
            _ => {}
        }
    }

    /// Drops the entries farthest back, then farthest forward, until the history with `live`
    /// fits [`MAX_ENTRIES`] and [`MAX_BYTES`].
    pub fn trim(&mut self, live: &NavigationStack) {
        let total = |session: &Self| {
            let all = session.back.iter().chain(&session.forward);
            all.fold((live.len(), live.bytes()), |(n, b), past| {
                (n + past.history.len(), b + past.history.bytes())
            })
        };
        let (mut entries, mut bytes) = total(self);
        while entries > MAX_ENTRIES || bytes > MAX_BYTES {
            let (list, newest) = match (self.back.is_empty(), self.forward.is_empty()) {
                (false, _) => (&mut self.back, false),
                (true, false) => (&mut self.forward, true),
                (true, true) => break,
            };
            let past = if newest {
                list.last_mut()
            } else {
                list.first_mut()
            };
            let past = past.expect("not empty");
            let before = past.history.bytes();
            past.history.drop_one(newest);
            (entries, bytes) = (entries - 1, bytes - (before - past.history.bytes()));
            if past.history.is_empty() {
                match newest {
                    true => list.pop(),
                    false => Some(list.remove(0)),
                };
            }
        }
    }

    /// Every entry's URL, oldest first, with those of `live` (the document on screen), and the
    /// index of the current entry.
    pub fn urls(&self, live: &NavigationStack) -> (Vec<String>, usize) {
        let mut urls = Vec::new();
        for past in &self.back {
            past.history.urls(&mut urls);
        }
        let current = urls.len() + live.index();
        live.urls(&mut urls);
        for past in &self.forward {
            past.history.urls(&mut urls);
        }
        (urls, current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(urls: &[&str]) -> NavigationStack {
        let mut stack = NavigationStack::new();
        for url in urls {
            stack.push(HistoryEntry::new(*url));
        }
        stack
    }

    #[test]
    fn push_and_navigate() {
        let mut stack = NavigationStack::new();
        assert!(!stack.can_go_back());
        assert!(!stack.can_go_forward());

        stack.push(HistoryEntry::new("https://a.com"));
        stack.push(HistoryEntry::new("https://b.com"));
        stack.push(HistoryEntry::new("https://c.com"));

        assert_eq!(stack.current().unwrap().url, "https://c.com");
        assert!(stack.can_go_back());
        assert!(!stack.can_go_forward());

        let entry = stack.go_back().unwrap();
        assert_eq!(entry.url, "https://b.com");
        let entry = stack.go_back().unwrap();
        assert_eq!(entry.url, "https://a.com");
        assert!(!stack.can_go_back());
        assert!(stack.can_go_forward());

        let entry = stack.go_forward().unwrap();
        assert_eq!(entry.url, "https://b.com");

        stack.push(HistoryEntry::new("https://d.com"));
        assert!(!stack.can_go_forward());
        assert_eq!(stack.len(), 3); // a, b, d
        assert_eq!(stack.bytes(), 3 * "https://a.com".len());
    }

    #[test]
    fn old_entries_go_first_and_every_change_counts() {
        let mut stack = NavigationStack::new();
        for i in 0..=MAX_ENTRIES {
            stack.push(HistoryEntry::new(format!("https://a.com/{i}")));
        }
        let mut urls = Vec::new();
        stack.urls(&mut urls);
        assert_eq!(urls.len(), MAX_ENTRIES);
        assert_eq!(urls[0], "https://a.com/1");
        assert_eq!(stack.index(), MAX_ENTRIES - 1);
        let version = stack.version();
        stack.go_back();
        stack.go_forward();
        stack.replace_current(HistoryEntry::new("https://b.com"));
        assert_eq!(stack.version(), version + 3);
        // Moving past either end changes nothing.
        stack.go_forward();
        assert_eq!(stack.version(), version + 3);
    }

    #[test]
    fn a_stack_keeps_a_bounded_number_of_bytes() {
        let mut stack = stack(&["https://a.com/"]);
        let big = vec![0; MAX_BYTES / 4];
        for _ in 0..10 {
            stack.push(HistoryEntry::new("https://a.com/#").with_state(big.clone()));
        }
        assert!(stack.bytes() <= MAX_BYTES, "{}", stack.bytes());
        assert_eq!(stack.len(), 3);
        assert_eq!(stack.current().unwrap().state.len(), big.len());
        // A replacement that doesn't fit drops older entries, never the current one.
        stack.replace_current(HistoryEntry::new("https://a.com/").with_state(vec![0; MAX_BYTES]));
        assert_eq!(stack.len(), 1);
        assert_eq!(stack.bytes(), MAX_BYTES + "https://a.com/".len());
    }

    #[test]
    fn replace_current() {
        let mut stack = NavigationStack::new();
        stack.push(HistoryEntry::new("https://a.com"));
        stack.replace_current(HistoryEntry::new("https://b.com"));
        assert_eq!(stack.current().unwrap().url, "https://b.com");
        assert_eq!(stack.len(), 1);
    }

    #[test]
    fn state_preserved_through_back_forward() {
        let mut stack = NavigationStack::new();
        stack.push(HistoryEntry::new("https://a.com").with_state(vec![10, 20]));
        stack.push(HistoryEntry::new("https://b.com"));

        let entry = stack.go_back().unwrap();
        assert_eq!(entry.state, vec![10, 20]);
    }

    #[test]
    fn a_session_spans_documents() {
        let mut session = Session::default();
        // a, a#1 | b | c, c#1, c#2 (on screen, at c#1).
        let mut a = stack(&["a", "a#1", "a#2"]);
        a.go_back();
        session.leave("a".into(), a);
        session.leave("b".into(), stack(&["b"]));
        let mut c = stack(&["c", "c#1", "c#2"]);
        c.go_back();
        assert_eq!(
            session.urls(&c),
            (
                ["a", "a#1", "b", "c", "c#1", "c#2"]
                    .map(String::from)
                    .into(),
                4
            )
        );
        assert_eq!(session.place(&c, 1), Some(Place::Current(2)));
        assert_eq!(session.place(&c, -1), Some(Place::Current(0)));
        assert_eq!(
            session.place(&c, -2),
            Some(Place::Back {
                document: 1,
                entry: 0
            })
        );
        assert_eq!(session.place(&c, 2), None);
        let place = session.place(&c, -4).unwrap();
        assert_eq!(
            place,
            Place::Back {
                document: 0,
                entry: 0
            }
        );

        // Back to "a": b and c go forward, c with all its entries.
        let a = session.document(place).unwrap();
        assert_eq!((a.url.as_str(), a.history.index()), ("a", 0));
        session.enter(place, "c".into(), c);
        assert_eq!(
            session.urls(&a.history),
            (
                ["a", "a#1", "b", "c", "c#1", "c#2"]
                    .map(String::from)
                    .into(),
                0
            )
        );
        let place = session.place(&a.history, 5).unwrap();
        assert_eq!(
            place,
            Place::Forward {
                document: 1,
                entry: 2
            }
        );
        let c = session.document(place).unwrap();
        session.enter(place, a.url, a.history);
        assert_eq!(session.urls(&c.history).1, 5);
        assert_eq!(session.urls(&c.history).0.len(), 6);

        // A new document drops everything after the current entry.
        let mut c = c.history;
        c.go_back();
        c.go_back();
        session.leave("c".into(), c);
        assert_eq!(
            session.urls(&stack(&["d"])),
            (["a", "a#1", "b", "c", "d"].map(String::from).into(), 4)
        );
    }

    #[test]
    fn a_session_keeps_a_bounded_history() {
        let mut session = Session::default();
        for i in 0..10 {
            let mut past = stack(&[&format!("{i}")]);
            past.replace_current(
                HistoryEntry::new(format!("{i}")).with_state(vec![0; MAX_BYTES / 4]),
            );
            session.leave(format!("{i}"), past);
        }
        let live = stack(&["live"]);
        session.trim(&live);
        let (urls, current) = session.urls(&live);
        // The oldest documents went first; what is left fits.
        assert_eq!(urls, ["7", "8", "9", "live"].map(String::from));
        assert_eq!(current, 3);
    }
}
