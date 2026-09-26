//! What the integration tests share: a chrome with the bundled test font, snapshots, and
//! painting into a wl_shm-style buffer.

#![allow(dead_code)]

use sighurt_api::geometry::{LogicalSize, Metrics, Scale, WindowRect};
use sighurt_api::ids::{LayoutEpoch, TabId};
use sighurt_ui::{
    Chrome, Colors, CpuPainter, EngineRow, Fonts, HomeInfo, LayoutOptions, PixelBuffer, Snapshot,
    TabContent, TabSnapshot,
};

/// The bundled test font (see tests/fonts/README.md).
pub const FONT: &[u8] = include_bytes!("../fonts/Cantarell-Regular.ttf");

/// The scales the tests cover, as numerators over 120: 100 %, 125 %, 150 % and 200 %.
pub const SCALES: [u32; 4] = [120, 150, 180, 240];

pub fn fonts() -> Fonts {
    Fonts::from_data([FONT.to_vec()])
}

/// A chrome for a window of `width` × `height` showing `snapshot`.
pub fn chrome(width: u32, height: u32, snapshot: Snapshot) -> Chrome {
    chrome_with(width, height, snapshot, Colors::default())
}

pub fn chrome_with(width: u32, height: u32, snapshot: Snapshot, colors: Colors) -> Chrome {
    let mut chrome = Chrome::new(fonts(), colors, LayoutOptions::default());
    chrome.resize(LogicalSize { width, height });
    chrome.set_snapshot(snapshot);
    chrome
}

/// A tab called `title` (the core's title for it) showing `url`.
pub fn tab(title: &str, url: &str, content: TabContent) -> TabSnapshot {
    TabSnapshot {
        id: TabId::next(),
        title: title.into(),
        url: url.into(),
        loading: false,
        active: false,
        content,
    }
}

/// Three tabs, the first active: a page, a loading page with a long Swedish title, the home
/// page.
pub fn three_tabs() -> Snapshot {
    let mut tabs = vec![
        tab(
            "Sighurt — a small browser",
            "https://sighurt.example/",
            TabContent::Engine,
        ),
        tab(
            "Blåbärssylt och kanelbullar från Göteborg",
            "https://recept.example/sylt",
            TabContent::Engine,
        ),
        tab("sig://home", "sig://home", TabContent::Internal),
    ];
    tabs[0].active = true;
    tabs[1].loading = true;
    Snapshot {
        tabs,
        can_go_back: false,
        can_go_forward: true,
        status: Some("https://sighurt.example/docs/räksmörgås".into()),
        home: home_info(),
    }
}

pub fn home_info() -> HomeInfo {
    HomeInfo {
        config_path: "/home/user/.config/sighurt/config.toml".into(),
        engines: vec![
            EngineRow {
                name: "servo".into(),
                detail: "compiled in · default".into(),
                available: true,
            },
            EngineRow {
                name: "wasm".into(),
                detail: "compiled in".into(),
                available: true,
            },
            EngineRow {
                name: "demo".into(),
                detail: "not compiled into this build".into(),
                available: false,
            },
        ],
    }
}

/// A snapshot whose active tab shows `content` at `url`.
pub fn showing(url: &str, content: TabContent) -> Snapshot {
    let mut only = tab(url, url, content);
    only.active = true;
    Snapshot {
        tabs: vec![only],
        home: home_info(),
        ..Snapshot::default()
    }
}

/// A painted buffer, turned from wl_shm's ARGB8888 into RGBA bytes.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y * self.width + x) as usize * 4;
        self.rgba[i..i + 4].try_into().unwrap()
    }
}

pub fn metrics(rect: WindowRect, scale: u32) -> Metrics {
    Metrics::new(
        LayoutEpoch::next(),
        rect,
        Scale::from_fractional(scale).unwrap(),
    )
}

/// Paints what `chrome` shows in `rect` at `scale` (over 120) and reads the buffer back.
pub fn paint(chrome: &mut Chrome, rect: WindowRect, scale: u32) -> Image {
    let metrics = metrics(rect, scale);
    let size = metrics.buffer();
    let mut data = vec![0u8; size.width as usize * size.height as usize * 4];
    let buffer = PixelBuffer {
        data: &mut data,
        width: size.width,
        height: size.height,
        stride: size.width as usize * 4,
    };
    CpuPainter::new().paint(chrome, &metrics, buffer).unwrap();
    Image {
        width: size.width,
        height: size.height,
        rgba: argb_to_rgba(&data),
    }
}

/// wl_shm ARGB8888 (little-endian: B, G, R, A) to R, G, B, A.
pub fn argb_to_rgba(data: &[u8]) -> Vec<u8> {
    data.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&pixel| {
            let argb = u32::from_le_bytes(pixel);
            let [a, r, g, b] = argb.to_be_bytes();
            [r, g, b, a]
        })
        .collect()
}
