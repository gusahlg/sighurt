//! The terminal's `sig://home`: what `sighurt_runtime::home` says, drawn in the page rows as
//! cells in every mode (graphics or text, kitty, sixel or none).
//!
//! Home is turned into a small document of headings, paragraphs and links (a
//! [`SemanticSnapshot`] built here) and laid out, drawn and navigated by text mode's own code
//! ([`crate::textmode`]): its text is sanitized and wrapped like a page's, a line too wide for
//! the keys' columns ends in an ellipsis, and its links are focused, followed and scrolled the
//! way a page's are. When the page rows are too short for all of it, whole sections are left
//! out in the window's order (Engines first, then Recently visited, then Keys); what is still
//! too tall scrolls.
//!
//! Owns: that document, its layout for the page rows' size, the scroll and the focused link.
//! Must not own: what home says (the runtime's `home`), what following a link does (the app
//! runs `page.open <url>`), which keys reach it (the keymap: `page.send-key` and passed keys
//! arrive as the core's `ChromeInput`), or whether the page rows may be drawn on (the app's
//! images, decision 133).

use sighurt_api::ids::DocumentGeneration;
use sighurt_api::input::KeyEvent;
use sighurt_api::semantic::{NodeId, NodeKind, SemanticNode, SemanticSnapshot};
use sighurt_runtime::home::Home;
use unicode_width::UnicodeWidthStr;

use crate::grid::Grid;
use crate::text::Nav;
use crate::textmode::{self, Area, TextDocument, TextView};

/// The parts of home below its heading, in the order they are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    Pages,
    Keys,
    Recent,
    Engines,
}

/// Shown when there is room for them, in this order.
const SECTIONS: [Section; 4] = [
    Section::Pages,
    Section::Keys,
    Section::Recent,
    Section::Engines,
];

/// How many sections a short area keeps, at least: the pages.
const KEPT: usize = 1;

/// Columns between the keys' two columns.
const KEY_GUTTER: usize = 4;

/// Home, laid out for the page rows.
pub struct HomePage {
    /// The session's home generation and the size it was laid out for.
    made: Option<(u64, u16, u16)>,
    doc: TextDocument,
    view: TextView,
    /// Each link's node and the address it opens, exactly as home gave it.
    links: Vec<(NodeId, String)>,
    rows: u16,
}

impl Default for HomePage {
    fn default() -> Self {
        Self::new()
    }
}

impl HomePage {
    /// Nothing laid out yet.
    pub fn new() -> Self {
        Self {
            made: None,
            doc: textmode::layout(&empty(), 1),
            view: TextView::new(1),
            links: Vec::new(),
            rows: 0,
        }
    }

    /// Whether home of `generation` laid out for `cols` by `rows` is what this shows.
    pub fn is(&self, generation: u64, cols: u16, rows: u16) -> bool {
        self.made == Some((generation, cols, rows))
    }

    /// Lays `home` (the session's `generation` of it) out for `cols` by `rows`, keeping the
    /// focused link when only the size changed.
    pub fn lay_out(&mut self, home: &Home, generation: u64, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let mut shown = SECTIONS.len();
        let (links, doc) = loop {
            let (snapshot, links) = document(home, &SECTIONS[..shown], usize::from(cols));
            let doc = textmode::layout(&snapshot, cols);
            if doc.lines.len() <= usize::from(rows) || shown == KEPT {
                break (links, doc);
            }
            shown -= 1;
        };
        // The same home at another size keeps its focused link, if it is still shown.
        let focused = self
            .focused_url()
            .filter(|_| self.made.is_some_and(|(made, ..)| made == generation));
        self.view = TextView::new(cols);
        let node = links.iter().find(|(_, url)| Some(url) == focused.as_ref());
        if let Some(item) = node.and_then(|(node, _)| doc.item_index(*node)) {
            self.view.focus(&doc, rows, item);
        }
        self.doc = doc;
        self.links = links;
        self.rows = rows;
        self.made = Some((generation, cols, rows));
    }

    /// Paints it into `area`; returns nothing, as there is no cursor to show.
    pub fn draw(&self, grid: &mut Grid, area: Area) {
        textmode::draw(&self.doc, &self.view, grid, area);
    }

    /// A left click at `col` of page row `row`: the address of the link there, focused.
    pub fn click(&mut self, col: u16, row: u16) -> Option<String> {
        self.view.click(&self.doc, col, row)?;
        self.focused_url()
    }

