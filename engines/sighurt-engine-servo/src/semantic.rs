//! A view's content as structure (`sighurt_api::semantic`), extracted in the page's main world.
//!
//! Owns: when a view extracts (the state per view) and the scripts that notice DOM changes.
//! Must not own: the extraction script or the parser (`sighurt-semantic`), calling Servo (the
//! runtime does), or what a frontend does with the snapshot.
//!
//! Servo 0.7 has no isolated world and no script-message channel. Every script runs in the main
//! world, where the node-id map lives, on the process-unique slot [`sighurt_semantic`] already
//! uses. A `MutationObserver` installed by the extraction sets a dirty flag on that slot; a new
//! frame, once [`DEBOUNCE`] has passed since the last extraction started and nothing is in
//! flight, checks and clears the flag and extracts when it was set. At most one extraction is
//! in flight per view. Nothing is extracted while semantics is off. The page can forge the
//! snapshot; native code stamps the document and the parser enforces the bounds.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use sighurt_api::ids::DocumentGeneration;

/// How long after an extraction or a check starts before a new frame may check the dirty flag.
/// A page that animates sends a frame on every Servo spin; this bounds its checks to four a
/// second. After a long extraction the wait is longer: [`SPACING`] times what it took.
pub const DEBOUNCE: Duration = Duration::from_millis(250);

/// The extraction's time budget in Servo. Its style and layout queries cost about 0.2 ms a node
/// (Servo 0.7, RTX 3070 desktop, 23 October 2026), so sighurt-semantic's 50 ms stopped
/// Wikipedia's article after 269 of its 2,373 nodes; with 2 s the walk finished and printed what
/// WPE does (2,051 words), and the dump took 0.3 s longer. 500 ms covers such pages.
pub const BUDGET_MS: u64 = 500;

/// The next check waits at least this many times as long as the last extraction took, so
/// extractions take at most about a quarter of the page's main thread however often it changes.
pub const SPACING: u32 = 3;

/// The attribute filter of WPE's observer. A mutation of anything else structural still
/// counts: `childList` and `characterData` are not filtered.
const ATTRIBUTE_FILTER: &str = r#"["hidden", "disabled", "checked", "value", "selected", "open",
        "href", "src", "alt", "type", "aria-hidden", "aria-label", "aria-expanded"]"#;

/// Installs the observer once for this document and clears the dirty flag, then walks.
///
/// The flag lives on the extraction slot, which a new document does not have, so the observer
/// is installed once per document. It is installed before the walk: a change during the walk
/// counts, and the flag starts clear so a change the walk already covers does not. Built once
/// per process: the slot's name and the budget never change.
pub fn extraction_script() -> &'static str {
    static SCRIPT: OnceLock<String> = OnceLock::new();
    SCRIPT.get_or_init(build_extraction_script)
}

fn build_extraction_script() -> String {
    let key = sighurt_semantic::state_key_literal();
    let extract = sighurt_semantic::extract_script(BUDGET_MS);
    let mut script = String::with_capacity(extract.len() + 1400);
    script.push_str("(function(){\n");
    script.push_str("var STATE_KEY = ");
    script.push_str(&key);
    script.push_str(";\n");
    script.push_str("var getOwn = Object.getOwnPropertyDescriptor;\n");
    script.push_str("function slot() {\n");
    script.push_str("  try { var d = getOwn(globalThis, STATE_KEY); return d && d.value; }\n");
    script.push_str("  catch (e) { return null; }\n");
    script.push_str("}\n");
    script.push_str("var installed = false;\n");
    script.push_str("var s = null;\n");
    script.push_str("try { s = slot(); } catch (e) { s = null; }\n");
    script.push_str("if (!s || s.watching !== true) {\n");
    script.push_str("  try {\n");
    script.push_str("    new MutationObserver(function () {\n");
    script.push_str("      try { var cur = slot(); if (cur) cur.dirty = true; }\n");
    script.push_str("      catch (err) {}\n");
    script.push_str("    }).observe(document, {\n");
    script
        .push_str("      subtree: true, childList: true, characterData: true, attributes: true,\n");
    script.push_str("      attributeFilter: ");
    script.push_str(ATTRIBUTE_FILTER);
    script.push_str("\n    });\n");
    script.push_str("    installed = true;\n");
    script.push_str("  } catch (obs) {}\n");
    script.push_str("}\n");
    script.push_str("try { if (s) s.dirty = false; } catch (e2) {}\n");
    // The walk is an expression. Its first token stays on this line: EXTRACT begins with a
    // block comment, and a newline between `(` and that comment is fine, but the value is the
    // call, not a `return` of it.
    script.push_str("var json = (");
    script.push_str(&extract);
    script.push_str(");\n");
    script.push_str("try {\n");
    script.push_str("  if (installed) { var after = slot(); if (after) after.watching = true; }\n");
    script.push_str("} catch (e3) {}\n");
    script.push_str("return json;\n})()");
    script
}

