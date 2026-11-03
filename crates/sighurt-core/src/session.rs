//! The session: what is saved of the tabs, their order and their history, as plain data.
//!
//! A [`Session`] is a snapshot the core makes (`BrowserState::session`) and the executable
//! writes, atomically, as the text [`Session::to_text`] gives. Reading one back
//! ([`Session::parse`]) checks it as strictly as anything a page sends, since the file may be
//! damaged, but keeps what it can: a damaged tab is left out and a field it doesn't know is
//! ignored, each with a notice ([`Session::notices`]). Only a file that is not a session at all
//! (not TOML, no version, a tab list that is not a list) or one of another version is refused
//! whole, and the executable keeps that file aside instead of writing over it.
//! The browser never holds more than [`MAX_TABS`] tabs, so what it saves is read back whole.
//! Restoring recreates the tabs with new ids and their segments as records, which reload their
//! saved URLs when shown: document state is lost, and restoration is never claimed to be exact.
//!
//! Must not own: reading or writing files.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::navigation::{MAX_HISTORY_ENTRIES, MAX_SEGMENTS};
use crate::tabs::{MAX_TEXT, MAX_URL};

/// The format version of saved sessions.
pub const VERSION: u32 = 1;

/// The most tabs the browser holds, and so the most a session file holds. Reading a file with
/// more keeps the first ones.
pub const MAX_TABS: usize = 1000;

/// The most unknown field names a notice lists.
const MAX_UNKNOWN_NAMED: usize = 8;

/// The fields of a [`SessionTab`] and of a [`SessionSegment`].
const TAB_FIELDS: [&str; 3] = ["current", "zoom", "segments"];
const SEGMENT_FIELDS: [&str; 4] = ["engine", "entries", "current", "title"];

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
    /// What [`Session::parse`] left out or ignored, for the user. Never saved.
    #[serde(skip)]
    pub notices: Vec<String>,
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

    /// Reads a session written by [`Session::to_text`], checking everything in it and keeping
    /// what is usable: a damaged tab, and the tabs after the first [`MAX_TABS`], are left out,
    /// and fields it doesn't know are ignored, each with a notice in [`Session::notices`].
    /// `Err` only for text that is not a session of this [`VERSION`].
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut file: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
        match file.remove("version").as_ref().map(toml::Value::as_integer) {
            Some(Some(version)) if version == i64::from(VERSION) => {}
            Some(Some(version)) if version > i64::from(VERSION) => {
                return Err(format!(
                    "it was written by a newer sig (session version {version})"
                ));
            }
            Some(Some(version)) => return Err(format!("unknown session version {version}")),
            Some(None) | None => return Err("it has no session version".into()),
        }
        let saved = match file.remove("tabs") {
            None => Vec::new(),
            Some(toml::Value::Array(tabs)) => tabs,
            Some(_) => return Err("its tab list is not a list".into()),
        };
        let active = file.remove("active");
        let active = active
            .as_ref()
            .and_then(toml::Value::as_integer)
            .and_then(|active| usize::try_from(active).ok())
            .unwrap_or(0);
        // One more than a notice names, to know there are others.
        let mut unknown: BTreeSet<String> =
            file.keys().take(MAX_UNKNOWN_NAMED + 1).cloned().collect();
        let mut session = Self {
            version: VERSION,
            active: 0,
            tabs: Vec::new(),
            notices: Vec::new(),
        };
        let (count, mut damaged) = (saved.len(), 0);
        for (i, value) in saved.into_iter().take(MAX_TABS).enumerate() {
            match saved_tab(value, &mut unknown) {
                Some(tab) => {
                    // The active tab, or the nearest one before it that is kept.
                    if i <= active {
                        session.active = session.tabs.len();
                    }
                    session.tabs.push(tab);
                }
                None => damaged += 1,
            }
        }
        if damaged > 0 {
            session.notices.push(match damaged {
                1 => "A saved tab was damaged and is left out.".into(),
                n => format!("{n} saved tabs were damaged and are left out."),
            });
        }
        if count > MAX_TABS {
            session.notices.push(format!(
                "Only the first {MAX_TABS} of the {count} saved tabs are restored."
            ));
        }
        if !unknown.is_empty() {
            let named: Vec<String> = unknown
                .iter()
                .take(MAX_UNKNOWN_NAMED)
                .map(|name| format!("{:?}", name.chars().take(40).collect::<String>()))
                .collect();
            let more = match unknown.len() > MAX_UNKNOWN_NAMED {
                true => " and others",
                false => "",
            };
            session.notices.push(format!(
                "The saved tabs have fields this sig doesn't know, ignored: {}{more}.",
                named.join(", ")
            ));
        }
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
        self.tabs.iter().try_for_each(SessionTab::check)
    }
}

