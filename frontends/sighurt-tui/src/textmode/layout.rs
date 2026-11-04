//! Reflow a [`SemanticSnapshot`] to a column width.
//!
//! Owns: line breaking by display width, the styles on each span, and the map of links and
//! controls. Must not own: the viewport ([`super::TextView`]), the grid, or the terminal.
//!
//! Flowing text breaks at spaces. A word is broken only when it is wider than the line, and
//! then on grapheme boundaries. Every string that is drawn goes through [`crate::sanitize`],
//! except printable ASCII, which that function would leave unchanged. A field's stored value is
//! the snapshot's own string (a password's is empty), so editing can commit it unchanged; the
//! brackets show a collapsed rendering. The snapshot does not say which lists are ordered, so
//! every item gets a bullet. Each node is laid out at most once and nesting is capped, so a
//! malformed snapshot cannot loop or blow up.

use std::collections::{HashMap, HashSet};

use sighurt_api::semantic::{NodeId, NodeKind, SemanticNode, SemanticSnapshot, MAX_DEPTH};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::sanitize::sanitize;

use super::{CellRange, Item, ItemKind, Joined, Span, Style, TextDocument, TextLine};

/// Least columns of a text field's brackets, filled with underscores.
const FIELD_MIN: usize = 10;
/// Most columns of a field's value shown.
const FIELD_MAX: usize = 30;
/// Most lines of a text area shown.
const AREA_LINES: usize = 20;

/// Lay `snapshot` out in `width` columns. A width of 0 is taken as 1.
pub fn layout(snapshot: &SemanticSnapshot, width: u16) -> TextDocument {
    let width = usize::from(width.max(1));
    let mut b = Builder {
        snap: snapshot,
        index: Index::build(&snapshot.nodes),
        done: vec![false; snapshot.nodes.len()],
        width,
        prefix: String::new(),
        lines: Vec::new(),
        items: Vec::new(),
        links: 0,
        depth: 0,
        no_blank: false,
        lists: 0,
        join: Joined::No,
    };
    b.block(snapshot.root);
    if snapshot.truncated {
        b.blank_before();
        let style = Style {
            dim: true,
            ..Style::default()
        };
        b.wrap(vec![Tok::Word("(truncated)".into(), style, None)], None);
    }
    TextDocument {
        document: snapshot.document,
        width: u16::try_from(width).unwrap_or(u16::MAX),
        lines: b.lines,
        items: b.items,
        truncated: snapshot.truncated,
    }
}

/// Inline pieces: words glue together, spaces separate.
#[derive(Clone)]
enum Tok {
    Space(Style, Option<usize>),
    Word(String, Style, Option<usize>),
}

/// First id wins. A table when ids are small; a map when they are sparse or huge.
enum Index {
    Dense(Vec<u32>),
    Map(HashMap<NodeId, usize>),
}

impl Index {
    fn build(nodes: &[SemanticNode]) -> Self {
        let max = nodes.iter().map(|n| n.id.0).max().unwrap_or(0);
        let n = nodes.len();
        // Four times the node count keeps a mostly dense id space in the table.
        let dense_ok =
            n <= u32::MAX as usize && max < 1_000_000 && max <= (n as u64).saturating_mul(4);
        if dense_ok {
            let mut dense = vec![u32::MAX; max as usize + 1];
            for (i, node) in nodes.iter().enumerate() {
                let slot = &mut dense[node.id.0 as usize];
                if *slot == u32::MAX {
                    *slot = i as u32;
                }
            }
            Index::Dense(dense)
        } else {
            let mut map = HashMap::with_capacity(n);
            for (i, node) in nodes.iter().enumerate() {
                map.entry(node.id).or_insert(i);
            }
            Index::Map(map)
        }
    }

    fn get(&self, id: NodeId) -> Option<usize> {
        match self {
            Index::Dense(dense) => {
                let slot = *dense.get(usize::try_from(id.0).ok()?)?;
                if slot == u32::MAX {
                    None
                } else {
                    Some(slot as usize)
                }
            }
            Index::Map(map) => map.get(&id).copied(),
        }
    }
}

/// An unbreakable run of styled parts, or the gap between two.
enum Unit {
    Gap(Style, Option<usize>),
    Run(Vec<(String, Style, Option<usize>)>),
}

fn span(text: String, style: Style, item: Option<usize>) -> Span {
    Span { text, style, item }
}

fn dim() -> Style {
    Style {
        dim: true,
        ..Style::default()
    }
}

fn field(disabled: bool) -> Style {
    Style {
        field: true,
        dim: disabled,
        ..Style::default()
    }
}

fn w(text: &str) -> usize {
    text.width()
}

/// Text from the page for flowing: line breaks and tabs are spaces, controls removed.
fn flow(text: &str) -> String {
    let spaced: String = text
        .chars()
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .collect();
    sanitize(&spaced)
}

