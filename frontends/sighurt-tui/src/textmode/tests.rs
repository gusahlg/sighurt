//! Tests of text mode: layout, dump goldens, navigation, drawing. Fixtures are built in code.
//!
//! Owns: the fixture snapshots and their checks. Must not own: anything the modules above
//! don't expose. Golden dumps live in `tests/textmode/*.txt`; set `UPDATE_GOLDEN=1` to rewrite.

use sighurt_api::ids::DocumentGeneration;
use sighurt_api::semantic::{NodeId, NodeKind, SemanticAction, SemanticNode, SemanticSnapshot};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::*;
use crate::grid::Grid;

#[derive(Default)]
struct Fx {
    nodes: Vec<SemanticNode>,
}

impl Fx {
    fn new() -> Self {
        let mut fx = Self::default();
        fx.nodes.push(SemanticNode {
            id: NodeId(1),
            kind: NodeKind::Document,
            ..SemanticNode::default()
        });
        fx
    }

    fn add(&mut self, parent: u64, kind: NodeKind, text: Option<&str>) -> u64 {
        let id = self.nodes.len() as u64 + 1;
        self.nodes.push(SemanticNode {
            id: NodeId(id),
            kind,
            text: text.map(str::to_string),
            ..SemanticNode::default()
        });
        self.nodes[parent as usize - 1].children.push(NodeId(id));
        id
    }

    fn set(&mut self, id: u64) -> &mut SemanticNode {
        &mut self.nodes[id as usize - 1]
    }

    fn link(&mut self, parent: u64, text: &str, href: &str) -> u64 {
        let id = self.add(parent, NodeKind::Link, None);
        self.add(id, NodeKind::Text, Some(text));
        self.set(id).href = Some(href.to_string());
        id
    }

    fn para(&mut self, parent: u64, text: &str) -> u64 {
        let p = self.add(parent, NodeKind::Paragraph, None);
        self.add(p, NodeKind::Text, Some(text));
        p
    }

    fn snap(self) -> SemanticSnapshot {
        SemanticSnapshot {
            document: DocumentGeneration::next(),
            root: NodeId(1),
            nodes: self.nodes,
            focus: None,
            truncated: false,
        }
    }
}

fn article() -> SemanticSnapshot {
    let mut f = Fx::new();
    let h = f.add(1, NodeKind::Heading(1), Some("Rust 2030 released"));
    let _ = h;
    let p = f.add(1, NodeKind::Paragraph, None);
    f.add(
        p,
        NodeKind::Text,
        Some("The language team announced the edition today, with notes on "),
    );
    f.link(p, "the migration guide", "https://example.com/guide");
    f.add(p, NodeKind::Text, Some(" and a "));
    f.link(p, "changelog", "https://example.com/changes");
    f.add(p, NodeKind::Text, Some(". Readers wrote in at length."));
    f.add(1, NodeKind::Heading(2), Some("Reactions"));
    let q = f.add(1, NodeKind::Quote, None);
    f.para(q, "It is the best release yet, and I say that every year.");
    let img = f.add(1, NodeKind::Image, Some("A crab"));
    let _ = img;
    f.add(1, NodeKind::Separator, None);
    f.para(1, "Footer text.");
    f.snap()
}

