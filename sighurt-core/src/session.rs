//! The session: what is saved of the tabs, their order and their history, as plain data.
//!
//! A [`Session`] is a snapshot the core makes (`BrowserState::session`) and the executable
//! writes, atomically, as the text [`Session::to_text`] gives. Reading one back
//! ([`Session::parse`]) checks it as strictly as anything a page sends: the file may be damaged.
//! Restoring recreates the tabs with new ids and their segments as records, which reload their
//! saved URLs when shown: document state is lost, and restoration is never claimed to be exact.
//!
//! Must not own: reading or writing files.

use serde::{Deserialize, Serialize};

use crate::navigation::{MAX_HISTORY_ENTRIES, MAX_SEGMENTS};
use crate::tabs::{MAX_TEXT, MAX_URL};

/// The format version of saved sessions.
pub const VERSION: u32 = 1;

/// The most tabs a session file may hold.
pub const MAX_TABS: usize = 1000;

/// The saved tabs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    /// [`VERSION`].
    pub version: u32,
    /// The index of the active tab.
    pub active: usize,
    /// The tabs, in order.
    #[serde(default)]
    pub tabs: Vec<SessionTab>,
}

/// One saved tab.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionTab {
    /// The index of the segment it showed.
    pub current: usize,
    /// Its page zoom.
    pub zoom: f64,
    /// Its history, oldest first.
    #[serde(default)]
    pub segments: Vec<SessionSegment>,
}

/// One saved stretch of a tab's history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSegment {
    /// The engine it was open in; none for an internal page.
    pub engine: Option<String>,
    /// Its URLs, oldest first.
    pub entries: Vec<String>,
    /// The index of the current entry.
    pub current: usize,
    /// The page's title.
    pub title: Option<String>,
}

impl Session {
    /// The session as text, for the executable to write.
    pub fn to_text(&self) -> String {
        toml::to_string(self).expect("a session is plain data")
    }

    /// Reads a session written by [`Session::to_text`], checking everything in it.
    pub fn parse(text: &str) -> Result<Self, String> {
        let session: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        session.check()?;
        Ok(session)
    }

    /// Checks that nothing in it is out of bounds or inconsistent.
    pub(crate) fn check(&self) -> Result<(), String> {
        if self.version != VERSION {
            return Err(format!("unknown session version {}", self.version));
        }
        if self.tabs.len() > MAX_TABS || (self.active >= self.tabs.len() && self.active > 0) {
            return Err("the tab list is damaged".into());
        }
        for tab in &self.tabs {
            let current_ok = tab.current < tab.segments.len() || tab.segments.is_empty();
            if !current_ok || tab.segments.len() > MAX_SEGMENTS || !tab.zoom.is_finite() {
                return Err("a tab is damaged".into());
            }
            for segment in &tab.segments {
                // A record without entries had a URL too long to keep.
                let current_ok = segment.current < segment.entries.len()
                    || (segment.entries.is_empty() && segment.current == 0);
                let valid = current_ok
                    && segment.entries.len() <= MAX_HISTORY_ENTRIES
                    && segment.entries.iter().all(|url| url.len() <= MAX_URL)
                    && segment.title.as_ref().is_none_or(|t| t.len() <= MAX_TEXT)
                    && (segment.engine.is_some() || segment.entries.len() == 1);
                if !valid {
                    return Err("a tab's history is damaged".into());
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session {
            version: VERSION,
            active: 1,
            tabs: vec![
                SessionTab {
                    current: 1,
                    zoom: 1.25,
                    segments: vec![
                        SessionSegment {
                            engine: None,
                            entries: vec!["sig://home".into()],
                            current: 0,
                            title: None,
                        },
                        SessionSegment {
                            engine: Some("servo".into()),
                            entries: vec!["https://a.test/".into(), "https://a.test/#b".into()],
                            current: 1,
                            title: Some("A \"quoted\" title".into()),
                        },
                    ],
                },
                SessionTab {
                    current: 0,
                    zoom: 1.0,
                    segments: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn a_session_survives_being_saved() {
        let session = session();
        assert_eq!(Session::parse(&session.to_text()), Ok(session));
    }

    #[test]
    fn damaged_sessions_are_refused() {
        let mut bad = session();
        bad.tabs[0].segments[1].current = 2;
        assert!(Session::parse(&bad.to_text()).is_err());
        let mut bad = session();
        bad.active = 2;
        assert!(Session::parse(&bad.to_text()).is_err());
        let mut bad = session();
        bad.tabs[0].segments[0].entries.push("sig://other".into());
        assert!(Session::parse(&bad.to_text()).is_err());
        let mut bad = session();
        bad.version = 2;
        assert!(Session::parse(&bad.to_text()).is_err());
        assert!(Session::parse("version = 1\nactive = 0\nsurprise = true").is_err());
        assert!(Session::parse("not toml [").is_err());
    }
}