    /// A key that reached the page: the arrows, PageUp and PageDown, Home and End scroll, Tab and
    /// Shift+Tab move between the links, Enter follows the focused one (its address). Returns
    /// whether it did anything, and the address to open.
    pub fn key(&mut self, key: &KeyEvent) -> (bool, Option<String>) {
        let Some(nav) = Nav::of(key) else {
            return (false, None);
        };
        let (doc, view, rows) = (&self.doc, &mut self.view, self.rows);
        match nav {
            Nav::Down => view.scroll_lines(doc, rows, 1),
            Nav::Up => view.scroll_lines(doc, rows, -1),
            Nav::PageDown => view.scroll_pages(doc, rows, 1),
            Nav::PageUp => view.scroll_pages(doc, rows, -1),
            Nav::Top => view.scroll_top(),
            Nav::Bottom => view.scroll_bottom(doc, rows),
            Nav::Next => view.focus_next(doc, rows),
            Nav::Previous => view.focus_previous(doc, rows),
            Nav::Activate if key.repeat => return (true, None),
            Nav::Activate => return (true, self.focused_url()),
        }
        (true, None)
    }

    /// The wheel: `lines` down, or up when negative.
    pub fn wheel(&mut self, lines: isize) {
        self.view.scroll_lines(&self.doc, self.rows, lines);
    }

    /// The address of the focused link.
    fn focused_url(&self) -> Option<String> {
        let node = self.doc.items.get(self.view.focused_item?)?.node;
        let (_, url) = self.links.iter().find(|(link, _)| *link == node)?;
        Some(url.clone())
    }

    /// Its lines as text, for tests.
    #[cfg(test)]
    fn text(&self) -> Vec<String> {
        self.doc
            .lines
            .iter()
            .map(|line| line.spans.iter().map(|span| span.text.as_str()).collect())
            .collect()
    }
}

/// A document with nothing in it.
fn empty() -> SemanticSnapshot {
    Builder::new().snapshot()
}

/// Home's document with `sections` (the empty ones left out) for a width of `cols`, and its
/// links with their addresses.
fn document(
    home: &Home,
    sections: &[Section],
    cols: usize,
) -> (SemanticSnapshot, Vec<(NodeId, String)>) {
    let mut b = Builder::new();
    let title = format!("Sighurt {}", home.version);
    b.add(Builder::ROOT, NodeKind::Heading(2), Some(title.trim_end()));
    let file = match home.config_path.as_str() {
        "" => "its configuration file",
        path => path,
    };
    let hint = format!(
        "Type an address, a search or a :command in the address line. Engines, search, keys \
         and colors are set in {file}."
    );
    b.paragraph(&hint);
    for section in sections {
        match section {
            Section::Pages if !home.pages.is_empty() => {
                let list = b.add(Builder::ROOT, NodeKind::List, Some("PAGES"));
                for page in &home.pages {
                    let p = b.add(list, NodeKind::ListItem, None);
                    b.link(p, &page.title, &page.url);
                    if !page.detail.is_empty() {
                        b.add(p, NodeKind::Text, Some(" "));
                        b.add(p, NodeKind::Code, Some(&page.detail));
                    }
                }
            }
            Section::Keys if !home.keys.is_empty() => {
                let keys = format!("KEYS\n{}", key_columns(home, cols));
                b.add(Builder::ROOT, NodeKind::Pre, Some(&keys));
            }
            Section::Recent if !home.recent.is_empty() => {
                let list = b.add(Builder::ROOT, NodeKind::List, Some("RECENTLY VISITED"));
                for visit in &home.recent {
                    let p = b.add(list, NodeKind::ListItem, None);
                    let title = visit.title.trim();
                    b.link(
                        p,
                        if title.is_empty() { &visit.url } else { title },
                        &visit.url,
                    );
                    if !title.is_empty() {
                        b.add(p, NodeKind::Text, Some(" "));
                        b.add(p, NodeKind::Code, Some(&visit.url));
                    }
                }
            }
            Section::Engines if !home.engines.is_empty() => {
                let list = b.add(Builder::ROOT, NodeKind::List, Some("ENGINES"));
                for engine in &home.engines {
                    let item = b.add(list, NodeKind::ListItem, None);
                    b.add(item, NodeKind::Text, Some(&format!("{} ", engine.name)));
                    b.add(item, NodeKind::Code, Some(&engine.detail));
                }
            }
            _ => {}
        }
    }
    let links = b.links.clone();
    (b.snapshot(), links)
}

