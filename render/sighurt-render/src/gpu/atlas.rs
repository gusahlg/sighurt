//! The atlas: where a GPU backend's glyph and icon images go in its two textures, planned on
//! the CPU. The backend only creates the textures at the pages' sizes and copies the uploads
//! [`crate::gpu::prepare`] hands it.
//!
//! Two pages, square: **mask** (R8: glyph coverage, and the icons) and **color** (RGBA8,
//! premultiplied: color glyphs, emoji). Each starts at [`INITIAL_SIZE`]² and doubles, up to
//! [`MAX_SIZE`]² (R8 4 MiB + RGBA8 16 MiB), when a frame's images don't fit; at that bound a
//! full page starts over at its size instead. Growing a page also starts it over (the texture
//! is new). A frame that still doesn't fit is [`AtlasFull`]: bounded, and reported to the
//! backend, which gives up on the GPU (RENDER.md §14.10).
//!
//! Glyphs are packed on shelves in the page's upper part and kept across frames, keyed by
//! cosmic-text's `CacheKey` (font, glyph, size at scale, subpixel bin, flags); the glyph
//! images themselves are not kept on the CPU once uploaded. Each tile has a one-pixel gutter
//! right and below. The atlas starts over when the scale changes.
//!
//! Icons (scene paths) are not cached (their paths are rebuilt each draw, RENDER.md §14.9):
//! each frame rasterizes them at their transform ([`crate::shape::coverage`]) into a ring of
//! two slots in the mask page's lowest quarter, used in turn, so a frame never overwrites the
//! icons of the frame before it.
//!
//! Uploads are coalesced: one region per shelf a frame added tiles to (from its first new
//! tile to the shelf's end, a strip nobody else uses) and one for the frame's icon slot.
//!
//! Must not own: textures or copies (the backend's), rasterizing (`crate::glyph`,
//! `crate::shape`), or geometry (`crate::gpu`).

use std::collections::HashMap;
use std::fmt;

use cosmic_text::{CacheKey, SwashCache, SwashContent};
use sighurt_api::geometry::Scale;

use crate::backend::AtlasStats;
use crate::glyph;
use crate::shape::Coverage;
use crate::text::FontSystemHandle;

/// A page's side when the atlas starts.
pub const INITIAL_SIZE: u32 = 512;
/// The largest side a page grows to.
pub const MAX_SIZE: u32 = 2048;

/// Which page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PageKind {
    /// R8: coverage.
    Mask,
    /// RGBA8, premultiplied.
    Color,
}

impl PageKind {
    /// Bytes per texel.
    pub fn bytes_per_texel(self) -> u32 {
        match self {
            Self::Mask => 1,
            Self::Color => 4,
        }
    }
}

/// Texels to copy into a page: `data` holds `height` rows of `width` texels, packed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upload {
    /// Into which page.
    pub page: PageKind,
    /// Where, in texels.
    pub x: u32,
    /// Where, in texels.
    pub y: u32,
    /// Its width in texels.
    pub width: u32,
    /// Its height in texels.
    pub height: u32,
    /// The texels.
    pub data: Vec<u8>,
}

/// A frame's images didn't fit in a page even after it started over at its largest size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtlasFull {
    /// The page.
    pub page: PageKind,
    /// Its side then.
    pub size: u32,
}

impl fmt::Display for AtlasFull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "one frame's images don't fit in the {:?} atlas page at {}²",
            self.page, self.size
        )
    }
}

impl std::error::Error for AtlasFull {}

/// Where an image is in a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tile {
    /// The page.
    pub page: PageKind,
    /// Its left texel.
    pub x: u32,
    /// Its top texel.
    pub y: u32,
    /// Its width in texels.
    pub width: u32,
    /// Its height in texels.
    pub height: u32,
    /// For a glyph: its image's left edge from the glyph origin, in pixels (swash's placement).
    pub left: i32,
    /// For a glyph: its image's top edge above the baseline, in pixels.
    pub top: i32,
}

/// A row of tiles of at most `height` (gutter included), filled left to right.
#[derive(Clone, Copy, Debug)]
struct Shelf {
    y: u32,
    height: u32,
    /// The next free texel.
    x: u32,
    /// The first texel this frame added a tile at.
    dirty_from: Option<u32>,
}

/// Shelf packing in a rectangle of a page.
#[derive(Clone, Debug)]
struct Shelves {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
    shelves: Vec<Shelf>,
    /// The first row no shelf takes.
    bottom: u32,
}

impl Shelves {
    fn new(left: u32, top: u32, width: u32, height: u32) -> Self {
        Self {
            left,
            top,
            width,
            height,
            shelves: Vec::new(),
            bottom: top,
        }
    }