/// Text with its whitespace collapsed to single spaces and trimmed.
fn collapse(text: &str) -> String {
    flow(text).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What editing commits. A password's value is never stored: the snapshot is
/// not supposed to carry one, and the brackets show asterisks either way.
fn stored_value(node: &SemanticNode) -> String {
    if node.password {
        String::new()
    } else {
        node.value.clone().unwrap_or_default()
    }
}

/// A rule of `cols` columns. `─` is one column; a wider measure is repeated to fit.
fn rule(cols: usize) -> String {
    let ch = "─";
    let cw = w(ch).max(1);
    ch.repeat(cols / cw)
}

/// Cut `text` to at most `cols` columns, ending in `…` when it was cut.
fn clip(text: &str, cols: usize) -> (String, bool) {
    if w(text) <= cols {
        return (text.to_string(), false);
    }
    let room = cols.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0;
    for g in text.graphemes(true) {
        let gw = w(g);
        if used + gw > room {
            break;
        }
        out.push_str(g);
        used += gw;
    }
    if cols > 0 {
        out.push('…');
    }
    (out, true)
}

/// Printable ASCII, or the line breaks that flowing text turns into spaces.
fn is_plain_flow(text: &str) -> bool {
    text.bytes()
        .all(|b| b == b'\n' || b == b'\r' || b == b'\t' || (0x20..=0x7e).contains(&b))
}

fn words(text: &str, style: Style, item: Option<usize>, out: &mut Vec<Tok>) {
    // Clean ASCII never changes under `flow` + `sanitize`, so split it in place.
    if is_plain_flow(text) {
        split_words(
            text.chars().map(|c| {
                if matches!(c, '\n' | '\r' | '\t') {
                    ' '
                } else {
                    c
                }
            }),
            style,
            item,
            out,
        );
    } else {
        split_words(flow(text).chars(), style, item, out);
    }
}

fn split_words(
    chars: impl Iterator<Item = char>,
    style: Style,
    item: Option<usize>,
    out: &mut Vec<Tok>,
) {
    let mut word = String::new();
    let mut space = false;
    for ch in chars {
        if ch.is_whitespace() {
            if !word.is_empty() {
                out.push(Tok::Word(std::mem::take(&mut word), style, item));
            }
            if !space {
                out.push(Tok::Space(style, item));
                space = true;
            }
        } else {
            space = false;
            word.push(ch);
        }
    }
    if !word.is_empty() {
        out.push(Tok::Word(word, style, item));
    }
}

/// Any word, including an empty one. Spaces alone are not content.
fn has_words(tokens: &[Tok]) -> bool {
    tokens.iter().any(|t| matches!(t, Tok::Word(..)))
}

/// Move `tokens` into runs. Leading and trailing gaps drop; repeated gaps collapse.
fn into_units(tokens: Vec<Tok>) -> Vec<Unit> {
    let mut out: Vec<Unit> = Vec::new();
    let mut run: Vec<(String, Style, Option<usize>)> = Vec::new();
    for tok in tokens {
        match tok {
            Tok::Space(s, i) => {
                if !run.is_empty() {
                    out.push(Unit::Run(std::mem::take(&mut run)));
                }
                if !matches!(out.last(), Some(Unit::Gap(..))) {
                    out.push(Unit::Gap(s, i));
                }
            }
            Tok::Word(t, s, i) => run.push((t, s, i)),
        }
    }
    if !run.is_empty() {
        out.push(Unit::Run(run));
    }
    if let Some(start) = out.iter().position(|u| matches!(u, Unit::Run(..))) {
        if start > 0 {
            out.drain(..start);
        }
    } else {
        out.clear();
    }
    while matches!(out.last(), Some(Unit::Gap(..))) {
        out.pop();
    }
    out
}

fn tokens_width(tokens: &[Tok]) -> usize {
    let mut width = 0usize;
    let mut pending = false;
    let mut started = false;
    for tok in tokens {
        match tok {
            Tok::Space(..) => {
                if started {
                    pending = true;
                }
            }
            Tok::Word(text, ..) => {
                if pending {
                    width += 1;
                    pending = false;
                }
                started = true;
                width += w(text);
            }
        }
    }
    width
}

/// Display width of flowed `tokens`, or `None` when a run has no columns.
///
/// [`Builder::wrap`] drops a zero-width run. The one-line path must not guess at that.
fn flowed_width(tokens: &[Tok]) -> Option<usize> {
    let mut width = 0usize;
    let mut pending = false;
    let mut started = false;
    let mut run = 0usize;
    let mut run_started = false;
    for tok in tokens {
        match tok {
            Tok::Space(..) => {
                if run_started && run == 0 {
                    return None;
                }
                run = 0;
                run_started = false;
                if started {
                    pending = true;
                }
            }
            Tok::Word(text, ..) => {
                let tw = w(text);
                run += tw;
                run_started = true;
                if pending {
                    width += 1;
                    pending = false;
                }
                started = true;
                width += tw;
            }
        }
    }
    if run_started && run == 0 {
        None
    } else {
        Some(width)
    }
}

/// A fitted prefix: no wider than two thirds of the page, so content keeps some room.
fn fit(prefix: &str, width: usize) -> String {
    let cap = (width * 2 / 3).max(1);
    if w(prefix) <= cap {
        return prefix.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for g in prefix.graphemes(true) {
        if used + w(g) > cap {
            break;
        }
        out.push_str(g);
        used += w(g);
    }
    out
}

fn push_merged(spans: &mut Vec<Span>, text: String, style: Style, item: Option<usize>) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut() {
        if last.style == style && last.item == item {
            last.text.push_str(&text);
            return;
        }
    }
    spans.push(span(text, style, item));
}

/// A table's children split into captions and rows. `groups` are thead/tbody wrappers.
struct TableClass {
    lead: Vec<NodeId>,
    rows: Vec<NodeId>,
    trail: Vec<NodeId>,
    groups: Vec<NodeId>,
}

struct Builder<'a> {
    snap: &'a SemanticSnapshot,
    index: Index,
    done: Vec<bool>,
    width: usize,
    /// What starts every line of the block being laid out: indent, quote bars, list hang.
    prefix: String,
    lines: Vec<TextLine>,
    items: Vec<Item>,
    links: u32,
    depth: usize,
    /// The next block adds no blank line before it (a list item's first block).
    no_blank: bool,
    /// How many lists enclose the node being laid out.
    lists: usize,
    /// How the next line continues the last one: set by [`Self::wrap`] when it breaks a run.
    join: Joined,
}