/// The keys and what they do as aligned lines: two columns, the first half down the left one,
/// where both fit in `cols`; else one.
fn key_columns(home: &Home, cols: usize) -> String {
    let width = |text: &str| UnicodeWidthStr::width(text);
    let keys = home
        .keys
        .iter()
        .map(|key| width(&key.keys))
        .max()
        .unwrap_or(0);
    let entry = |index: usize| {
        let key = &home.keys[index];
        let pad = keys.saturating_sub(width(&key.keys));
        format!("{}{}  {}", key.keys, " ".repeat(pad), key.action)
    };
    let entries: Vec<String> = (0..home.keys.len()).map(entry).collect();
    let widest = entries.iter().map(|e| width(e)).max().unwrap_or(0);
    let two = entries.len() > 1 && 2 * widest + KEY_GUTTER <= cols;
    let rows = match two {
        true => entries.len().div_ceil(2),
        false => entries.len(),
    };
    let mut lines = Vec::with_capacity(rows);
    for row in 0..rows {
        let mut line = entries[row].clone();
        if let Some(right) = entries.get(row + rows).filter(|_| two) {
            let pad = widest - width(&line) + KEY_GUTTER;
            line.push_str(&" ".repeat(pad));
            line.push_str(right);
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// Builds a snapshot's nodes, the document first.
struct Builder {
    nodes: Vec<SemanticNode>,
    links: Vec<(NodeId, String)>,
}

impl Builder {
    const ROOT: u64 = 1;

    fn new() -> Self {
        Self {
            nodes: vec![SemanticNode {
                id: NodeId(Self::ROOT),
                kind: NodeKind::Document,
                ..SemanticNode::default()
            }],
            links: Vec::new(),
        }
    }

    /// A node of `kind` with `text`, the last child of `parent`; its id.
    fn add(&mut self, parent: u64, kind: NodeKind, text: Option<&str>) -> u64 {
        let id = self.nodes.len() as u64 + 1;
        self.nodes.push(SemanticNode {
            id: NodeId(id),
            kind,
            text: text.map(str::to_owned),
            ..SemanticNode::default()
        });
        self.nodes[(parent - 1) as usize].children.push(NodeId(id));
        id
    }

    /// A paragraph of `text`.
    fn paragraph(&mut self, text: &str) {
        let p = self.add(Self::ROOT, NodeKind::Paragraph, None);
        self.add(p, NodeKind::Text, Some(text));
    }

    /// A link saying `text` to `url` in `parent`.
    fn link(&mut self, parent: u64, text: &str, url: &str) {
        let link = self.add(parent, NodeKind::Link, None);
        self.add(link, NodeKind::Text, Some(text));
        self.nodes[(link - 1) as usize].href = Some(url.to_owned());
        self.links.push((NodeId(link), url.to_owned()));
    }

    fn snapshot(self) -> SemanticSnapshot {
        SemanticSnapshot {
            // Home's document: links act here, never in an engine.
            document: DocumentGeneration::next(),
            root: NodeId(Self::ROOT),
            nodes: self.nodes,
            focus: None,
            truncated: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use sighurt_api::input::{ButtonState, Key, KeyCode, Modifiers};
    use sighurt_runtime::home::{EngineLine, Key as HomeKey, Link};

    use super::*;

    fn home() -> Home {
        let link = |url: &str, title: &str, detail: &str| Link {
            url: url.into(),
            title: title.into(),
            detail: detail.into(),
        };
        let key = |keys: &str, action: &str| HomeKey {
            keys: keys.into(),
            action: action.into(),
        };
        Home {
            version: "0.10.0".into(),
            config_path: "/home/u/.config/sighurt/config.toml".into(),
            pages: vec![
                link(
                    "sig://help",
                    "Help",
                    "Getting started, the keys, the commands",
                ),
                link("sig://help/keys", "Keys", "Every key of the keymap in use"),
                link(
                    "sig://config",
                    "Configuration",
                    "Edit the configuration file in your editor",
                ),
                link(
                    "sig://terminal",
                    "Terminal",
                    "Off: [pages.terminal] enabled = true turns it on",
                ),
            ],
            keys: vec![
                key("o, ctrl-o", "Address or search"),
                key("escape", "INSERT mode"),
                key("escape", "Back to NORMAL mode"),
                key("t", "New tab"),
                key("x, d", "Close the tab"),
                key("J", "Next tab"),
                key("K", "Previous tab"),
                key("H", "Back"),
                key("L", "Forward"),
                key("/", "Find in the page"),
                key("?, f1", "Help"),
                key("q, ctrl-c", "Quit sig"),
            ],
            recent: vec![link("https://a.test/x", "A test", "")],
            engines: vec![EngineLine {
                name: "demo".into(),
                detail: "compiled in, CPU · default".into(),
                available: true,
            }],
        }
    }

    fn laid_out(cols: u16, rows: u16) -> HomePage {
        let mut page = HomePage::new();
        page.lay_out(&home(), 1, cols, rows);
        page
    }

    fn has(page: &HomePage, heading: &str) -> bool {
        page.text().iter().any(|line| line == heading)
    }

    fn press(name: &str, shift: bool) -> KeyEvent {
        KeyEvent {
            code: KeyCode(0),
            key: Key::Named(name.into()),
            state: ButtonState::Pressed,
            repeat: false,
            modifiers: Modifiers {
                shift,
                ..Modifiers::default()
            },
            consumed: Modifiers::default(),
            text: None,
        }
    }

    #[test]
    fn a_tall_area_shows_every_section_and_short_ones_drop_from_the_bottom() {
        let sections = ["PAGES", "KEYS", "RECENTLY VISITED", "ENGINES"];
        let shown = |page: &HomePage| -> Vec<&str> {
            sections.into_iter().filter(|s| has(page, s)).collect()
        };
        let tall = laid_out(80, 60);
        assert_eq!(shown(&tall), sections);
        assert!(tall.text()[0].starts_with("Sighurt 0.10.0"));
        // 80x24: the page rows are 22.
        let normal = laid_out(80, 22);
        assert_eq!(
            shown(&normal),
            ["PAGES", "KEYS", "RECENTLY VISITED"],
            "{:#?}",
            normal.text()
        );
        assert!(normal.text().len() <= 22, "{:#?}", normal.text());
        // The keys in two columns where they fit, the first half down the left.
        let keys = normal.text();
        let first = keys.iter().find(|l| l.starts_with("o, ctrl-o")).unwrap();
        assert!(first.contains("Previous tab"), "{first}");
        // 40x10 (8 rows): the pages only, in one column of keys if they were shown.
        let small = laid_out(40, 8);
        assert_eq!(shown(&small), ["PAGES"], "{:#?}", small.text());
        assert!(small.text().iter().all(|l| l.width() <= 40));
        // 20x5 (3 rows): still the pages, scrolled to.
        let mut tiny = laid_out(20, 3);
        assert_eq!(shown(&tiny), ["PAGES"]);
        assert!(tiny.text().iter().all(|l| l.width() <= 20));
        let lines = tiny.text().len();
        for _ in 0..lines {
            tiny.key(&press("ArrowDown", false));
        }
        assert_eq!(
            tiny.view.scroll,
            lines - 3,
            "scrolls to the end, no further"
        );
    }

    #[test]
    fn links_are_followed_by_click_and_by_tab_and_enter() {
        let mut page = laid_out(80, 22);
        let row = page
            .text()
            .iter()
            .position(|line| line.starts_with("• Configuration"))
            .unwrap();
        assert_eq!(page.click(4, row as u16), Some("sig://config".into()));
        // Off the link's text: nothing.
        assert_eq!(page.click(70, row as u16), None);
        let mut page = laid_out(80, 22);
        assert_eq!(page.key(&press("Tab", false)), (true, None));
        assert_eq!(
            page.key(&press("Enter", false)),
            (true, Some("sig://help".into()))
        );
        page.key(&press("Tab", false));
        page.key(&press("Tab", true));
        assert_eq!(
            page.key(&press("Enter", false)).1,
            Some("sig://help".into())
        );
        page.key(&press("Tab", true));
        assert_eq!(
            page.key(&press("Enter", false)).1,
            Some("https://a.test/x".into()),
            "Shift+Tab wraps to the last link shown"
        );
        // A character is not home's.
        let mut letter = press("a", false);
        letter.key = Key::Character("a".into());
        assert_eq!(page.key(&letter), (false, None));
        // A new size keeps the focused link.
        page.lay_out(&home(), 1, 70, 40);
        assert_eq!(
            page.key(&press("Enter", false)).1,
            Some("https://a.test/x".into())
        );
        // A new home starts over.
        page.lay_out(&home(), 2, 70, 40);
        assert_eq!(page.key(&press("Enter", false)).1, None);
    }

    #[test]
    fn what_home_says_is_sanitized_and_cut_to_the_width() {
        let mut hostile = home();
        hostile.pages[0].title = "He\u{1b}[31mlp\u{202e}".into();
        hostile.keys[0].action = "x".repeat(200);
        let mut page = HomePage::new();
        page.lay_out(&hostile, 1, 40, 60);
        for line in page.text() {
            assert!(
                !line.contains('\u{1b}') && !line.contains('\u{202e}'),
                "{line:?}"
            );
            assert!(line.width() <= 40, "{line:?}");
        }
        assert!(page.text().iter().any(|line| line.ends_with('…')));
    }
}