    /// Room for `width` × `height` texels and their gutter; returns the tile's corner.
    fn place(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        let (w, h) = (width + 1, height + 1);
        let right = self.left + self.width;
        // The lowest shelf that takes it without wasting more than half its height.
        let fits = |shelf: &Shelf| shelf.height >= h && shelf.height <= h + h / 2 + 1;
        let index = match self
            .shelves
            .iter()
            .position(|shelf| fits(shelf) && shelf.x + w <= right)
        {
            Some(index) => index,
            None => {
                if w > self.width || self.bottom + h > self.top + self.height {
                    return None;
                }
                self.shelves.push(Shelf {
                    y: self.bottom,
                    height: h,
                    x: self.left,
                    dirty_from: None,
                });
                self.bottom += h;
                self.shelves.len() - 1
            }
        };
        let shelf = &mut self.shelves[index];
        let x = shelf.x;
        shelf.x += w;
        shelf.dirty_from.get_or_insert(x);
        Some((x, shelf.y))
    }

    /// The region of each shelf this frame added to, and forgets that it did.
    fn take_dirty(&mut self) -> Vec<(u32, u32, u32, u32)> {
        let right = self.left + self.width;
        let bottom = self.top + self.height;
        let mut regions = Vec::new();
        for shelf in &mut self.shelves {
            if let Some(from) = shelf.dirty_from.take() {
                let width = shelf.x.min(right) - from;
                let height = shelf.height.min(bottom - shelf.y);
                regions.push((from, shelf.y, width, height));
            }
        }
        regions
    }
}

/// One page's plan.
#[derive(Clone, Debug)]
struct Page {
    kind: PageKind,
    size: u32,
    /// Glyphs: the whole color page, the mask page above its icon ring.
    glyphs: Shelves,
    /// Started over this frame already.
    reset_this_frame: bool,
}

impl Page {
    fn new(kind: PageKind, size: u32) -> Self {
        let glyph_rows = match kind {
            PageKind::Mask => size - ring_height(size),
            PageKind::Color => size,
        };
        Self {
            kind,
            size,
            glyphs: Shelves::new(0, 0, size, glyph_rows),
            reset_this_frame: false,
        }
    }
}

/// The icon ring's rows at the bottom of a mask page of `size`.
fn ring_height(size: u32) -> u32 {
    size / 4
}

/// A tile placed this frame and not uploaded yet, with its texels.
struct Pending {
    tile: Tile,
    icon: bool,
    data: Vec<u8>,
}

/// A GPU backend's atlas plan. One serves every surface of the backend.
pub struct Atlas {
    swash: SwashCache,
    mask: Page,
    color: Page,
    /// Every glyph placed, or known to have no image (`None`).
    glyphs: HashMap<CacheKey, Option<Tile>>,
    scale: Option<Scale>,
    /// The ring slot of this frame's icons, and their packing.
    slot: usize,
    icons: Shelves,
    pending: Vec<Pending>,
    max: u32,
    stats: AtlasStats,
}

impl Default for Atlas {
    fn default() -> Self {
        Self::new()
    }
}

impl Atlas {
    /// An empty atlas, its pages at [`INITIAL_SIZE`].
    pub fn new() -> Self {
        Self::with_sizes(INITIAL_SIZE, MAX_SIZE)
    }

    /// An empty atlas whose pages start at `initial` and grow to `max` (powers of two, at least
    /// 16): for tests of what happens when it fills.
    #[doc(hidden)]
    pub fn with_sizes(initial: u32, max: u32) -> Self {
        assert!(initial.is_power_of_two() && max >= initial && initial >= 16);
        let mask = Page::new(PageKind::Mask, initial);
        Self {
            swash: SwashCache::new(),
            icons: ring_slot(&mask, 0),
            mask,
            color: Page::new(PageKind::Color, initial),
            glyphs: HashMap::new(),
            scale: None,
            slot: 0,
            pending: Vec::new(),
            max,
            stats: AtlasStats {
                mask_size: initial,
                color_size: initial,
                ..AtlasStats::default()
            },
        }
    }

    /// A page's side now: the backend's texture must have it.
    pub fn size(&self, page: PageKind) -> u32 {
        self.page(page).size
    }

    /// Glyphs placed.
    pub fn glyphs(&self) -> usize {
        self.glyphs.values().filter(|tile| tile.is_some()).count()
    }

    /// What it counted, and its pages' sizes and bytes.
    pub fn stats(&self) -> AtlasStats {
        let bytes =
            |page: &Page| u64::from(page.size).pow(2) * u64::from(page.kind.bytes_per_texel());
        AtlasStats {
            mask_size: self.mask.size,
            color_size: self.color.size,
            bytes: bytes(&self.mask) + bytes(&self.color),
            ..self.stats
        }
    }