fn form() -> SemanticSnapshot {
    let mut f = Fx::new();
    f.add(1, NodeKind::Heading(1), Some("Sign up"));
    let c = f.add(1, NodeKind::Container, None);
    let name = f.add(c, NodeKind::TextField, None);
    f.set(name).label = Some("Name".into());
    f.set(name).value = Some("Ada".into());
    let c = f.add(1, NodeKind::Container, None);
    let pw = f.add(c, NodeKind::TextField, None);
    f.set(pw).label = Some("Password".into());
    f.set(pw).password = true;
    let c = f.add(1, NodeKind::Container, None);
    let cb = f.add(c, NodeKind::Checkbox, None);
    f.set(cb).label = Some("Subscribe".into());
    f.set(cb).checked = Some(true);
    let c = f.add(1, NodeKind::Container, None);
    let r1 = f.add(c, NodeKind::Radio, None);
    f.set(r1).label = Some("Monthly".into());
    let c = f.add(1, NodeKind::Container, None);
    let r2 = f.add(c, NodeKind::Radio, None);
    f.set(r2).label = Some("Yearly".into());
    f.set(r2).checked = Some(true);
    let c = f.add(1, NodeKind::Container, None);
    let sel = f.add(c, NodeKind::Select, None);
    f.set(sel).label = Some("Plan".into());
    f.set(sel).options = vec!["Free".into(), "Pro".into()];
    f.set(sel).selected = Some(1);
    let area = f.add(1, NodeKind::TextArea, None);
    f.set(area).label = Some("Notes".into());
    f.set(area).value = Some("one\ntwo".into());
    let c = f.add(1, NodeKind::Container, None);
    f.add(c, NodeKind::Button, Some("Send"));
    let off = f.add(c, NodeKind::Button, Some("Cancel"));
    f.set(off).disabled = true;
    f.snap()
}

fn nested_lists() -> SemanticSnapshot {
    let mut f = Fx::new();
    let list = f.add(1, NodeKind::List, None);
    let a = f.add(list, NodeKind::ListItem, None);
    f.add(
        a,
        NodeKind::Text,
        Some("First item with enough words to wrap around the narrow measure"),
    );
    let inner = f.add(a, NodeKind::List, None);
    let b = f.add(inner, NodeKind::ListItem, Some("Nested one"));
    let _ = b;
    let c = f.add(inner, NodeKind::ListItem, None);
    f.add(c, NodeKind::Text, Some("Nested two"));
    let deep = f.add(c, NodeKind::List, None);
    f.add(deep, NodeKind::ListItem, Some("Deepest"));
    let d = f.add(list, NodeKind::ListItem, None);
    f.para(d, "Second item, a paragraph");
    f.snap()
}

fn wide_table() -> SemanticSnapshot {
    let mut f = Fx::new();
    let t = f.add(1, NodeKind::Table, None);
    let cap = f.add(t, NodeKind::Paragraph, None);
    f.add(cap, NodeKind::Text, Some("A few crates"));
    // thead/tbody arrive as containers around the rows.
    let body = f.add(t, NodeKind::Container, None);
    for (a, b, c) in [
        (
            "Crate",
            "Version",
            "Description of the crate that is rather long indeed",
        ),
        (
            "serde",
            "1.0.200",
            "Serialization framework for everything under the sun",
        ),
        ("tokio", "1.40", "Async runtime"),
    ] {
        let row = f.add(body, NodeKind::Row, None);
        for text in [a, b, c] {
            let cell = f.add(row, NodeKind::Cell, None);
            f.add(cell, NodeKind::Text, Some(text));
        }
    }
    f.snap()
}

fn pre_page() -> SemanticSnapshot {
    let mut f = Fx::new();
    f.para(1, "Code:");
    let pre = f.add(1, NodeKind::Pre, Some("fn main() {\n\tprintln!(\"hello\");\n    let very_long_line_that_goes_on = \"and on and on and on and on and on\";\n}\n"));
    let _ = pre;
    f.snap()
}

fn hostile() -> SemanticSnapshot {
    let mut f = Fx::new();
    f.para(
        1,
        "evil \u{1b}[31mred\u{1b}[0m \u{202e}reversed\u{202c} \u{9b}31m \u{1b}]0;title\u{7}done",
    );
    f.para(1, &"x".repeat(100));
    f.para(
        1,
        "日本語のテキストはとても長い文章です。絵文字 😀👩‍👩‍👧 と 🇸🇪 も。",
    );
    f.link(1, "bad\u{1b}[2Jlink", "https://e.com/\u{1b}[2J");
    let pre = f.add(1, NodeKind::Pre, Some("\u{1b}[2Jpre\u{1b}[0m"));
    let _ = pre;
    f.snap()
}

fn empty() -> SemanticSnapshot {
    Fx::new().snap()
}

