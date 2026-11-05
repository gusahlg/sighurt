//! Text layout and drawing for pages that read like documents: a [`Builder`] lays headings,
//! word-wrapped paragraphs, lists, code and tables out as [`Op`]s at absolute positions, and
//! [`draw`] paints the ones in view with a [`Role`]'s colour for the theme.
//!
//! The layout is pure: text is measured through a closure, so it runs (and is tested) on the
//! host. Only [`draw`] and [`measure`] call the canvas.
//!
//! Must not own: what any one page says, scrolling or input.

use sighurt_sdk as sdk;

/// A font, as the canvas takes one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Font {
    /// Size in pixels.
    pub size: f32,
    /// CSS weight (400 normal, 700 bold).
    pub weight: u32,
    /// The monospace family instead of the UI font.
    pub mono: bool,
    /// Italic style.
    pub italic: bool,
}

impl Font {
    /// The UI font at `size`, normal weight.
    pub const fn new(size: f32) -> Self {
        Self {
            size,
            weight: 400,
            mono: false,
            italic: false,
        }
    }
    /// The same font, bold.
    pub const fn bold(self) -> Self {
        Self {
            weight: 700,
            ..self
        }
    }
    /// The same size, monospace.
    pub const fn mono(self) -> Self {
        Self { mono: true, ..self }
    }
    /// The canvas's family name; empty for the UI font.
    pub fn family(&self) -> &'static str {
        if self.mono {
            "monospace"
        } else {
            ""
        }
    }
    /// The distance between lines: 1.5 times the size.
    pub fn line_height(&self) -> f32 {
        (self.size * 1.5).round()
    }
    fn style(&self) -> u32 {
        if self.italic {
            sdk::FONT_STYLE_ITALIC
        } else {
            sdk::FONT_STYLE_NORMAL
        }
    }
}

/// What a thing is for; [`color`] gives it a colour in a theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The page behind everything.
    Bg,
    /// Body text.
    Text,
    /// Headings.
    Heading,
    /// Secondary text.
    Muted,
    /// Links.
    Accent,
    /// Behind code and key names.
    CodeBg,
    /// Every second table row.
    RowAlt,
    /// Rules and borders.
    Rule,
    /// The sidebar or header.
    Panel,
    /// The current item of a list of links.
    Selected,
    /// Text on [`Role::Selected`].
    SelectedText,
}

/// The colour of `role` in the light or dark theme.
pub fn color(role: Role, dark: bool) -> [u8; 3] {
    use Role::*;
    match (role, dark) {
        (Bg, false) => [252, 252, 250],
        (Text, false) => [30, 32, 36],
        (Heading, false) => [12, 14, 18],
        (Muted, false) => [96, 102, 112],
        (Accent, false) => [0, 88, 178],
        (CodeBg, false) => [236, 238, 242],
        (RowAlt, false) => [244, 245, 247],
        (Rule, false) => [212, 215, 220],
        (Panel, false) => [241, 242, 244],
        (Selected, false) => [214, 228, 248],
        (SelectedText, false) => [8, 56, 128],
        (Bg, true) => [24, 26, 30],
        (Text, true) => [220, 223, 228],
        (Heading, true) => [246, 248, 251],
        (Muted, true) => [150, 156, 166],
        (Accent, true) => [124, 182, 255],
        (CodeBg, true) => [36, 39, 46],
        (RowAlt, true) => [30, 33, 38],
        (Rule, true) => [58, 62, 70],
        (Panel, true) => [30, 32, 37],
        (Selected, true) => [44, 62, 92],
        (SelectedText, true) => [176, 208, 255],
    }
}

/// One thing to draw, at absolute positions in the page's own coordinates.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// A line of text whose top is `y` and whose height is `h`.
    Text {
        x: f32,
        y: f32,
        h: f32,
        font: Font,
        role: Role,
        text: String,
    },
    /// A filled rectangle, rounded when `radius` is not 0.
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        role: Role,
    },
    /// A clickable region that opens `url`.
    Link {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        url: String,
    },
}