    fn page(&self, kind: PageKind) -> &Page {
        match kind {
            PageKind::Mask => &self.mask,
            PageKind::Color => &self.color,
        }
    }

    fn page_mut(&mut self, kind: PageKind) -> &mut Page {
        match kind {
            PageKind::Mask => &mut self.mask,
            PageKind::Color => &mut self.color,
        }
    }

    /// Starts a frame at `scale`: a new scale starts both pages over; the icons go into the
    /// next ring slot.
    pub(crate) fn begin_frame(&mut self, scale: Scale) {
        if self.scale.is_some_and(|old| old != scale) {
            for kind in [PageKind::Mask, PageKind::Color] {
                if self.page(kind).glyphs.shelves.is_empty() {
                    continue;
                }
                let size = self.page(kind).size;
                *self.page_mut(kind) = Page::new(kind, size);
                self.stats.resets += 1;
            }
            self.glyphs.clear();
        }
        self.scale = Some(scale);
        self.mask.reset_this_frame = false;
        self.color.reset_this_frame = false;
        self.pending.clear();
        self.slot = 1 - self.slot;
        self.icons = ring_slot(&self.mask, self.slot);
    }

    /// The tile of the glyph `key`, placing it (rasterized from `fonts`) if it's new. `Ok(None)`
    /// for a glyph with no image; `Err` names the page that is full.
    pub(crate) fn glyph(
        &mut self,
        fonts: &FontSystemHandle,
        key: CacheKey,
    ) -> Result<Option<Tile>, PageKind> {
        if let Some(&tile) = self.glyphs.get(&key) {
            return Ok(tile);
        }
        let image = glyph::rasterize(fonts, &mut self.swash, key);
        let Some(image) = image.filter(|i| i.placement.width > 0 && i.placement.height > 0) else {
            self.glyphs.insert(key, None);
            return Ok(None);
        };
        let (width, height) = (image.placement.width, image.placement.height);
        let (kind, data) = match image.content {
            SwashContent::Mask => (PageKind::Mask, image.data),
            // Straight alpha from swash; the pages are premultiplied.
            SwashContent::Color => (PageKind::Color, premultiply(image.data)),
            // The CPU backend draws none either.
            SwashContent::SubpixelMask => {
                self.glyphs.insert(key, None);
                return Ok(None);
            }
        };
        let (x, y) = self
            .page_mut(kind)
            .glyphs
            .place(width, height)
            .ok_or(kind)?;
        let tile = Tile {
            page: kind,
            x,
            y,
            width,
            height,
            left: image.placement.left,
            top: image.placement.top,
        };
        self.glyphs.insert(key, Some(tile));
        self.pending.push(Pending {
            tile,
            icon: false,
            data,
        });
        Ok(Some(tile))
    }

    /// Where the glyph `key` was placed, if it has an image.
    pub(crate) fn placed(&self, key: CacheKey) -> Option<Tile> {
        self.glyphs.get(&key).copied().flatten()
    }

    /// A tile for this frame's icon `coverage` in the ring slot; `Err` if the slot is full.
    pub(crate) fn icon(&mut self, coverage: Coverage) -> Result<Tile, PageKind> {
        let (width, height) = (coverage.bounds.width, coverage.bounds.height);
        let (x, y) = self.icons.place(width, height).ok_or(PageKind::Mask)?;
        let tile = Tile {
            page: PageKind::Mask,
            x,
            y,
            width,
            height,
            left: 0,
            top: 0,
        };
        self.pending.push(Pending {
            tile,
            icon: true,
            data: coverage.data,
        });
        Ok(tile)
    }

    /// Makes room in `kind` after it was full: doubles it while it may grow, else starts it
    /// over once a frame. Either way the page is new (and so are this frame's icons, when it is
    /// the mask page). `Err` when it already started over this frame.
    pub(crate) fn make_room(&mut self, kind: PageKind) -> Result<(), AtlasFull> {
        let (size, reset) = {
            let page = self.page(kind);
            (page.size, page.reset_this_frame)
        };
        let new_size = if size < self.max {
            self.stats.grows += 1;
            size * 2
        } else if !reset {
            self.stats.resets += 1;
            size
        } else {
            self.stats.overflows += 1;
            return Err(AtlasFull { page: kind, size });
        };
        let mut page = Page::new(kind, new_size);
        page.reset_this_frame = size == new_size;
        *self.page_mut(kind) = page;
        self.glyphs
            .retain(|_, tile| tile.is_none_or(|t| t.page != kind));
        // Icons are placed anew each attempt: the frame's placement restarts.
        self.pending.retain(|p| p.tile.page != kind && !p.icon);
        self.icons = ring_slot(&self.mask, self.slot);
        Ok(())
    }