/// Returns whether the document changed since the last extraction, and clears the flag.
///
/// Evaluates to a boolean. The flag is on the extraction slot; a document that has not been
/// extracted yet has none, and this is false. Built once per process.
pub fn dirty_script() -> &'static str {
    static SCRIPT: OnceLock<String> = OnceLock::new();
    SCRIPT.get_or_init(build_dirty_script)
}

fn build_dirty_script() -> String {
    let key = sighurt_semantic::state_key_literal();
    let mut script = String::with_capacity(key.len() + 400);
    script.push_str("(function(){\n");
    script.push_str("var STATE_KEY = ");
    script.push_str(&key);
    script.push_str(";\n");
    script.push_str("var getOwn = Object.getOwnPropertyDescriptor;\n");
    script.push_str("var s = null;\n");
    script.push_str("try { var d = getOwn(globalThis, STATE_KEY); s = d && d.value; }\n");
    script.push_str("catch (e) { s = null; }\n");
    script.push_str("var dirty = false;\n");
    script.push_str("try { dirty = !!(s && s.dirty === true); } catch (e2) { dirty = false; }\n");
    script.push_str("try { if (s) s.dirty = false; } catch (e3) {}\n");
    script.push_str("return dirty;\n})()");
    script
}

/// What a finished script was, carried from Servo's callback to the runtime.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Tag {
    /// An extraction that started for this document. `token` is [`Semantics::extract_token`]
    /// at the start, so a result from an extraction that a page reset superseded changes no
    /// state, and does not end the one running now.
    Extract {
        document: DocumentGeneration,
        token: u64,
    },
    /// An action that was sent for this document.
    Action(DocumentGeneration),
    /// A dirty-flag check. `token` is [`Semantics::check_token`] at the start, so a result
    /// from a check that was superseded does not clear the one running now.
    Check(u64),
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
    /// [`Tag::Extract`] of the extraction that is running, when `inflight`.
    pub extract_token: u64,
    /// A dirty-flag check is running.
    pub checking: bool,
    /// [`Tag::Check`] of the check that is running, when `checking`.
    pub check_token: u64,
    /// When the extraction in flight, or the last one, started. The debounce is measured
    /// from here. `None` until the first extraction: nothing has installed the observer.
    pub started: Option<Instant>,
    /// When the last dirty-flag check started. The debounce also runs from here.
    pub checked: Option<Instant>,
    /// How long the last extraction took, start to result.
    pub took: Duration,
    /// A frame arrived before a check could run. Check the dirty flag at this time, once no
    /// script is in flight and the debounce has elapsed. Never set while `on` is false.
    pub due: Option<Instant>,
}

impl Semantics {
    /// The page's scripts are gone (it crashed, or its WebView was replaced). `on` stays:
    /// the frontend still wants snapshots, and the next document extracts.
    pub fn reset_page(&mut self) {
        self.inflight = false;
        self.checking = false;
        self.again = false;
        self.due = None;
        self.started = None;
        self.checked = None;
        self.check_token = self.check_token.wrapping_add(1);
        self.extract_token = self.extract_token.wrapping_add(1);
    }