impl Op {
    /// The vertical extent, top and bottom.
    pub fn span(&self) -> (f32, f32) {
        match self {
            Op::Text { y, h, .. } | Op::Rect { y, h, .. } | Op::Link { y, h, .. } => (*y, y + h),
        }
    }
}

/// The width of `text` in `font`, from the canvas.
pub fn measure(font: &Font, text: &str) -> f32 {
    sdk::canvas_measure_text(font.size, font.family(), font.weight, font.style(), text).width
}

/// Draws a text or rectangle op moved up by `scroll`. Links are the caller's to register.
pub fn draw(op: &Op, scroll: f32, dark: bool) {
    match op {
        Op::Text {
            x,
            y,
            font,
            role,
            text,
            ..
        } => {
            let [r, g, b] = color(*role, dark);
            sdk::canvas_text_ex(
                *x,
                y - scroll,
                font.size,
                r,
                g,
                b,
                255,
                font.family(),
                font.weight,
                font.style(),
                sdk::TEXT_ALIGN_LEFT,
                text,
            );
        }
        Op::Rect {
            x,
            y,
            w,
            h,
            radius,
            role,
        } => {
            let [r, g, b] = color(*role, dark);
            if *radius > 0.0 {
                sdk::canvas_rounded_rect(*x, y - scroll, *w, *h, *radius, r, g, b, 255);
            } else {
                sdk::canvas_rect(*x, y - scroll, *w, *h, r, g, b, 255);
            }
        }
        Op::Link { .. } => {}
    }
}

/// Splits `text` into lines no wider than `max`, breaking at spaces (a word wider than `max`
/// is broken between characters). `width` measures a piece of text. A `\n` starts a new line.
pub fn wrap(text: &str, max: f32, mut width: impl FnMut(&str) -> f32) -> Vec<String> {
    let space = width(" ");
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut line_w = 0.0;
        for word in paragraph.split_whitespace() {
            let word_w = width(word);
            if word_w > max {
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                }
                let mut piece_w = 0.0;
                for ch in word.chars() {
                    let ch_w = width(ch.encode_utf8(&mut [0; 4]));
                    if piece_w + ch_w > max && !line.is_empty() {
                        out.push(std::mem::take(&mut line));
                        piece_w = 0.0;
                    }
                    line.push(ch);
                    piece_w += ch_w;
                }
                line_w = piece_w;
            } else if line.is_empty() {
                line.push_str(word);
                line_w = word_w;
            } else if line_w + space + word_w <= max {
                line.push(' ');
                line.push_str(word);
                line_w += space + word_w;
            } else {
                out.push(std::mem::take(&mut line));
                line.push_str(word);
                line_w = word_w;
            }
        }
        out.push(line);
    }
    out
}

/// Parses an info reply: one record per non-empty line, fields split at tabs. Records with
/// fewer than `min_fields` fields are skipped; extra fields are kept for the caller to ignore.
pub fn records(text: &str, min_fields: usize) -> Vec<Vec<String>> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.split('\t').map(str::to_string).collect::<Vec<_>>())
        .filter(|fields| fields.len() >= min_fields)
        .collect()
}

/// A table column: its header, its share of the table's width, and whether it is code.
#[derive(Clone, Copy, Debug)]
pub struct Column<'a> {
    /// The header text.
    pub title: &'a str,
    /// The share of the width (the shares should add up to 1).
    pub frac: f32,
    /// Monospace cells.
    pub mono: bool,
}

/// Body text size.
pub const BODY: f32 = 16.0;

/// Lays a document out down a column starting at `x`, `width` wide.
pub struct Builder<M> {
    ops: Vec<Op>,
    x: f32,
    width: f32,
    y: f32,
    measure: M,
}

impl<M: FnMut(&Font, &str) -> f32> Builder<M> {
    /// A column at `x` of `width`, starting at `y`; `measure` gives a text's width.
    pub fn new(x: f32, width: f32, y: f32, measure: M) -> Self {
        Self {
            ops: Vec::new(),
            x,
            width,
            y,
            measure,
        }
    }