    /// This frame's uploads, coalesced: a region per shelf added to, one for the icons.
    pub(crate) fn take_uploads(&mut self) -> Vec<Upload> {
        let mut uploads = Vec::new();
        let pending = std::mem::take(&mut self.pending);
        for kind in [PageKind::Mask, PageKind::Color] {
            for (x, y, width, height) in self.page_mut(kind).glyphs.take_dirty() {
                uploads.push(region(kind, x, y, width, height, &pending, false));
            }
        }
        if let Some((x, y, width, height)) = used(&self.icons) {
            uploads.push(region(PageKind::Mask, x, y, width, height, &pending, true));
        }
        self.icons.take_dirty();
        uploads
    }
}

/// The ring slot `slot` (0 or 1) of `mask`: a half of its lowest quarter.
fn ring_slot(mask: &Page, slot: usize) -> Shelves {
    let height = ring_height(mask.size);
    let width = mask.size / 2;
    Shelves::new(width * slot as u32, mask.size - height, width, height)
}

/// The rectangle of `shelves` that tiles took, if any.
fn used(shelves: &Shelves) -> Option<(u32, u32, u32, u32)> {
    let right = shelves.shelves.iter().map(|shelf| shelf.x).max()?;
    let width = (right - shelves.left).min(shelves.width);
    let height = (shelves.bottom - shelves.top).min(shelves.height);
    (width > 0 && height > 0).then_some((shelves.left, shelves.top, width, height))
}

/// The upload of a region of page `kind`: zeros, with the pending tiles inside it copied in.
fn region(
    kind: PageKind,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    pending: &[Pending],
    icons: bool,
) -> Upload {
    let bpp = kind.bytes_per_texel() as usize;
    let mut data = vec![0u8; width as usize * height as usize * bpp];
    let inside = |t: &Tile| {
        t.page == kind
            && t.x >= x
            && t.y >= y
            && t.x + t.width <= x + width
            && t.y + t.height <= y + height
    };
    for p in pending
        .iter()
        .filter(|p| p.icon == icons && inside(&p.tile))
    {
        let row = p.tile.width as usize * bpp;
        for (r, source) in p.data.chunks_exact(row).enumerate() {
            let start = ((p.tile.y - y) as usize + r) * width as usize * bpp
                + (p.tile.x - x) as usize * bpp;
            data[start..start + row].copy_from_slice(source);
        }
    }
    Upload {
        page: kind,
        x,
        y,
        width,
        height,
        data,
    }
}

/// Straight-alpha RGBA bytes, premultiplied as tiny-skia rounds.
fn premultiply(mut data: Vec<u8>) -> Vec<u8> {
    for px in data.as_chunks_mut::<4>().0 {
        let a = u16::from(px[3]);
        for c in &mut px[..3] {
            *c = ((u16::from(*c) * a + 127) / 255) as u8;
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shelves_pack_with_gutters_and_report_their_regions() {
        let mut shelves = Shelves::new(0, 0, 64, 64);
        assert_eq!(shelves.place(10, 12), Some((0, 0)));
        assert_eq!(
            shelves.place(10, 11),
            Some((11, 0)),
            "same shelf, one texel apart"
        );
        assert_eq!(
            shelves.place(10, 30),
            Some((0, 13)),
            "too tall: a new shelf"
        );
        assert_eq!(shelves.place(64, 5), None, "no room for its gutter");
        assert_eq!(shelves.take_dirty(), [(0, 0, 22, 13), (0, 13, 11, 31)]);
        assert!(shelves.take_dirty().is_empty());
        assert_eq!(shelves.place(5, 10), Some((22, 0)));
        assert_eq!(shelves.take_dirty(), [(22, 0, 6, 13)], "only the new strip");
        assert_eq!(shelves.place(10, 30), Some((11, 13)));
        assert_eq!(shelves.place(10, 30), Some((22, 13)));
        assert_eq!(shelves.place(60, 40), None, "no rows left");
    }

    #[test]
    fn regions_copy_their_tiles_and_zero_the_rest() {
        let tile = |x, y| Tile {
            page: PageKind::Mask,
            x,
            y,
            width: 2,
            height: 2,
            left: 0,
            top: 0,
        };
        let pending = [
            Pending {
                tile: tile(4, 0),
                icon: false,
                data: vec![1, 2, 3, 4],
            },
            Pending {
                tile: tile(7, 0),
                icon: false,
                data: vec![5, 6, 7, 8],
            },
        ];
        let upload = region(PageKind::Mask, 4, 0, 6, 3, &pending, false);
        assert_eq!(
            upload.data,
            [1, 2, 0, 5, 6, 0, 3, 4, 0, 7, 8, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn color_glyphs_are_premultiplied() {
        assert_eq!(
            premultiply(vec![255, 128, 0, 128, 9, 9, 9, 255]),
            [128, 64, 0, 128, 9, 9, 9, 255]
        );
    }
}
