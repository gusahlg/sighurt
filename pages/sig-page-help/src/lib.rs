//! `sig://help`: what a new user of sig needs to know.
//!
//! Owns the page's frame loop: which section the URL names, the sidebar (or the row of links
//! on a narrow canvas), scrolling, and drawing the laid-out section. The words are in
//! [`sections`], the browser's answers in [`data`], layout and drawing in `sig-page-kit`.

mod data;
mod sections;

use data::Data;
use sections::{Section, ALL};
use sig_page_kit::sig::{self, InputKind};
use sig_page_kit::ui::{self, Builder, Font, Op, Role};
use sighurt_sdk as sdk;
use std::cell::RefCell;

const SIDEBAR_W: f32 = 200.0;
const NARROW: f32 = 640.0;
const MAX_COLUMN: f32 = 760.0;
const LINE_STEP: f32 = 40.0;
const ITEM_H: f32 = 34.0;

/// What a layout was made for; a different key means making it again.
#[derive(Clone, Copy, PartialEq)]
struct Key {
    section: Section,
    x: u32,
    width: u32,
    version: u32,
}

struct Layout {
    key: Key,
    ops: Vec<Op>,
    height: f32,
}

/// A clickable entry of the sidebar or the narrow header.
struct NavItem {
    section: Section,
    x: f32,
    y: f32,
    w: f32,
}

struct App {
    data: Data,
    section: Section,
    last_url: String,
    scroll: [f32; ALL.len()],
    layout: Option<Layout>,
    label_w: Vec<f32>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

#[no_mangle]
pub extern "C" fn start_app() {
    APP.with(|app| {
        *app.borrow_mut() = Some(App {
            data: Data::ask(),
            section: Section::from_url(&sdk::get_url()),
            last_url: sdk::get_url(),
            scroll: [0.0; ALL.len()],
            layout: None,
            label_w: Vec::new(),
        });
    });
}

#[no_mangle]
pub extern "C" fn on_frame(_dt_ms: u32) {
    APP.with(|app| {
        if let Some(app) = app.borrow_mut().as_mut() {
            app.frame();
        }
    });
}

impl App {
    fn go(&mut self, section: Section) {
        if section != self.section {
            self.section = section;
            self.scroll[section.index()] = 0.0;
            sdk::push_state(&[], &format!("Help: {}", section.label()), &section.url());
        }
    }

    fn nav_items(&mut self, width: f32, narrow: bool) -> (Vec<NavItem>, f32) {
        if !narrow {
            let items = ALL
                .iter()
                .enumerate()
                .map(|(i, &section)| NavItem {
                    section,
                    x: 12.0,
                    y: 72.0 + i as f32 * ITEM_H,
                    w: SIDEBAR_W - 24.0,
                })
                .collect();
            return (items, 0.0);
        }
        if self.label_w.iter().any(|&w| w <= 0.0) || self.label_w.is_empty() {
            self.label_w = ALL
                .iter()
                .map(|s| ui::measure(&Font::new(15.0), s.label()) + 20.0)
                .collect();
        }
        let (mut x, mut y) = (8.0, 6.0);
        let mut items = Vec::new();
        for (&section, &w) in ALL.iter().zip(&self.label_w) {
            if x + w > width - 8.0 && x > 8.0 {
                x = 8.0;
                y += ITEM_H;
            }
            items.push(NavItem { section, x, y, w });
            x += w + 4.0;
        }
        (items, y + ITEM_H + 6.0)
    }