fn text_of(line: &TextLine) -> String {
    line.spans.iter().map(|s| s.text.as_str()).collect()
}

fn no_controls(s: &str) {
    assert!(
        !s.chars().any(|c| c.is_control() && c != '\n'),
        "control character in {s:?}"
    );
    assert!(!s.contains('\u{202e}'));
}

fn check_widths(doc: &TextDocument) {
    for line in &doc.lines {
        let t = text_of(line);
        no_controls(&t);
        let wide = t.width();
        // A single grapheme wider than the line is the one allowed overflow.
        assert!(
            wide <= usize::from(doc.width) || t.graphemes(true).count() == 1,
            "line {t:?} is {wide} wide in {}",
            doc.width
        );
    }
    for item in &doc.items {
        for r in &item.ranges {
            assert!(r.line < doc.lines.len() && r.start <= r.end);
        }
    }
}

fn golden(name: &str, got: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/textmode")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, got).unwrap();
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing golden {}", path.display()));
    assert_eq!(
        got, want,
        "golden {name} differs (UPDATE_GOLDEN=1 rewrites)"
    );
}

#[test]
fn goldens() {
    let cases: Vec<(&str, SemanticSnapshot, u16)> = vec![
        ("article-20", article(), 20),
        ("article-40", article(), 40),
        ("article-80", article(), 80),
        ("article-200", article(), 200),
        ("form-60", form(), 60),
        ("lists-24", nested_lists(), 24),
        ("table-wide-100", wide_table(), 100),
        ("table-stacked-40", wide_table(), 40),
        ("pre-30", pre_page(), 30),
        ("hostile-20", hostile(), 20),
        ("empty-80", empty(), 80),
    ];
    for (name, snap, width) in cases {
        golden(name, &dump(&snap, width));
    }
}

#[test]
fn every_fixture_fits_every_width() {
    for snap in [
        article(),
        form(),
        nested_lists(),
        wide_table(),
        pre_page(),
        hostile(),
        empty(),
    ] {
        for width in [1, 2, 5, 20, 80, 200] {
            let doc = layout(&snap, width);
            if width >= 5 {
                check_widths(&doc);
            }
            no_controls(&dump(&snap, width));
        }
    }
}

#[test]
fn links_are_numbered_and_listed() {
    let out = dump(&article(), 200);
    assert!(out.contains("the migration guide[1]"), "{out}");
    assert!(out.contains("changelog[2]"));
    assert!(out.contains(
        "\nReferences\n\n1. https://example.com/guide\n2. https://example.com/changes\n"
    ));
}

#[test]
fn form_controls_have_their_shapes() {
    let out = dump(&form(), 80);
    for want in [
        "Name [Ada_______]",
        "Password [********]",
        "[x] Subscribe",
        "( ) Monthly",
        "(o) Yearly",
        "Plan [Pro ▾]",
        "[ Send ]",
    ] {
        assert!(out.contains(want), "{want} missing from\n{out}");
    }
}

#[test]
fn hostile_text_cannot_reach_the_terminal() {
    let out = dump(&hostile(), 40);
    assert!(!out.contains('\u{1b}') && !out.contains('\u{9b}'));
    assert!(out.contains("evil red reversed"));
    assert!(!out.contains("2J"), "{out}");
    let doc = layout(&hostile(), 7);
    check_widths(&doc);
}

#[test]
fn a_huge_word_breaks_and_wide_graphemes_stay_whole() {
    let doc = layout(&hostile(), 20);
    let xs: Vec<String> = doc
        .lines
        .iter()
        .map(text_of)
        .filter(|t| t.starts_with('x'))
        .collect();
    assert_eq!(xs.len(), 5);
    assert!(xs.iter().all(|t| t.len() == 20));
    // The family emoji is one grapheme: never split across lines.
    let all: String = doc.lines.iter().map(text_of).collect::<Vec<_>>().join("\n");
    assert!(all.contains("👩\u{200d}👩\u{200d}👧"));
    // Width 1: wide characters cannot fit; they still appear whole, one per line.
    let narrow = layout(&hostile(), 1);
    assert!(narrow.lines.iter().any(|l| text_of(l).contains('日')));
}