    /// The ops and the height of the content (the next free `y`).
    pub fn finish(self) -> (Vec<Op>, f32) {
        (self.ops, self.y)
    }

    /// Empty space.
    pub fn gap(&mut self, h: f32) {
        self.y += h;
    }

    fn lines(&mut self, x: f32, width: f32, font: Font, role: Role, text: &str) -> usize {
        let lines = wrap(text, width, |s| (self.measure)(&font, s));
        let h = font.line_height();
        for line in &lines {
            if !line.is_empty() {
                self.ops.push(Op::Text {
                    x,
                    y: self.y,
                    h,
                    font,
                    role,
                    text: line.clone(),
                });
            }
            self.y += h;
        }
        lines.len()
    }

    /// A heading: level 1 is the page title, 2 a section, 3 a subsection.
    pub fn heading(&mut self, level: u8, text: &str) {
        let size = match level {
            1 => 32.0,
            2 => 22.0,
            _ => 17.0,
        };
        self.gap(if level == 1 { 0.0 } else { size });
        let (x, w) = (self.x, self.width);
        self.lines(x, w, Font::new(size).bold(), Role::Heading, text);
        self.gap(if level == 1 { 12.0 } else { 6.0 });
    }

    /// A paragraph of body text.
    pub fn para(&mut self, text: &str) {
        self.styled(Role::Text, text);
    }

    /// A paragraph of secondary text.
    pub fn muted(&mut self, text: &str) {
        self.styled(Role::Muted, text);
    }

    fn styled(&mut self, role: Role, text: &str) {
        let (x, w) = (self.x, self.width);
        self.lines(x, w, Font::new(BODY), role, text);
        self.gap(10.0);
    }

    /// A bulleted item.
    pub fn bullet(&mut self, text: &str) {
        let (x, w) = (self.x, self.width);
        let font = Font::new(BODY);
        self.ops.push(Op::Text {
            x: x + 4.0,
            y: self.y,
            h: font.line_height(),
            font,
            role: Role::Muted,
            text: "\u{2022}".into(),
        });
        self.lines(x + 22.0, w - 22.0, font, Role::Text, text);
        self.gap(4.0);
    }

    /// A block of code on a shaded panel; lines are kept as written and wrapped when too wide.
    pub fn code(&mut self, text: &str) {
        let font = Font::new(14.0).mono();
        let (x, w) = (self.x, self.width);
        let start = self.ops.len();
        self.ops.push(Op::Rect {
            x,
            y: self.y,
            w,
            h: 0.0,
            radius: 6.0,
            role: Role::CodeBg,
        });
        self.gap(10.0);
        for line in text.lines() {
            self.lines(x + 14.0, w - 28.0, font, Role::Text, line);
        }
        self.gap(10.0);
        let h = self.y - self.ops_rect_top(start);
        if let Op::Rect { h: rect_h, .. } = &mut self.ops[start] {
            *rect_h = h;
        }
        self.gap(10.0);
    }

    fn ops_rect_top(&self, index: usize) -> f32 {
        self.ops[index].span().0
    }

    /// A link on its own line: coloured, underlined, and clickable.
    pub fn link(&mut self, text: &str, url: &str) {
        let font = Font::new(BODY);
        let w = (self.measure)(&font, text);
        let h = font.line_height();
        let x = self.x;
        self.ops.push(Op::Text {
            x,
            y: self.y,
            h,
            font,
            role: Role::Accent,
            text: text.into(),
        });
        self.ops.push(Op::Rect {
            x,
            y: self.y + h - 5.0,
            w,
            h: 1.0,
            radius: 0.0,
            role: Role::Accent,
        });
        self.ops.push(Op::Link {
            x,
            y: self.y,
            w,
            h,
            url: url.into(),
        });
        self.gap(h + 4.0);
    }

