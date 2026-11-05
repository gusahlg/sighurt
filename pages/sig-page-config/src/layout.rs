//! Where text goes across a line: the advance of each grapheme cluster, measured once and
//! cached, and the conversions between a column and an x offset.
//!
//! The font is monospace when one is installed, but nothing assumes it: every cluster is
//! measured by the host the first time it is seen (an ASCII character in a table, anything
//! else in a bounded map), so a wide character or a fallback font still lines up with what
//! is drawn. A tab goes to the next multiple of four spaces.
//!
//! Must not own: what the text is (the buffer), or drawing.

use std::collections::HashMap;

use unicode_segmentation::UnicodeSegmentation;

/// Measures the advance of one grapheme cluster, in pixels. The host's text measurement on
/// the page; a fixed table in tests.
pub trait Measure {
    fn advance(&mut self, cluster: &str) -> f32;
}

/// Most clusters beyond ASCII remembered; the map is emptied when it is full.
const MAX_CACHED: usize = 4096;
/// Columns between tab stops.
const TAB: f32 = 4.0;

/// Cached advances.
pub struct Widths {
    measure: Box<dyn Measure>,
    ascii: [Option<f32>; 128],
    other: HashMap<String, f32>,
}

impl Widths {
    pub fn new(measure: Box<dyn Measure>) -> Self {
        Self {
            measure,
            ascii: [None; 128],
            other: HashMap::new(),
        }
    }

    /// Forgets every advance, for a new font.
    pub fn reset(&mut self, measure: Box<dyn Measure>) {
        *self = Self::new(measure);
    }

    fn ascii(&mut self, byte: u8) -> f32 {
        let slot = &mut self.ascii[usize::from(byte & 0x7f)];
        if let Some(width) = *slot {
            return width;
        }
        let width = self
            .measure
            .advance(std::str::from_utf8(&[byte]).unwrap_or(" "))
            .max(0.0);
        *slot = Some(width);
        width
    }

    /// The width of a space: the unit of tab stops and of the caret's width.
    pub fn space(&mut self) -> f32 {
        self.ascii(b' ')
    }

    fn cluster(&mut self, cluster: &str) -> f32 {
        if let [byte] = cluster.as_bytes() {
            return self.ascii(*byte);
        }
        if let Some(&width) = self.other.get(cluster) {
            return width;
        }
        if self.other.len() >= MAX_CACHED {
            self.other.clear();
        }
        let width = self.measure.advance(cluster).max(0.0);
        self.other.insert(cluster.to_owned(), width);
        width
    }

    /// The x after advancing from `x` over `cluster`.
    fn step(&mut self, x: f32, cluster: &str) -> f32 {
        if cluster == "\t" {
            let stop = self.space() * TAB;
            if stop <= 0.0 {
                return x;
            }
            return ((x / stop).floor() + 1.0) * stop;
        }
        x + self.cluster(cluster)
    }

    /// The x offset of byte `col` in `line`.
    pub fn x_of(&mut self, line: &str, col: usize) -> f32 {
        self.advance(0.0, &line[..col.min(line.len())])
    }

    /// The x after `text` drawn from `x`, where `x` is a cluster boundary's offset in its
    /// line (tab stops are counted from the line's start).
    pub fn advance(&mut self, x: f32, head: &str) -> f32 {
        let mut x = x;
        if head.is_ascii() {
            for byte in head.bytes() {
                x = if byte == b'\t' {
                    self.step(x, "\t")
                } else {
                    x + self.ascii(byte)
                };
            }
            return x;
        }
        for cluster in head.graphemes(true) {
            x = self.step(x, cluster);
        }
        x
    }

    /// The cluster boundary in `line` nearest to `x`.
    pub fn col_at(&mut self, line: &str, x: f32) -> usize {
        let mut left = 0.0;
        for (col, cluster) in line.grapheme_indices(true) {
            let right = self.step(left, cluster);
            if x < (left + right) / 2.0 {
                return col;
            }
            left = right;
        }
        line.len()
    }

    /// The bytes of `line` that show between `x0` and `x1`, by whole clusters, with the x
    /// of the first: what is drawn of a line scrolled sideways.
    pub fn visible(&mut self, line: &str, x0: f32, x1: f32) -> (usize, usize, f32) {
        let mut x = 0.0;
        let mut start = None;
        for (col, cluster) in line.grapheme_indices(true) {
            let next = self.step(x, cluster);
            if start.is_none() && next > x0 {
                start = Some((col, x));
            }
            if x >= x1 {
                let (from, at) = start.unwrap_or((col, x));
                return (from, col, at);
            }
            x = next;
        }
        let (from, at) = start.unwrap_or((line.len(), x));
        (from, line.len(), at)
    }
}

#[cfg(test)]
pub mod testing {
    /// Every cluster is 10 pixels wide, except CJK, which is 20.
    pub struct Cells;

    impl super::Measure for Cells {
        fn advance(&mut self, cluster: &str) -> f32 {
            if cluster
                .chars()
                .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
            {
                20.0
            } else {
                10.0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widths() -> Widths {
        Widths::new(Box::new(testing::Cells))
    }

    #[test]
    fn columns_and_offsets_agree_with_wide_clusters_and_tabs() {
        let mut w = widths();
        let line = "a中e\u{301}\tb";
        assert_eq!(w.x_of(line, 0), 0.0);
        assert_eq!(w.x_of(line, 1), 10.0);
        assert_eq!(w.x_of(line, 4), 30.0);
        assert_eq!(w.x_of(line, 7), 40.0, "e and its accent are one cluster");
        assert_eq!(
            w.x_of(line, 8),
            80.0,
            "a tab at a stop goes to the next one"
        );
        assert_eq!(w.x_of(line, 9), 90.0);
        assert_eq!(w.col_at(line, 0.0), 0);
        assert_eq!(w.col_at(line, 14.0), 1);
        assert_eq!(w.col_at(line, 21.0), 4, "past the middle of the wide one");
        assert_eq!(w.col_at(line, 33.0), 4);
        assert_eq!(w.col_at(line, 36.0), 7, "never inside the cluster");
        assert_eq!(w.col_at(line, 999.0), line.len());
        assert_eq!(w.x_of("\tx", 1), 40.0);
        assert_eq!(w.x_of("ab\tx", 3), 40.0);
        assert_eq!(w.x_of("abcd\tx", 5), 80.0);
    }

    #[test]
    fn the_visible_part_of_a_long_line() {
        let mut w = widths();
        let line = "0123456789".repeat(10);
        assert_eq!(w.visible(&line, 0.0, 50.0), (0, 5, 0.0));
        assert_eq!(w.visible(&line, 25.0, 55.0), (2, 6, 20.0));
        assert_eq!(w.visible(&line, 2000.0, 2100.0), (100, 100, 1000.0));
        assert_eq!(w.visible("", 0.0, 10.0), (0, 0, 0.0));
    }
}