#[test]
fn pre_is_kept_and_clipped() {
    let out = dump(&pre_page(), 30);
    assert!(out.contains("    println!(\"hello\");"));
    assert!(out.contains("…"));
}

#[test]
fn table_aligns_when_it_fits() {
    let out = dump(&wide_table(), 100);
    assert!(out.contains("A few crates"), "{out}");
    let rows: Vec<&str> = out.lines().filter(|l| l.contains(" | ")).collect();
    assert_eq!(rows.len(), 3);
    let cols: Vec<usize> = rows.iter().map(|r| r.find(" | ").unwrap()).collect();
    assert!(cols.iter().all(|c| *c == cols[0]));
    let stacked = dump(&wide_table(), 40);
    assert!(stacked.contains("A few crates\n"));
    assert!(stacked.contains("serde / 1.0.200 / "), "{stacked}");
}

#[test]
fn an_unselected_select_shows_no_option() {
    let mut f = Fx::new();
    let s = f.add(1, NodeKind::Select, None);
    f.set(s).options = vec!["Free".into(), "Pro".into()];
    let out = dump(&f.snap(), 80);
    assert!(out.contains("[ ▾]"), "{out}");
    assert!(!out.contains("Free") && !out.contains("Pro"), "{out}");
}

#[test]
fn empty_document_is_empty() {
    let doc = layout(&empty(), 80);
    assert!(doc.lines.is_empty() && doc.items.is_empty());
    assert_eq!(dump(&empty(), 80), "");
}

#[test]
fn a_truncated_document_says_so_and_a_big_one_is_fast() {
    let mut f = Fx::new();
    let mut links = 0u32;
    // A paragraph, its text, a link and the link's text: four nodes. Fill up to the cap.
    while f.nodes.len() + 4 <= 20_000 {
        let p = f.add(1, NodeKind::Paragraph, None);
        f.add(
            p,
            NodeKind::Text,
            Some("Paragraph number with several words to wrap around "),
        );
        f.link(
            p,
            &format!("link {links}"),
            &format!("https://example.com/{links}"),
        );
        links += 1;
    }
    while f.nodes.len() < 20_000 {
        f.add(1, NodeKind::Paragraph, Some("tail"));
    }
    let mut snap = f.snap();
    snap.truncated = true;
    assert_eq!(snap.nodes.len(), 20_000);
    let start = std::time::Instant::now();
    let doc = layout(&snap, 80);
    let took = start.elapsed();
    eprintln!("20k-node layout: {took:?} ({links} links)");
    // Debug builds vary. 50 ms is the aim; a few times that still means the walk is linear.
    assert!(took < std::time::Duration::from_millis(500), "{took:?}");
    assert_eq!(doc.items.len(), usize::try_from(links).unwrap());
    assert!(text_of(doc.lines.last().unwrap()).contains("(truncated)"));
}

#[test]
fn malformed_snapshots_do_not_loop() {
    let mut f = Fx::new();
    let p = f.add(1, NodeKind::Container, Some("loop"));
    f.set(p).children.push(NodeId(1));
    f.set(p).children.push(NodeId(p));
    f.set(1).children.push(NodeId(77));
    let doc = layout(&f.snap(), 20);
    assert_eq!(doc.lines.len(), 1);
}

#[test]
fn items_map_to_columns_and_nodes() {
    let doc = layout(&article(), 200);
    assert_eq!(doc.items.len(), 2);
    let it = &doc.items[0];
    assert_eq!(it.kind, ItemKind::Link);
    assert_eq!(it.href.as_deref(), Some("https://example.com/guide"));
    let r = it.ranges[0];
    let line = text_of(&doc.lines[r.line]);
    let got: String = line
        .chars()
        .skip(r.start as usize)
        .take((r.end - r.start) as usize)
        .collect();
    assert_eq!(got, "the migration guide[1]");
    assert_eq!(doc.item_index(it.node), Some(0));
}