    /// An extraction starts at `now`. Returns its token for [`Tag::Extract`]. Call it only
    /// when none is in flight.
    pub fn begin_extract(&mut self, now: Instant) -> u64 {
        self.inflight = true;
        self.again = false;
        self.due = None;
        self.started = Some(now);
        self.extract_token = self.extract_token.wrapping_add(1);
        self.extract_token
    }

    /// An extraction with `token` finished. A result whose token is not current (a page
    /// reset came between) changes nothing and returns `None`. Otherwise the extraction is
    /// no longer in flight, `took` is set, and the result is the `again` that was waiting.
    pub fn end_extract(&mut self, token: u64) -> Option<bool> {
        if self.extract_token != token {
            return None;
        }
        self.inflight = false;
        if let Some(started) = self.started {
            self.took = started.elapsed();
        }
        Some(std::mem::take(&mut self.again))
    }

    /// A new frame was reported. Returns whether the dirty flag should be checked now.
    ///
    /// A frame before the debounce has elapsed, or while a script is running, records
    /// [`Self::due`] instead. No extraction has run yet when `started` is missing, and the
    /// observer is not installed, so the frame is not a check.
    pub fn note_frame(&mut self, now: Instant) -> bool {
        if !self.on {
            return false;
        }
        let Some(ready) = self.ready() else {
            return false;
        };
        if self.inflight || self.checking || now < ready {
            let at = ready.max(now);
            self.due = Some(self.due.map_or(at, |due| due.min(at)));
            return false;
        }
        self.due = None;
        true
    }

    /// Whether a frame's dirty-flag check is due at `now`.
    pub fn check_due(&self, now: Instant) -> bool {
        self.on
            && !self.inflight
            && !self.checking
            && self.ready().is_some_and(|ready| now >= ready)
            && self.due.is_some_and(|at| at <= now)
    }

    /// When a frame may next check: [`DEBOUNCE`], or [`SPACING`] times the last extraction's
    /// time if that is longer, after the later of the last extraction and the last check.
    /// `None` before the first extraction.
    fn ready(&self) -> Option<Instant> {
        let started = self.started?;
        let last = self.checked.map_or(started, |checked| checked.max(started));
        Some(last + DEBOUNCE.max(self.took * SPACING))
    }