impl SessionTab {
    /// Checks that nothing in it is out of bounds or inconsistent.
    fn check(&self) -> Result<(), String> {
        let current_ok = self.current < self.segments.len() || self.segments.is_empty();
        if !current_ok || self.segments.len() > MAX_SEGMENTS || !self.zoom.is_finite() {
            return Err("a tab is damaged".into());
        }
        for segment in &self.segments {
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
        Ok(())
    }
}

/// One saved tab, if it is usable, without the fields it doesn't know (added to `unknown`,
/// one more than a notice names).
fn saved_tab(value: toml::Value, unknown: &mut BTreeSet<String>) -> Option<SessionTab> {
    let toml::Value::Table(mut table) = value else {
        return None;
    };
    let mut strip = |table: &mut toml::Table, known: &[&str]| {
        table.retain(|key, _| {
            let keep = known.contains(&key);
            if !keep && unknown.len() <= MAX_UNKNOWN_NAMED {
                unknown.insert(key.to_owned());
            }
            keep
        });
    };
    strip(&mut table, &TAB_FIELDS);
    if let Some(toml::Value::Array(segments)) = table.get_mut("segments") {
        for segment in segments {
            if let toml::Value::Table(segment) = segment {
                strip(segment, &SEGMENT_FIELDS);
            }
        }
    }
    let tab: SessionTab = toml::Value::Table(table).try_into().ok()?;
    tab.check().is_ok().then_some(tab)
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
            notices: Vec::new(),
        }
    }

    #[test]
    fn a_session_survives_being_saved() {
        let session = session();
        assert_eq!(Session::parse(&session.to_text()), Ok(session));
    }

    #[test]
    fn what_is_not_a_session_of_this_version_is_refused_whole() {
        let mut newer = session();
        newer.version = VERSION + 1;
        let error = Session::parse(&newer.to_text()).unwrap_err();
        assert!(error.contains("newer sig"), "{error}");
        let mut older = session();
        older.version = 0;
        assert!(Session::parse(&older.to_text()).is_err());
        assert!(Session::parse("active = 0").is_err());
        assert!(Session::parse("version = 1\ntabs = 3").is_err());
        assert!(Session::parse("not toml [").is_err());
        assert_eq!(Session::parse("version = 1").unwrap().tabs, []);
    }

    #[test]
    fn a_damaged_tab_is_left_out_and_the_rest_kept() {
        let mut bad = session();
        bad.tabs[0].segments[1].current = 2;
        let read = Session::parse(&bad.to_text()).unwrap();
        assert_eq!(read.tabs, session().tabs[1..]);
        assert_eq!(read.active, 0);
        assert_eq!(read.notices, ["A saved tab was damaged and is left out."]);

        let mut bad = session();
        bad.tabs[0].segments[0].entries.push("sig://other".into());
        bad.tabs[1].zoom = f64::NAN;
        let read = Session::parse(&bad.to_text().replace("nan", "\"x\"")).unwrap();
        assert!(read.tabs.is_empty());
        assert_eq!(
            read.notices,
            ["2 saved tabs were damaged and are left out."]
        );

        // An active index past the end selects the last tab kept.
        let mut far = session();
        far.active = 7;
        assert_eq!(Session::parse(&far.to_text()).unwrap().active, 1);
    }

    #[test]
    fn unknown_fields_are_ignored_with_a_notice() {
        let text = session().to_text().replace(
            "current = 1\nzoom = 1.25",
            "current = 1\nzoom = 1.25\nsurprise = true",
        );
        let text = format!("later = 2\n{text}");
        let read = Session::parse(&text).unwrap();
        assert_eq!(read.tabs, session().tabs);
        assert_eq!(read.active, 1);
        assert_eq!(
            read.notices,
            ["The saved tabs have fields this sig doesn't know, ignored: \"later\", \"surprise\"."]
        );
    }

    #[test]
    fn at_most_max_tabs_are_read() {
        let mut many = session();
        many.tabs = vec![many.tabs[1].clone(); MAX_TABS + 2];
        many.active = MAX_TABS + 1;
        let read = Session::parse(&many.to_text()).unwrap();
        assert_eq!(read.tabs.len(), MAX_TABS);
        assert_eq!(read.active, MAX_TABS - 1);
        assert_eq!(read.notices.len(), 1, "{:?}", read.notices);
    }
}