fn view_fixture() -> (TextDocument, TextView) {
    let mut f = Fx::new();
    for i in 0..30 {
        let p = f.add(1, NodeKind::Paragraph, None);
        f.link(p, &format!("item {i}"), &format!("https://e.com/{i}"));
    }
    let doc = layout(&f.snap(), 40);
    (doc, TextView::new(40))
}

#[test]
fn tab_order_wraps_and_scrolls_into_view() {
    let (doc, mut v) = view_fixture();
    let rows = 10;
    v.focus_next(&doc, rows);
    assert_eq!(v.focused_item, Some(0));
    for i in 1..30 {
        v.focus_next(&doc, rows);
        assert_eq!(v.focused_item, Some(i));
        let line = doc.items[i].ranges[0].line;
        assert!(
            line >= v.scroll && line < v.scroll + usize::from(rows),
            "item {i} off screen"
        );
    }
    v.focus_next(&doc, rows);
    assert_eq!(v.focused_item, Some(0));
    assert_eq!(v.scroll, doc.items[0].ranges[0].line.min(v.scroll));
    v.focus_previous(&doc, rows);
    assert_eq!(v.focused_item, Some(29));
    assert!(doc.items[29].ranges[0].line < v.scroll + usize::from(rows));
}

#[test]
fn scrolling_stops_at_the_ends() {
    let (doc, mut v) = view_fixture();
    let rows = 10;
    v.scroll_lines(&doc, rows, -5);
    assert_eq!(v.scroll, 0);
    v.scroll_pages(&doc, rows, 1);
    assert_eq!(v.scroll, 9);
    v.scroll_bottom(&doc, rows);
    assert_eq!(v.scroll, doc.lines.len() - 10);
    v.scroll_lines(&doc, rows, 99);
    assert_eq!(v.scroll, doc.lines.len() - 10);
    v.scroll_top();
    assert_eq!(v.scroll, 0);
}

#[test]
fn clicks_find_items_and_activation_gives_intents() {
    let doc = layout(&form(), 80);
    let mut v = TextView::new(80);
    let find = |kind| doc.items.iter().position(|i| i.kind == kind).unwrap();
    // Click the text field's bracket.
    let name = &doc.items[find(ItemKind::TextField)];
    let r = name.ranges[0];
    let hit = v.click(&doc, r.start + 1, u16::try_from(r.line).unwrap());
    assert_eq!(hit, Some(find(ItemKind::TextField)));
    assert_eq!(v.item_at(&doc, 79, 0), None);
    assert_eq!(
        v.activate(&doc),
        Some(Intent::EditField {
            node: name.node,
            value: "Ada".into()
        })
    );
    v.focused_item = Some(find(ItemKind::Select));
    match v.activate(&doc) {
        Some(Intent::ChooseOption {
            options, selected, ..
        }) => {
            assert_eq!(options, ["Free", "Pro"]);
            assert_eq!(selected, Some(1));
        }
        other => panic!("{other:?}"),
    }
    v.focused_item = Some(find(ItemKind::Checkbox));
    assert!(matches!(
        v.activate(&doc),
        Some(Intent::Action(SemanticAction::Activate(_), None))
    ));
    // The disabled button does nothing.
    v.focused_item = Some(doc.items.len() - 1);
    assert_eq!(v.activate(&doc), None);
    let a = layout(&article(), 80);
    let mut v = TextView::new(80);
    v.focused_item = Some(1);
    assert_eq!(
        v.activate(&a),
        Some(Intent::Action(
            SemanticAction::Activate(a.items[1].node),
            Some("https://example.com/changes".into())
        ))
    );
    v.focused_item = None;
    assert_eq!(v.activate(&a), None);
}