    fn frame(&mut self) {
        let (cw, ch) = sdk::canvas_dimensions();
        let (w, h) = (cw as f32, ch as f32);
        let dark = sdk::system_theme() == sdk::THEME_DARK;
        let narrow = w < NARROW;

        self.data.poll();
        let url = sdk::get_url();
        if url != self.last_url {
            self.section = Section::from_url(&url);
            self.last_url = url;
        }

        let (nav, header_h) = self.nav_items(w, narrow);
        let view_h = (h - header_h).max(1.0);
        let left = if narrow { 0.0 } else { SIDEBAR_W };
        let side = if narrow { 16.0 } else { 32.0 };
        let col_w = MAX_COLUMN.min(w - left - 2.0 * side).max(120.0);
        let col_x = left + ((w - left - col_w) / 2.0).max(0.0);

        let key = Key {
            section: self.section,
            x: col_x.to_bits(),
            width: col_w.to_bits(),
            version: self.data.version,
        };
        if self.layout.as_ref().map(|l| l.key) != Some(key) {
            let mut b = Builder::new(col_x, col_w, 28.0, ui::measure);
            sections::build(&mut b, self.section, &self.data);
            let (ops, height) = b.finish();
            self.layout = Some(Layout { key, ops, height });
        }
        let Some(layout) = &self.layout else { return };
        let max_scroll = (layout.height + header_h - h).max(0.0);

        let i = self.section.index();
        let mut scroll = self.scroll[i];
        while let Some(input) = sig::input_next() {
            if input.kind == InputKind::Paste {
                continue;
            }
            let page = view_h * 0.9;
            scroll += if input.is_key("ArrowDown", 0) {
                LINE_STEP
            } else if input.is_key("ArrowUp", 0) {
                -LINE_STEP
            } else if input.is_key("PageDown", 0) || input.is_key(" ", 0) {
                page
            } else if input.is_key("PageUp", 0) || input.is_key(" ", sig::modifiers::SHIFT) {
                -page
            } else if input.is_key("Home", 0) {
                -scroll
            } else if input.is_key("End", 0) {
                max_scroll - scroll
            } else {
                0.0
            };
        }
        scroll -= sdk::scroll_delta().1;
        scroll = scroll.clamp(0.0, max_scroll);
        self.scroll[i] = scroll;

        let (mx, my) = sdk::mouse_position();
        let mut target = None;
        if sdk::mouse_button_clicked(0) {
            target = nav
                .iter()
                .find(|n| mx >= n.x && mx < n.x + n.w && my >= n.y && my < n.y + ITEM_H)
                .map(|n| n.section);
            if target.is_none() && my >= header_h && mx >= left {
                let (cx, cy) = (mx, my - header_h + scroll);
                target = layout.ops.iter().find_map(|op| match op {
                    Op::Link { x, y, w, h, url }
                        if url.starts_with("sig://help")
                            && cx >= *x
                            && cx < x + w
                            && cy >= *y
                            && cy < y + h =>
                    {
                        Some(Section::from_url(url))
                    }
                    _ => None,
                });
            }
        }

        // Content, drawn first so the sidebar or header covers what scrolls under it.
        sdk::clear_hyperlinks();
        let [r, g, b] = ui::color(Role::Bg, dark);
        sdk::canvas_clear(r, g, b, 255);
        let shift = scroll - header_h;
        for op in &layout.ops {
            let (top, bottom) = op.span();
            if bottom < shift || top > shift + h {
                continue;
            }
            match op {
                Op::Link { x, y, w, h, url } => {
                    if !url.starts_with("sig://help") && y - shift >= header_h {
                        sdk::register_hyperlink(*x, y - shift, *w, *h, url);
                    }
                }
                _ => ui::draw(op, shift, dark),
            }
        }
        if layout.height + header_h > h {
            let track = view_h;
            let thumb = (track * view_h / (layout.height)).clamp(24.0, track);
            let y = header_h + (track - thumb) * scroll / max_scroll.max(1.0);
            ui::draw(
                &Op::Rect {
                    x: w - 6.0,
                    y,
                    w: 4.0,
                    h: thumb,
                    radius: 2.0,
                    role: Role::Rule,
                },
                0.0,
                dark,
            );
        }

        self.draw_nav(&nav, w, h, header_h, narrow, (mx, my), dark);
        if let Some(section) = target {
            self.go(section);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_nav(
        &self,
        nav: &[NavItem],
        w: f32,
        h: f32,
        header_h: f32,
        narrow: bool,
        mouse: (f32, f32),
        dark: bool,
    ) {
        let rect = |x, y, w, h, radius, role| {
            ui::draw(
                &Op::Rect {
                    x,
                    y,
                    w,
                    h,
                    radius,
                    role,
                },
                0.0,
                dark,
            )
        };
        let font = Font::new(15.0);
        if narrow {
            rect(0.0, 0.0, w, header_h, 0.0, Role::Panel);
            rect(0.0, header_h - 1.0, w, 1.0, 0.0, Role::Rule);
        } else {
            rect(0.0, 0.0, SIDEBAR_W, h, 0.0, Role::Panel);
            rect(SIDEBAR_W - 1.0, 0.0, 1.0, h, 0.0, Role::Rule);
            let title = Op::Text {
                x: 20.0,
                y: 24.0,
                h: 30.0,
                font: Font::new(20.0).bold(),
                role: Role::Heading,
                text: "sig help".into(),
            };
            ui::draw(&title, 0.0, dark);
        }
        for item in nav {
            let current = item.section == self.section;
            let hover = mouse.0 >= item.x
                && mouse.0 < item.x + item.w
                && mouse.1 >= item.y
                && mouse.1 < item.y + ITEM_H;
            if current {
                rect(item.x, item.y, item.w, ITEM_H - 2.0, 6.0, Role::Selected);
            } else if hover {
                rect(item.x, item.y, item.w, ITEM_H - 2.0, 6.0, Role::CodeBg);
            }
            let (role, font) = if current {
                (Role::SelectedText, font.bold())
            } else {
                (Role::Text, font)
            };
            let text = Op::Text {
                x: item.x + 10.0,
                y: item.y + 5.0,
                h: 24.0,
                font,
                role,
                text: item.section.label().into(),
            };
            ui::draw(&text, 0.0, dark);
        }
    }
}
