//! A view's content as structure (`sighurt_api::semantic`), extracted in an isolated world.
//!
//! Owns: when a view extracts (the state per view) and the observer script that notices DOM
//! changes. Must not own: the extraction script or the parser (`sighurt-semantic`), calling
//! WebKit (the runtime does), or what a frontend does with the snapshot.
//!
//! Every script runs in the isolated world [`WORLD`], so a page cannot see the node table or the
//! observer, and an action finds the nodes the extraction numbered. At most one extraction is in
//! flight per view. A `MutationObserver` in that world posts once per burst to the UI process;
//! the runtime extracts at least [`DEBOUNCE`] later. Nothing is extracted while semantics is off.

use std::ffi::CStr;
use std::time::{Duration, Instant};

use sighurt_api::ids::DocumentGeneration;

/// The isolated world's name. The message handler uses the same name.
pub const WORLD: &CStr = c"sighurt";

/// The script-message channel the observer posts to, in [`WORLD`].
pub const HANDLER: &CStr = c"sighurt";

/// How long a burst of DOM changes waits before another extraction.
pub const DEBOUNCE: Duration = Duration::from_millis(250);

/// Installs the observer once in this world, and clears its "already posted" flag so the next
/// change posts again. Run at the start of an extraction: changes during the walk count, changes
/// already covered by this walk do not post a second time until the walk begins.
pub const OBSERVER_SCRIPT: &str = r#"(function () {
  var g = globalThis;
  var s = g.__sighurtObserver;
  if (!s) {
    s = { posted: false };
    try {
      new MutationObserver(function () {
        if (s.posted) return;
        s.posted = true;
        try {
          var handlers = g.webkit && g.webkit.messageHandlers;
          var channel = handlers && handlers.sighurt;
          if (channel && typeof channel.postMessage === "function") channel.postMessage(0);
          else s.posted = false;
        } catch (err) { s.posted = false; }
      }).observe(document, {
        subtree: true, childList: true, characterData: true, attributes: true,
        attributeFilter: ["hidden", "disabled", "checked", "value", "selected", "open",
          "href", "src", "alt", "type", "aria-hidden", "aria-label", "aria-expanded"]
      });
    } catch (e) {}
    try {
      Object.defineProperty(g, "__sighurtObserver", { value: s, configurable: true });
    } catch (e2) {}
  }
  s.posted = false;
})()"#;

/// The observer install followed by [`sighurt_semantic::EXTRACT`](sighurt_semantic::EXTRACT), as
/// one expression whose value is the snapshot JSON. The observer is installed synchronously
/// before the walk, in the same world.
pub fn extraction_script() -> String {
    let extract = sighurt_semantic::EXTRACT.as_str();
    let mut script = String::with_capacity(OBSERVER_SCRIPT.len() + extract.len() + 48);
    // `return` and the expression's first token stay on one line. EXTRACT begins with a
    // comment, and a newline between `return` and its operand would return undefined.
    script.push_str("(function(){try{");
    script.push_str(OBSERVER_SCRIPT);
    script.push_str("}catch(e){}return (");
    script.push_str(extract);
    script.push_str(");})()");
    script
}

/// What a finished script was, carried through WebKit's callback.
#[derive(Clone, Copy, Debug)]
pub enum Tag {
    /// [`WpeRuntime::evaluate`](crate::WpeRuntime::evaluate): the result is for the caller.
    Test,
    /// An extraction that started for this document.
    Extract(DocumentGeneration),
    /// An action that was sent for this document.
    Action(DocumentGeneration),
}

/// One view's semantic state.
#[derive(Debug, Default)]
pub struct Semantics {
    /// Whether the frontend asked for snapshots.
    pub on: bool,
    /// An extraction script is running.
    pub inflight: bool,
    /// Another extraction was asked for while one was in flight.
    pub again: bool,
    /// The script-message handler is connected for this view.
    pub listening: bool,
    /// Extract at this time because the document changed. Never set while `on` is false.
    pub due: Option<Instant>,
}

impl Semantics {
    /// A DOM change was reported. The next extraction waits out [`DEBOUNCE`] from the latest one.
    pub fn note_mutation(&mut self, now: Instant) {
        if self.on {
            let at = now + DEBOUNCE;
            self.due = Some(self.due.map_or(at, |due| due.max(at)));
        }
    }

    /// Whether a mutation's extraction is due at `now`.
    pub fn mutation_due(&self, now: Instant) -> bool {
        self.on && !self.inflight && self.due.is_some_and(|at| at <= now)
    }

    /// When the next mutation extraction is due, if one is waiting and none is running.
    pub fn deadline(&self) -> Option<Instant> {
        if self.on && !self.inflight {
            self.due
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mutation_waits_out_the_debounce() {
        let mut state = Semantics::default();
        let now = Instant::now();
        state.note_mutation(now);
        assert!(state.due.is_none(), "off: nothing is scheduled");
        state.on = true;
        state.note_mutation(now);
        assert_eq!(state.due, Some(now + DEBOUNCE));
        assert!(!state.mutation_due(now + DEBOUNCE - Duration::from_millis(1)));
        assert!(state.mutation_due(now + DEBOUNCE));
        state.inflight = true;
        assert!(!state.mutation_due(now + DEBOUNCE));
        assert!(state.deadline().is_none());
        state.inflight = false;
        assert_eq!(state.deadline(), Some(now + DEBOUNCE));
        state.note_mutation(now + Duration::from_millis(100));
        assert_eq!(state.due, Some(now + Duration::from_millis(100) + DEBOUNCE));
    }

    #[test]
    fn the_extraction_installs_the_observer_then_walks() {
        let script = extraction_script();
        assert!(script.starts_with("(function(){try{"));
        assert!(
            script.contains("}catch(e){}return ("),
            "return and its operand start on one line"
        );
        assert!(script.contains("MutationObserver"));
        assert!(script.contains("webkit.messageHandlers"));
        assert!(script.contains("handlers.sighurt"));
        assert!(script.contains("var MAX_NODES = 20000;"));
        let walk = script.rfind("return ").expect("the walk is returned");
        let observe = script.find("MutationObserver").expect("observer");
        assert!(observe < walk, "the observer is installed before the walk");
    }
}