#[test]
fn a_new_snapshot_of_the_same_document_keeps_place() {
    let mut f = Fx::new();
    for i in 0..40 {
        let p = f.add(1, NodeKind::Paragraph, None);
        f.link(p, &format!("item {i}"), &format!("https://e.com/{i}"));
    }
    let snap = f.snap();
    let old = layout(&snap, 40);
    let mut v = TextView::new(40);
    v.retarget(&old, &old);
    v.focus(&old, 10, 20);
    let scroll = v.scroll;
    assert!(scroll > 0);
    // The same document grows a paragraph at the top: ids stay, content shifts.
    let mut next = snap.clone();
    let top = NodeId(1000);
    next.nodes.push(SemanticNode {
        id: top,
        kind: NodeKind::Paragraph,
        text: Some("New banner".into()),
        ..SemanticNode::default()
    });
    next.nodes[0].children.insert(0, top);
    let new = layout(&next, 40);
    v.retarget(&old, &new);
    assert_eq!(new.items[v.focused_item.unwrap()].node, old.items[20].node);
    assert_eq!(
        v.scroll,
        scroll + 2,
        "the content at the top row stays at the top row"
    );
    // Another document resets.
    let mut other = snap.clone();
    other.document = DocumentGeneration::next();
    let fresh = layout(&other, 40);
    v.retarget(&new, &fresh);
    assert_eq!((v.scroll, v.focused_item), (0, None));
}

#[test]
fn draw_paints_styles_and_clips() {
    let doc = layout(&article(), 80);
    let mut grid = Grid::new(30, 8);
    let mut v = TextView::new(30);
    v.focused_item = Some(0);
    draw(
        &doc,
        &v,
        &mut grid,
        Area {
            col: 0,
            row: 1,
            cols: 30,
            rows: 6,
        },
    );
    // Row 0 is outside the area; the heading is on row 1, bold and underlined.
    let c = grid.cell(0, 1).unwrap();
    assert_eq!(c.grapheme, "R");
    assert!(c.attrs.bold && c.attrs.underline);
    // The focused link is reversed where it is visible.
    let reversed =
        (0..8).any(|r| (0..30).any(|c| grid.cell(c, r).is_some_and(|x| x.attrs.reverse)));
    assert!(reversed);
    assert_eq!(grid.cell(0, 0).unwrap().grapheme, " ");
    assert_eq!(grid.cell(0, 7).unwrap().grapheme, " ");
}

#[test]
fn a_field_stores_the_raw_value_and_draws_a_sanitized_rendering() {
    let mut f = Fx::new();
    let field = f.add(1, NodeKind::TextField, None);
    f.set(field).value = Some("  two  spaces".into());
    let leaked = f.add(1, NodeKind::TextField, None);
    f.set(leaked).password = true;
    f.set(leaked).value = Some("secret".into());
    let area = f.add(1, NodeKind::TextArea, None);
    let mut raw = String::from("keep\tthis\n");
    for i in 0..25 {
        raw.push_str(&format!("line {i}\n"));
    }
    f.set(area).value = Some(raw.clone());
    let doc = layout(&f.snap(), 60);
    let text = doc
        .items
        .iter()
        .find(|item| item.kind == ItemKind::TextField && !item.password)
        .unwrap();
    assert_eq!(text.value, "  two  spaces");
    let drawn = doc.lines.iter().map(text_of).collect::<Vec<_>>().join("\n");
    assert!(drawn.contains("[two spaces]"), "{drawn}");
    assert!(!drawn.contains("  two  spaces"), "{drawn}");
    let password = doc.items.iter().find(|item| item.password).unwrap();
    assert_eq!(password.value, "");
    assert!(drawn.contains("********"), "{drawn}");
    assert!(!drawn.contains("secret"), "{drawn}");
    let area_at = doc
        .items
        .iter()
        .position(|item| item.kind == ItemKind::TextArea)
        .unwrap();
    assert_eq!(doc.items[area_at].value, raw);
    assert!(doc.items[area_at].value.contains('\t'));
    assert!(doc.items[area_at].value.contains("line 24"));
    let area_lines: Vec<_> = doc
        .lines
        .iter()
        .filter(|line| line.spans.iter().any(|span| span.item == Some(area_at)))
        .map(text_of)
        .collect();
    assert_eq!(area_lines.len(), 20, "{area_lines:?}");
    assert!(area_lines[0].contains("keep    this"), "{area_lines:?}");
    assert!(area_lines.iter().all(|line| !line.contains('\t')));
    assert!(area_lines.iter().all(|line| !line.contains("line 19")));
    assert!(area_lines.iter().all(|line| !line.contains("line 24")));
    check_widths(&doc);
}