    /// A table: a header, then a row for each of `rows` (a missing cell is empty), cells
    /// wrapped to their column, every second row shaded.
    pub fn table(&mut self, cols: &[Column], rows: &[Vec<String>]) {
        const PAD_X: f32 = 8.0;
        const PAD_Y: f32 = 6.0;
        let font = Font::new(14.0);
        let lh = font.line_height();
        let mut xs = Vec::with_capacity(cols.len());
        let mut at = self.x;
        for col in cols {
            xs.push((at, col.frac * self.width));
            at += col.frac * self.width;
        }
        for (col, &(x, _)) in cols.iter().zip(&xs) {
            let (x, y) = (x + PAD_X, self.y + PAD_Y);
            self.ops.push(Op::Text {
                x,
                y,
                h: lh,
                font: font.bold(),
                role: Role::Muted,
                text: col.title.into(),
            });
        }
        self.y += lh + 2.0 * PAD_Y;
        let w = self.width;
        self.ops.push(Op::Rect {
            x: self.x,
            y: self.y,
            w,
            h: 1.0,
            radius: 0.0,
            role: Role::Rule,
        });
        self.y += 1.0;
        for (i, row) in rows.iter().enumerate() {
            let wrapped: Vec<Vec<String>> = cols
                .iter()
                .zip(&xs)
                .enumerate()
                .map(|(c, (col, &(_, cw)))| {
                    let cell_font = if col.mono { font.mono() } else { font };
                    let text = row.get(c).map_or("", String::as_str);
                    wrap(text, cw - 2.0 * PAD_X, |s| (self.measure)(&cell_font, s))
                })
                .collect();
            let n = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
            let h = n as f32 * lh + 2.0 * PAD_Y;
            if i % 2 == 1 {
                self.ops.push(Op::Rect {
                    x: self.x,
                    y: self.y,
                    w,
                    h,
                    radius: 0.0,
                    role: Role::RowAlt,
                });
            }
            for ((col, &(x, _)), lines) in cols.iter().zip(&xs).zip(&wrapped) {
                let cell_font = if col.mono { font.mono() } else { font };
                for (k, line) in lines.iter().enumerate().filter(|(_, l)| !l.is_empty()) {
                    let y = self.y + PAD_Y + k as f32 * lh;
                    self.ops.push(Op::Text {
                        x: x + PAD_X,
                        y,
                        h: lh,
                        font: cell_font,
                        role: Role::Text,
                        text: line.clone(),
                    });
                }
            }
            self.y += h;
        }
        self.gap(14.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> f32 {
        s.chars().count() as f32
    }

    #[test]
    fn wraps_at_spaces() {
        assert_eq!(wrap("aaa bbb ccc", 7.0, chars), ["aaa bbb", "ccc"]);
        assert_eq!(wrap("aaa bbb ccc", 11.0, chars), ["aaa bbb ccc"]);
        assert_eq!(wrap("", 5.0, chars), [""]);
        assert_eq!(wrap("a\n\nb", 5.0, chars), ["a", "", "b"]);
    }

    #[test]
    fn breaks_long_words() {
        assert_eq!(wrap("abcdefgh x", 3.0, chars), ["abc", "def", "gh", "x"]);
    }

    #[test]
    fn parses_records() {
        let rows = records("a\tb\tc\n\nbad\nd\te\n", 2);
        assert_eq!(rows, vec![vec!["a", "b", "c"], vec!["d", "e"]]);
    }

    #[test]
    fn table_rows_grow_with_wrapped_cells() {
        let cols = [
            Column {
                title: "k",
                frac: 0.5,
                mono: false,
            },
            Column {
                title: "v",
                frac: 0.5,
                mono: false,
            },
        ];
        let rows = vec![vec!["a".to_string(), "word ".repeat(30)]];
        let mut b = Builder::new(0.0, 200.0, 0.0, |_: &Font, s: &str| chars(s) * 6.0);
        b.table(&cols, &rows);
        let (ops, height) = b.finish();
        assert!(height > 100.0);
        assert!(ops.iter().all(|op| op.span().1 <= height));
    }
}