impl<'a> Builder<'a> {
    /// Marks `id` visited and returns its node; `None` if unknown or seen already.
    fn take(&mut self, id: NodeId) -> Option<&'a SemanticNode> {
        let i = self.index.get(id)?;
        if self.done[i] {
            return None;
        }
        self.done[i] = true;
        Some(&self.snap.nodes[i])
    }

    fn room(&self) -> usize {
        self.width.saturating_sub(w(&self.prefix)).max(1)
    }

    fn blank_before(&mut self) {
        if self.no_blank {
            self.no_blank = false;
            return;
        }
        if self.lines.last().is_some_and(|l| !l.is_blank()) {
            self.lines.push(TextLine {
                spans: Vec::new(),
                node: None,
                joined: Joined::No,
                prefixed: false,
            });
        }
    }

    /// Appends a line (the prefix first) and records the columns of its items.
    fn push_line(&mut self, spans: Vec<Span>, node: Option<NodeId>) {
        self.no_blank = false;
        let line = self.lines.len();
        let mut all = Vec::with_capacity(spans.len() + 1);
        let mut col = w(&self.prefix);
        let prefixed = !self.prefix.is_empty();
        if prefixed {
            all.push(span(self.prefix.clone(), Style::default(), None));
        }
        for s in spans {
            let sw = w(&s.text);
            if let Some(i) = s.item {
                let (start, end) = (to_col(col), to_col(col + sw));
                let ranges = &mut self.items[i].ranges;
                match ranges.last_mut() {
                    Some(r) if r.line == line && r.end == start => r.end = end,
                    _ => ranges.push(CellRange { line, start, end }),
                }
            }
            col += sw;
            all.push(s);
        }
        let joined = std::mem::take(&mut self.join);
        self.lines.push(TextLine {
            spans: all,
            node,
            joined,
            prefixed,
        });
    }

    fn wrap(&mut self, tokens: Vec<Tok>, node: Option<NodeId>) {
        let room = self.room();
        let mut cur: Vec<Span> = Vec::new();
        let mut cur_w = 0usize;
        let mut gap: Option<(Style, Option<usize>)> = None;
        for unit in into_units(tokens) {
            let parts = match unit {
                Unit::Gap(s, i) => {
                    if cur_w > 0 {
                        gap = Some((s, i));
                    }
                    continue;
                }
                Unit::Run(parts) => parts,
            };
            let uw: usize = parts.iter().map(|p| w(&p.0)).sum();
            if uw == 0 {
                continue;
            }
            let gw = usize::from(gap.is_some());
            if cur_w > 0 && cur_w + gw + uw > room {
                self.push_line(std::mem::take(&mut cur), node);
                self.join = match gap {
                    Some(_) => Joined::Space,
                    None => Joined::Directly,
                };
                cur_w = 0;
                gap = None;
            }
            if cur_w == 0 && uw > room {
                // Wider than a whole line: break on graphemes.
                for (text, style, item) in &parts {
                    for g in text.graphemes(true) {
                        let gw = w(g);
                        if cur_w > 0 && cur_w + gw > room {
                            self.push_line(std::mem::take(&mut cur), node);
                            self.join = Joined::Directly;
                            cur_w = 0;
                        }
                        push_merged(&mut cur, g.to_string(), *style, *item);
                        cur_w += gw;
                    }
                }
                gap = None;
                continue;
            }
            if let Some((s, i)) = gap.take() {
                push_merged(&mut cur, " ".into(), s, i);
                cur_w += 1;
            }
            for (text, style, item) in parts {
                cur_w += w(&text);
                push_merged(&mut cur, text, style, item);
            }
        }
        if !cur.is_empty() {
            self.push_line(cur, node);
        }
        self.join = Joined::No;
    }

    /// One line, in order. The caller has checked that the tokens fit in [`Self::room`].
    fn push_fitted(&mut self, tokens: Vec<Tok>, node: Option<NodeId>) {
        let mut spans = Vec::new();
        let mut pending: Option<(Style, Option<usize>)> = None;
        let mut started = false;
        for tok in tokens {
            match tok {
                Tok::Space(style, item) => {
                    if started && pending.is_none() {
                        pending = Some((style, item));
                    }
                }
                Tok::Word(text, style, item) => {
                    if let Some((gs, gi)) = pending.take() {
                        push_merged(&mut spans, " ".into(), gs, gi);
                    }
                    started = true;
                    push_merged(&mut spans, text, style, item);
                }
            }
        }
        if !spans.is_empty() {
            self.push_line(spans, node);
        }
    }

    /// Flow `tokens` as a block, with a blank line before it when `blank`.
    fn flow_block(&mut self, tokens: Vec<Tok>, node: NodeId, blank: bool) {
        if !has_words(&tokens) {
            return;
        }
        if blank {
            self.blank_before();
        }
        // Most blocks fit on one line. Skip the run vector in that case.
        if flowed_width(&tokens).is_some_and(|width| width <= self.room()) {
            self.push_fitted(tokens, Some(node));
        } else {
            self.wrap(tokens, Some(node));
        }
    }

    // Blocks ----------------------------------------------------------------------------------

    fn block(&mut self, id: NodeId) {
        if self.depth > MAX_DEPTH {
            return;
        }
        let Some(node) = self.take(id) else { return };
        self.depth += 1;
        match node.kind {
            NodeKind::Heading(level) => self.heading(node, level),
            NodeKind::Paragraph => {
                let mut t = Vec::new();
                self.content(node, &mut t, Style::default());
                self.flow_block(t, node.id, true);
            }
            NodeKind::List => self.list(node),
            NodeKind::ListItem => self.list_item(node),
            NodeKind::Table => self.table(node),
            NodeKind::Pre => self.pre(node),
            NodeKind::Quote => self.quote(node),
            NodeKind::Separator => {
                self.blank_before();
                self.push_line(
                    vec![span(rule(self.room()), Style::default(), None)],
                    Some(node.id),
                );
            }
            NodeKind::TextArea => self.text_area(node),
            _ => self.container(node),
        }
        self.depth -= 1;
    }

    fn is_inline(&self, id: NodeId) -> bool {
        self.index.get(id).is_some_and(|i| {
            matches!(
                self.snap.nodes[i].kind,
                NodeKind::Text
                    | NodeKind::Link
                    | NodeKind::Code
                    | NodeKind::Image
                    | NodeKind::Button
                    | NodeKind::TextField
                    | NodeKind::Checkbox
                    | NodeKind::Radio
                    | NodeKind::Select
            )
        })
    }

    /// Own text, then children: runs of inline children flow together, blocks stand alone.
    fn container(&mut self, node: &'a SemanticNode) {
        let mut run: Vec<Tok> = Vec::new();
        if let Some(text) = &node.text {
            words(text, Style::default(), None, &mut run);
        }
        for &child in &node.children {
            if self.is_inline(child) {
                self.inline(child, &mut run, Style::default(), None);
            } else {
                self.flush(&mut run, node.id);
                self.block(child);
            }
        }
        self.flush(&mut run, node.id);
    }

    fn flush(&mut self, run: &mut Vec<Tok>, node: NodeId) {
        if !run.is_empty() {
            let t = std::mem::take(run);
            // Loose text between blocks (an image, a run in a container) is its own paragraph.
            self.flow_block(t, node, true);
        }
    }

    fn heading(&mut self, node: &'a SemanticNode, level: u8) {
        let style = Style {
            bold: true,
            underline: true,
            ..Style::default()
        };
        let mut t = Vec::new();
        self.content(node, &mut t, style);
        if !has_words(&t) {
            return;
        }
        self.flow_block(t, node.id, true);
        if level == 1 {
            self.push_line(
                vec![span(rule(self.room()), Style::default(), None)],
                Some(node.id),
            );
        }
    }

    fn list(&mut self, node: &'a SemanticNode) {
        if self.prefix.is_empty() {
            self.blank_before();
        }
        let mut run: Vec<Tok> = Vec::new();
        if let Some(text) = &node.text {
            words(text, Style::default(), None, &mut run);
            self.flush(&mut run, node.id);
        }
        self.lists += 1;
        for &child in &node.children {
            self.block(child);
        }
        self.lists -= 1;
    }

    fn list_item(&mut self, node: &'a SemanticNode) {
        let old = std::mem::take(&mut self.prefix);
        let bullet = match self.lists.saturating_sub(1) {
            0 => "• ",
            1 => "◦ ",
            _ => "▪ ",
        };
        let hang = fit(&format!("{old}{}", " ".repeat(w(bullet))), self.width);
        let marker = fit(&format!("{old}{bullet}"), self.width);
        self.prefix = hang.clone();
        let start = self.lines.len();
        self.no_blank = true;
        self.container(node);
        self.no_blank = false;
        if self.lines.len() == start {
            self.prefix = marker;
            self.push_line(Vec::new(), Some(node.id));
        } else if let Some(first) = self.lines[start].spans.first_mut() {
            if first.item.is_none() && first.text.starts_with(&hang) {
                first.text = format!("{marker}{}", &first.text[hang.len()..]);
            }
        }
        self.prefix = old;
    }

    fn quote(&mut self, node: &'a SemanticNode) {
        self.blank_before();
        let old = self.prefix.clone();
        self.prefix = fit(&format!("{old}│ "), self.width);
        let bar = self.prefix.trim_end().to_string();
        let start = self.lines.len();
        self.container(node);
        self.prefix = old;
        for line in &mut self.lines[start..] {
            if line.is_blank() {
                line.spans = vec![span(bar.clone(), Style::default(), None)];
            }
        }
        self.no_blank = false;
    }

    fn pre(&mut self, node: &'a SemanticNode) {
        let mut raw = String::new();
        self.raw(node, &mut raw, 0);
        let raw = raw.replace('\r', "");
        if raw.trim().is_empty() {
            return;
        }
        self.blank_before();
        let room = self.room();
        let lines: Vec<&str> = raw.trim_end_matches('\n').split('\n').collect();
        for line in lines {
            let clean = sanitize(&line.replace('\t', "    "));
            let (body, clipped) = clip(&clean, room);
            let mut spans = Vec::new();
            if clipped {
                let n = body.len() - '…'.len_utf8();
                spans.push(span(body[..n].to_string(), Style::default(), None));
                spans.push(span("…".into(), dim(), None));
            } else if !body.is_empty() {
                spans.push(span(body, Style::default(), None));
            }
            self.push_line(spans, Some(node.id));
        }
    }

    /// The text of `node` and its descendants, for preformatted content.
    fn raw(&mut self, node: &'a SemanticNode, out: &mut String, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        if let Some(t) = &node.text {
            out.push_str(t);
        }
        for &c in &node.children {
            if let Some(child) = self.take(c) {
                self.raw(child, out, depth + 1);
            }
        }
    }

    fn text_area(&mut self, node: &'a SemanticNode) {
        let stored = stored_value(node);
        // The brackets show a bounded rendering. `stored` is the whole value.
        let raw = if node.password {
            ""
        } else {
            node.value.as_deref().unwrap_or("")
        };
        let lines: Vec<String> = raw
            .replace('\r', "")
            .split('\n')
            .take(AREA_LINES)
            .map(|l| sanitize(&l.replace('\t', "    ")))
            .collect();
        let idx = self.control(node, ItemKind::TextArea, stored, Vec::new());
        let style = field(node.disabled);
        self.blank_before();
        let mut t = Vec::new();
        self.label(node, style, idx, &mut t);
        if has_words(&t) {
            self.wrap(t, Some(node.id));
        }
        let room = self.room();
        for l in &lines {
            let shown = if node.password {
                "********".to_string()
            } else {
                underscored(l)
            };
            let (shown, _) = clip(&format!("[{shown}]"), room);
            self.push_line(vec![span(shown, style, Some(idx))], Some(node.id));
        }
    }

    // Tables ----------------------------------------------------------------------------------

    /// Split `id` into caption blocks and rows. Row groups are descended into; a cycle stops.
    fn classify(
        &self,
        id: NodeId,
        class: &mut TableClass,
        seen: &mut HashSet<NodeId>,
        started: &mut bool,
        depth: usize,
    ) {
        if depth > MAX_DEPTH || !seen.insert(id) {
            return;
        }
        let Some(i) = self.index.get(id) else {
            return;
        };
        if self.done[i] {
            return;
        }
        let node = &self.snap.nodes[i];
        if node.kind == NodeKind::Row {
            *started = true;
            class.rows.push(id);
            return;
        }
        // thead, tbody and similar wrappers. Anything else (a caption paragraph, a nested
        // table) stays a block, before the grid until a row has been seen and after it then.
        if matches!(
            node.kind,
            NodeKind::Container | NodeKind::Landmark(_) | NodeKind::Document
        ) {
            class.groups.push(id);
            for &child in &node.children {
                self.classify(child, class, seen, started, depth + 1);
            }
            return;
        }
        if *started {
            class.trail.push(id);
        } else {
            class.lead.push(id);
        }
    }

    fn table(&mut self, node: &'a SemanticNode) {
        let caption_text = node.text.clone();
        let children = node.children.clone();
        let table_id = node.id;
        let mut class = TableClass {
            lead: Vec::new(),
            rows: Vec::new(),
            trail: Vec::new(),
            groups: Vec::new(),
        };
        let mut seen = HashSet::new();
        let mut started = false;
        for child in children {
            self.classify(child, &mut class, &mut seen, &mut started, 0);
        }
        let mut caption = Vec::new();
        if let Some(text) = &caption_text {
            words(text, Style::default(), None, &mut caption);
        }
        if class.rows.is_empty()
            && class.lead.is_empty()
            && class.trail.is_empty()
            && !has_words(&caption)
        {
            return;
        }
        // The wrappers were descended into. Marking them keeps `block` from laying them out again.
        for id in &class.groups {
            if let Some(i) = self.index.get(*id) {
                self.done[i] = true;
            }
        }
        self.blank_before();
        let had_caption = has_words(&caption);
        if had_caption {
            self.wrap(caption, Some(table_id));
        }
        let had_lead = !class.lead.is_empty();
        for id in class.lead {
            self.block(id);
        }
        // Captions are laid out first so their links take the earlier footnote numbers.
        let mut layouts = Vec::with_capacity(class.rows.len());
        for id in class.rows {
            let Some(row) = self.take(id) else { continue };
            let rid = row.id;
            let text = row.text.clone();
            let kids = row.children.clone();
            layouts.push((rid, self.row_cells(text.as_deref(), &kids)));
        }
        if !layouts.is_empty() {
            if had_caption || had_lead {
                self.blank_before();
            }
            self.emit_rows(layouts);
        }
        for id in class.trail {
            self.block(id);
        }
    }

    fn row_cells(&mut self, text: Option<&str>, children: &[NodeId]) -> Vec<Vec<Tok>> {
        let mut cells = Vec::new();
        if let Some(text) = text {
            let mut own = Vec::new();
            words(text, Style::default(), None, &mut own);
            if has_words(&own) {
                cells.push(own);
            }
        }
        for &child in children {
            let Some(cell) = self.take(child) else {
                continue;
            };
            let mut tokens = Vec::new();
            if cell.kind == NodeKind::Cell {
                self.content(cell, &mut tokens, Style::default());
            } else {
                self.inline_node(cell, &mut tokens, Style::default(), None);
            }
            cells.push(tokens);
        }
        cells
    }

    fn emit_rows(&mut self, rows: Vec<(NodeId, Vec<Vec<Tok>>)>) {
        let cols = rows.iter().map(|row| row.1.len()).max().unwrap_or(0);
        if cols == 0 {
            return;
        }
        let mut widths = vec![0usize; cols];
        for (_, cells) in &rows {
            for (i, cell) in cells.iter().enumerate() {
                widths[i] = widths[i].max(tokens_width(cell));
            }
        }
        let total = widths.iter().sum::<usize>() + 3 * cols.saturating_sub(1);
        if total <= self.room() {
            for (id, cells) in &rows {
                self.emit_aligned_row(*id, cells, &widths);
            }
        } else {
            for (id, cells) in rows {
                self.emit_stacked_row(id, cells);
            }
        }
    }

    fn emit_aligned_row(&mut self, id: NodeId, cells: &[Vec<Tok>], widths: &[usize]) {
        let mut spans = Vec::new();
        for (i, width) in widths.iter().enumerate() {
            if i > 0 {
                spans.push(span(" | ".into(), Style::default(), None));
            }
            let mut used = 0;
            if let Some(cell) = cells.get(i) {
                let mut pending: Option<(Style, Option<usize>)> = None;
                let mut started = false;
                for tok in cell {
                    match tok {
                        Tok::Space(style, item) => {
                            if started {
                                pending = Some((*style, *item));
                            }
                        }
                        Tok::Word(text, style, item) => {
                            if let Some((gs, gi)) = pending.take() {
                                spans.push(span(" ".into(), gs, gi));
                                used += 1;
                            }
                            started = true;
                            used += w(text);
                            spans.push(span(text.clone(), *style, *item));
                        }
                    }
                }
            }
            if i + 1 < widths.len() && used < *width {
                spans.push(span(" ".repeat(width - used), Style::default(), None));
            }
        }
        self.push_line(spans, Some(id));
    }

    fn emit_stacked_row(&mut self, id: NodeId, cells: Vec<Vec<Tok>>) {
        let mut tokens = Vec::new();
        for cell in cells {
            if !has_words(&cell) {
                continue;
            }
            if !tokens.is_empty() {
                tokens.push(Tok::Space(Style::default(), None));
                tokens.push(Tok::Word("/".into(), Style::default(), None));
                tokens.push(Tok::Space(Style::default(), None));
            }
            tokens.extend(cell);
        }
        self.flow_block(tokens, id, false);
    }

    // Inline ----------------------------------------------------------------------------------

    /// Own text then children, as inline tokens.
    fn content(&mut self, node: &'a SemanticNode, out: &mut Vec<Tok>, style: Style) {
        if let Some(t) = &node.text {
            words(t, style, None, out);
        }
        for &c in &node.children {
            self.inline(c, out, style, None);
        }
    }

    fn inline(&mut self, id: NodeId, out: &mut Vec<Tok>, style: Style, item: Option<usize>) {
        if self.depth > MAX_DEPTH {
            return;
        }
        let Some(node) = self.take(id) else { return };
        self.depth += 1;
        self.inline_node(node, out, style, item);
        self.depth -= 1;
    }

    /// A control stands apart from the words around it.
    fn inline_node(
        &mut self,
        node: &'a SemanticNode,
        out: &mut Vec<Tok>,
        style: Style,
        item: Option<usize>,
    ) {
        let control = matches!(
            node.kind,
            NodeKind::Button
                | NodeKind::TextField
                | NodeKind::TextArea
                | NodeKind::Checkbox
                | NodeKind::Radio
                | NodeKind::Select
        );
        if control && !out.is_empty() {
            out.push(Tok::Space(style, None));
        }
        self.inline_node_inner(node, out, style, item);
        if control {
            out.push(Tok::Space(style, None));
        }
    }

    fn inline_node_inner(
        &mut self,
        node: &'a SemanticNode,
        out: &mut Vec<Tok>,
        style: Style,
        item: Option<usize>,
    ) {
        match node.kind {
            NodeKind::Link => self.link(node, out, style, item),
            NodeKind::Image => {
                let alt = node.text.as_deref().map(collapse).unwrap_or_default();
                let text = if alt.is_empty() {
                    "[image]".to_string()
                } else {
                    format!("[image: {alt}]")
                };
                out.push(Tok::Word(text, Style { dim: true, ..style }, item));
            }
            NodeKind::Button => {
                let mut caption = node.text.as_deref().map(collapse).unwrap_or_default();
                if caption.is_empty() {
                    caption = node.label.as_deref().map(collapse).unwrap_or_default();
                }
                if caption.is_empty() {
                    let mut t = Vec::new();
                    for &c in &node.children {
                        self.inline(c, &mut t, Style::default(), None);
                    }
                    caption = plain(&t);
                }
                let idx = self.control(node, ItemKind::Button, String::new(), Vec::new());
                out.push(Tok::Word(
                    format!("[ {caption} ]"),
                    field(node.disabled),
                    Some(idx),
                ));
            }
            NodeKind::TextField => {
                let stored = stored_value(node);
                let shown = if node.password {
                    "********".to_string()
                } else {
                    underscored(&collapse(node.value.as_deref().unwrap_or("")))
                };
                let idx = self.control(node, ItemKind::TextField, stored, Vec::new());
                let st = field(node.disabled);
                self.label(node, st, idx, out);
                out.push(Tok::Word(format!("[{shown}]"), st, Some(idx)));
            }
            NodeKind::TextArea => {
                let stored = stored_value(node);
                let shown = if node.password {
                    "********".into()
                } else {
                    underscored(&collapse(node.value.as_deref().unwrap_or("")))
                };
                let idx = self.control(node, ItemKind::TextArea, stored, Vec::new());
                let st = field(node.disabled);
                self.label(node, st, idx, out);
                out.push(Tok::Word(format!("[{shown}]"), st, Some(idx)));
            }
            NodeKind::Checkbox | NodeKind::Radio => {
                let check = node.kind == NodeKind::Checkbox;
                let on = node.checked == Some(true);
                let mark = match (check, on) {
                    (true, true) => "[x]",
                    (true, false) => "[ ]",
                    (false, true) => "(o)",
                    (false, false) => "( )",
                };
                let kind = if check {
                    ItemKind::Checkbox
                } else {
                    ItemKind::Radio
                };
                let idx = self.control(node, kind, String::new(), Vec::new());
                let st = field(node.disabled);
                out.push(Tok::Word(mark.into(), st, Some(idx)));
                let mut t = Vec::new();
                self.label(node, st, idx, &mut t);
                if t.is_empty() {
                    if let Some(text) = &node.text {
                        out.push(Tok::Space(st, Some(idx)));
                        words(text, st, Some(idx), out);
                    }
                } else {
                    out.push(Tok::Space(st, Some(idx)));
                    out.extend(t);
                    // `label` ends with a space; the word that follows is the next token.
                    if matches!(out.last(), Some(Tok::Space(..))) {
                        out.pop();
                    }
                }
            }
            NodeKind::Select => {
                let options: Vec<String> = node.options.iter().map(|o| collapse(o)).collect();
                // Nothing selected stays blank. The first option is not implied.
                let shown = node
                    .selected
                    .and_then(|s| options.get(s as usize))
                    .cloned()
                    .unwrap_or_default();
                let (shown, _) = clip(&shown, FIELD_MAX);
                let idx = self.control(node, ItemKind::Select, String::new(), options);
                self.items[idx].selected = node.selected;
                let st = field(node.disabled);
                self.label(node, st, idx, out);
                out.push(Tok::Word(format!("[{shown} ▾]"), st, Some(idx)));
            }
            NodeKind::Code => {
                if let Some(t) = &node.text {
                    words(t, Style { dim: true, ..style }, item, out);
                }
                for &c in &node.children {
                    self.inline(c, out, style, item);
                }
            }
            NodeKind::Text => {
                if let Some(t) = &node.text {
                    words(t, style, item, out);
                }
            }
            NodeKind::Container | NodeKind::Landmark(_) | NodeKind::Document => {
                self.content_item(node, out, style, item);
            }
            _ => {
                // A block inside inline content flattens, set apart by spaces.
                let mark = out.len();
                self.content_item(node, out, style, item);
                if mark > 0 && out.len() > mark {
                    out.insert(mark, Tok::Space(style, item));
                }
                if out.len() > mark {
                    out.push(Tok::Space(style, item));
                }
            }
        }
    }

    fn content_item(
        &mut self,
        node: &'a SemanticNode,
        out: &mut Vec<Tok>,
        style: Style,
        item: Option<usize>,
    ) {
        if let Some(t) = &node.text {
            words(t, style, item, out);
        }
        for &c in &node.children {
            self.inline(c, out, style, item);
        }
    }

    fn link(
        &mut self,
        node: &'a SemanticNode,
        out: &mut Vec<Tok>,
        style: Style,
        outer: Option<usize>,
    ) {
        let href = node
            .href
            .as_deref()
            .map(page_string)
            .filter(|h| !h.is_empty());
        let Some(href) = href else {
            self.content_item(node, out, style, outer);
            return;
        };
        self.links = self.links.saturating_add(1);
        let number = self.links;
        let idx = self.items.len();
        self.items.push(Item {
            node: node.id,
            kind: ItemKind::Link,
            ranges: Vec::new(),
            href: Some(href),
            value: String::new(),
            options: Vec::new(),
            selected: None,
            disabled: node.disabled,
            password: false,
            number: Some(number),
        });
        let st = Style {
            link: true,
            underline: true,
            dim: node.disabled,
            ..style
        };
        let mut inner = Vec::new();
        self.content_item(node, &mut inner, st, Some(idx));
        // Leading and trailing spaces belong outside the link, so the marker stays on the text.
        while matches!(inner.first(), Some(Tok::Space(..))) {
            inner.remove(0);
        }
        while matches!(inner.last(), Some(Tok::Space(..))) {
            inner.pop();
        }
        out.extend(inner);
        out.push(Tok::Word(format!("[{number}]"), st, Some(idx)));
    }

    /// The control's label as tokens, followed by a space; nothing without one.
    fn label(&self, node: &SemanticNode, style: Style, idx: usize, out: &mut Vec<Tok>) {
        if let Some(l) = node.label.as_deref() {
            let start = out.len();
            words(&collapse(l), style, Some(idx), out);
            if out.len() > start {
                out.push(Tok::Space(style, Some(idx)));
            }
        }
    }

    fn control(
        &mut self,
        node: &SemanticNode,
        kind: ItemKind,
        value: String,
        options: Vec<String>,
    ) -> usize {
        self.items.push(Item {
            node: node.id,
            kind,
            ranges: Vec::new(),
            href: None,
            value,
            options,
            selected: None,
            disabled: node.disabled,
            password: node.password,
            number: None,
        });
        self.items.len() - 1
    }
}

fn plain(tokens: &[Tok]) -> String {
    let mut out = String::new();
    let mut pending = false;
    let mut started = false;
    for tok in tokens {
        match tok {
            Tok::Space(..) => {
                if started {
                    pending = true;
                }
            }
            Tok::Word(text, ..) => {
                if pending {
                    out.push(' ');
                    pending = false;
                }
                started = true;
                out.push_str(text);
            }
        }
    }
    out
}

/// `sanitize`, or a copy when the string is already printable ASCII.
fn page_string(text: &str) -> String {
    if text.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        text.to_string()
    } else {
        sanitize(text)
    }
}

/// A field's brackets' inside: the value, padded with underscores, cut when long.
fn underscored(value: &str) -> String {
    let (v, _) = clip(value, FIELD_MAX);
    let pad = FIELD_MIN.saturating_sub(w(&v));
    format!("{v}{}", "_".repeat(pad))
}

fn to_col(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}