/// The text of `line`'s cells `range.start..range.end`.
fn cells_text(doc: &TextDocument, range: CellRange) -> String {
    let text = text_of(&doc.lines[range.line]);
    let mut out = String::new();
    let mut col = 0usize;
    for g in text.graphemes(true) {
        if col >= usize::from(range.start) && col < usize::from(range.end) {
            out.push_str(g);
        }
        col += g.width();
    }
    out
}

#[test]
fn find_searches_the_text_before_wrapping_and_maps_matches_to_rows() {
    let mut f = Fx::new();
    f.para(1, "alpha bravo charlie delta echo foxtrot");
    let doc = layout(&f.snap(), 12);
    // Wrapped: "alpha bravo" / "charlie" / "delta echo" / "foxtrot".
    assert!(doc.lines.len() >= 3, "{:?}", doc.lines);
    // A match across a wrap: the joining space shows on no row.
    let found = find(&doc, "bravo charlie", false);
    assert_eq!(found.len(), 1);
    let rows: Vec<String> = found[0].iter().map(|r| cells_text(&doc, *r)).collect();
    assert_eq!(rows, ["bravo", "charlie"]);
    assert_eq!(found[0][0].line + 1, found[0][1].line);
    // Case: either, unless asked.
    assert_eq!(find(&doc, "ECHO", false).len(), 1);
    assert!(find(&doc, "ECHO", true).is_empty());
    assert_eq!(find(&doc, "echo", true).len(), 1);
    // Non-overlapping, in reading order.
    let mut f = Fx::new();
    f.para(1, "aaaa");
    f.para(1, "xaa");
    let doc = layout(&f.snap(), 40);
    let found = find(&doc, "aa", false);
    assert_eq!(found.len(), 3);
    assert!(found[0][0].line < found[2][0].line);
    assert!(find(&doc, "", false).is_empty());
}

#[test]
fn find_joins_a_word_broken_inside_and_leaves_prefixes_out() {
    let mut f = Fx::new();
    f.para(1, "supercalifragilistic");
    let doc = layout(&f.snap(), 8);
    let found = find(&doc, "fragil", false);
    assert_eq!(found.len(), 1);
    let text: String = found[0].iter().map(|r| cells_text(&doc, *r)).collect();
    assert_eq!(text, "fragil");
    // A list's bullet and hang are not page text, and its columns are skipped.
    let doc = layout(&nested_lists(), 30);
    let bullet = text_of(&doc.lines[0]);
    let marker = bullet.trim_start().chars().next().unwrap().to_string();
    assert!(find(&doc, &marker, false).is_empty(), "{bullet}");
    let all: Vec<String> = doc.lines.iter().map(text_of).collect();
    let word = all[0]
        .split_whitespace()
        .last()
        .expect("a word on the first row")
        .to_owned();
    let found = find(&doc, &word, true);
    assert!(!found.is_empty());
    assert_eq!(cells_text(&doc, found[0][0]), word);
}

#[test]
fn the_current_match_is_drawn_in_reverse() {
    let mut f = Fx::new();
    f.para(1, "one two three");
    let doc = layout(&f.snap(), 40);
    let mut view = TextView::new(40);
    view.found = find(&doc, "two", false).remove(0);
    let mut grid = Grid::new(40, 3);
    draw(
        &doc,
        &view,
        &mut grid,
        Area {
            col: 0,
            row: 1,
            cols: 40,
            rows: 2,
        },
    );
    let reversed: String = (0..40)
        .filter_map(|col| grid.cell(col, 1))
        .filter(|cell| cell.attrs.reverse)
        .map(|cell| cell.grapheme.clone())
        .collect();
    assert_eq!(reversed, "two");
}