    /// When the next dirty-flag check is due, if one is waiting and no script is running: not
    /// before [`Self::ready`]. An extraction that ended after `due` was set can push `ready`
    /// past it, and a deadline already passed would make every pump return at once.
    pub fn deadline(&self) -> Option<Instant> {
        if self.on && !self.inflight && !self.checking {
            let due = self.due?;
            Some(self.ready().map_or(due, |ready| due.max(ready)))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_waits_out_the_debounce_since_the_extraction_started() {
        let mut state = Semantics::default();
        let now = Instant::now();
        assert!(!state.note_frame(now), "off: a frame checks nothing");
        state.on = true;
        assert!(
            !state.note_frame(now),
            "no extraction has installed the observer"
        );
        assert!(state.due.is_none());
        state.started = Some(now);
        assert!(!state.note_frame(now), "inside the debounce");
        assert_eq!(state.due, Some(now + DEBOUNCE));
        assert!(!state.check_due(now + DEBOUNCE - Duration::from_millis(1)));
        assert!(state.check_due(now + DEBOUNCE));
        assert_eq!(state.deadline(), Some(now + DEBOUNCE));
        state.inflight = true;
        assert!(!state.check_due(now + DEBOUNCE), "an extraction is running");
        assert!(state.deadline().is_none());
        state.inflight = false;
        assert!(
            state.note_frame(now + DEBOUNCE),
            "the debounce has elapsed and nothing is running"
        );
        assert!(state.due.is_none(), "an immediate check keeps no deadline");
        state.checked = Some(now + DEBOUNCE);
        assert!(
            !state.note_frame(now + DEBOUNCE + Duration::from_millis(10)),
            "a check starts the debounce again: an animating page is checked four times a second"
        );
        assert_eq!(state.due, Some(now + DEBOUNCE * 2));
        state.checked = None;
        state.due = None;
        // A slow extraction spaces the next check out.
        state.took = Duration::from_millis(400);
        assert!(!state.note_frame(now + DEBOUNCE));
        assert_eq!(state.due, Some(now + Duration::from_millis(1200)));
        // A frame recorded before a slow extraction ended: the deadline waits for the spacing
        // the extraction's time asks for, not for the earlier `due`, which has passed.
        state.due = Some(now + DEBOUNCE);
        assert_eq!(state.deadline(), Some(now + Duration::from_millis(1200)));
        assert!(!state.check_due(now + DEBOUNCE));
        state.took = Duration::ZERO;
        state.due = None;
        state.started = Some(now);
        state.checking = true;
        assert!(!state.note_frame(now + DEBOUNCE));
        assert_eq!(state.due, Some(now + DEBOUNCE));
        assert!(state.deadline().is_none(), "a check is running");
    }

    #[test]
    fn the_extraction_installs_the_observer_then_walks() {
        let script = extraction_script();
        let key = sighurt_semantic::state_key_literal();
        assert!(
            script.contains(&key),
            "the observer uses the extraction slot"
        );
        assert!(
            sighurt_semantic::EXTRACT.as_str().contains(&key),
            "the walk uses the same slot"
        );
        assert!(script.contains("MutationObserver"));
        assert!(script.contains("attributeFilter"));
        assert!(script.contains("\"aria-expanded\""));
        assert!(
            !script.contains("webkit.messageHandlers"),
            "Servo has no script-message channel"
        );
        assert!(script.contains("var MAX_NODES = 20000;"));
        assert!(script.contains(&format!("var TIME_BUDGET_MS = {BUDGET_MS};")));
        assert!(script.contains("return json;"));
        let observe = script.find("MutationObserver").expect("observer");
        let walk = script.find("var MAX_NODES").expect("walk");
        assert!(observe < walk, "the observer is installed before the walk");
        let dirty = dirty_script();
        assert!(dirty.contains(&key));
        assert!(dirty.contains("s.dirty = false"));
        assert!(dirty.contains("return dirty;"));
    }

    #[test]
    fn the_scripts_parse_when_node_is_installed() {
        let Some(node) = node_binary() else {
            eprintln!("node not available; JavaScript syntax was not checked");
            return;
        };
        let dir =
            std::env::temp_dir().join(format!("sighurt-servo-semantic-js-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, source) in [
            ("extract.js", extraction_script()),
            ("dirty.js", dirty_script()),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, source).unwrap();
            let output = std::process::Command::new(&node)
                .arg("--check")
                .arg(&path)
                .output()
                .unwrap_or_else(|error| panic!("node --check {name}: {error}"));
            assert!(
                output.status.success(),
                "{name} is not valid JavaScript:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_extraction_result_changes_no_state() {
        let mut semantics = Semantics {
            on: true,
            ..Semantics::default()
        };
        let old = semantics.begin_extract(Instant::now());
        semantics.again = true;
        // The page crashed: its scripts are gone, and a new document extracts.
        semantics.reset_page();
        assert!(!semantics.inflight);
        let new = semantics.begin_extract(Instant::now());
        assert_ne!(old, new);
        semantics.again = true;
        let took = semantics.took;
        let started = semantics.started;
        // The old extraction's late result must not end the new one.
        assert_eq!(semantics.end_extract(old), None);
        assert!(semantics.inflight);
        assert!(semantics.again);
        assert_eq!(semantics.took, took);
        assert_eq!(semantics.started, started);
        // The current one ends it, and hands back the `again` that waited.
        assert_eq!(semantics.end_extract(new), Some(true));
        assert!(!semantics.inflight);
        assert!(!semantics.again);
        // A result is stale once the next extraction has begun.
        let next = semantics.begin_extract(Instant::now());
        assert_ne!(new, next);
        assert_eq!(semantics.end_extract(new), None);
        assert!(semantics.inflight);
    }

    fn node_binary() -> Option<std::path::PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join("node"))
            .find(|candidate| candidate.is_file())
    }
}
