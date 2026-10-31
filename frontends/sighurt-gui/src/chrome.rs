//! The chrome as `sig` runs it: what it is told (the configuration's `[ui]` and its own
//! `[chrome.<name>]`, a snapshot of the core with every string contained, and its own commands),
//! and its named surfaces as the host's targets.
//!
//! [`ChromeTargets`] keeps the surface part of the chrome API's contract
//! (`sighurt_ui::extension`): one target for each name the chrome has used, at most
//! `MAX_SURFACES`; a listed surface at its rectangle clipped to the window, taking input on its
//! input area clipped to it; a name no longer listed hidden at its last rectangle. It is plain
//! bookkeeping: the owner loop creates a target for each name it hasn't seen and hands the
//! placement to the host, which validates it. The chromes of the build are listed here
//! ([`CHROMES`]), compiled in and chosen by `[ui] chrome` at startup.
//!
//! Must not own: the chrome's layout and painting (the chrome), browser state (the core), or
//! targets' lifetimes (the owner loop and the host).

use sighurt_api::geometry::{LogicalSize, SurfaceRect, WindowRect};
use sighurt_core::config::Config;
use sighurt_core::tabs::{Content, TabSummary};
use sighurt_core::{BrowserState, ChromeChecks};
use sighurt_ui::extension::{
    ChromeEvent, ChromeKind, ChromeLayout, InputArea, SurfaceName, Theme, MAX_SURFACES,
};
use sighurt_ui::{HomeInfo, Snapshot, TabContent, TabSnapshot};
use sighurt_wayland::InputRegion;

/// The chromes compiled into this build: the standard one always, then those of features.
pub const CHROMES: &[ChromeKind] = &[
    sighurt_ui::standard::KIND,
    #[cfg(feature = "chrome-line")]
    sighurt_chrome_line::KIND,
];

/// The chrome of this build called `name`. A configuration checked against [`CHROMES`] names
/// one of them; the standard chrome stands in otherwise.
pub fn chrome(name: &str) -> ChromeKind {
    let found = CHROMES.iter().find(|chrome| chrome.name == name);
    found.copied().unwrap_or(sighurt_ui::standard::KIND)
}

/// What the core checks of `chrome`.
pub fn checks(chrome: &ChromeKind) -> ChromeChecks {
    ChromeChecks {
        name: chrome.name,
        check: chrome.check,
        commands: chrome.commands,
    }
}

/// What the core checks of every chrome of this build.
pub fn checks_all() -> Vec<ChromeChecks> {
    CHROMES.iter().map(checks).collect()
}

/// What a chrome called `name` is told of `config`: `[ui]`, and `[chrome.<name>]`.
pub fn configure(config: &Config, name: &str) -> ChromeEvent {
    ChromeEvent::Configure {
        theme: theme(config),
        settings: config.chrome_settings(name).cloned().unwrap_or_default(),
    }
}

/// The look every chrome shares: `config`'s `[ui]`.
pub fn theme(config: &Config) -> Theme {
    let mut theme = Theme::default();
    for (color, value) in config.colors() {
        // The core checked each against `Colors::NAMES`, the names it was given.
        let set = theme.colors.set(color, value);
        debug_assert!(set.is_ok(), "{set:?}");
    }
    theme.font = config.font().map(str::to_owned);
    theme.font_size = config.font_size();
    theme
}

/// `chrome.<action>` for the chrome running as `kind`: its event, or the notice that it has no
/// such command. The configuration's commands were checked against the chrome it names, so this
/// happens only for one typed after `:`, or after `[ui] chrome` changed without a restart.
pub fn command(kind: &ChromeKind, action: String) -> Result<ChromeEvent, String> {
    if kind.commands.contains(&action.as_str()) {
        return Ok(ChromeEvent::Command(action));
    }
    let has = match kind.commands {
        [] => "it has none".to_owned(),
        commands => format!("it has chrome.{}", commands.join(", chrome.")),
    };
    Err(format!(
        "The {} chrome has no command chrome.{action}; {has}.",
        kind.name
    ))
}

/// The engine `tab`'s page runs in, by its configured name: the engine of the view it shows, and
/// none for `sig:` pages, blank tabs and errors. Browser state the core keeps, which the chrome
/// labels the tab with.
pub fn engine<'a>(core: &'a BrowserState, tab: &TabSummary<'_>) -> Option<&'a str> {
    match tab.content {
        Content::View(view) => core.view(view).map(|view| view.engine()),
        Content::Internal(_) | Content::Blank => None,
    }
}

/// What the chrome shows of `core`: its tabs, `status` (the active tab's status line) and
/// `home`, with every string contained to the bound its field states.
pub fn snapshot(
    core: &BrowserState,
    home: HomeInfo,
    status: impl Fn(&TabSummary<'_>) -> Option<String>,
) -> Snapshot {
    let active = core.active();
    let mut snapshot = Snapshot {
        home,
        keymap: core.keymap_indicator().map(str::to_owned),
        ..Snapshot::default()
    };
    for (index, tab) in core.tabs().enumerate() {
        let content = match (tab.content, tab.error) {
            (Content::View(_), _) => TabContent::Engine,
            (Content::Internal(_), _) => TabContent::Internal,
            (Content::Blank, Some(error)) => TabContent::Error(error.message.clone()),
            (Content::Blank, None) => TabContent::Blank,
        };
        let engine = engine(core, &tab).map(str::to_owned);
        let switching_to = tab.switching_to.map(str::to_owned);
        if index == active {
            snapshot.can_go_back = tab.can_go_back;
            snapshot.can_go_forward = tab.can_go_forward;
            snapshot.status = status(&tab);
        }
        snapshot.tabs.push(TabSnapshot {
            id: tab.id,
            title: tab.title.into_owned(),
            url: tab.url.unwrap_or_default().to_owned(),
            loading: tab.loading,
            active: index == active,
            engine,
            switching_to,
            content,
        });
    }
    snapshot.contain();
    snapshot
}

/// The chrome's surfaces as targets: which names it has used, and where each went last.
#[derive(Debug, Default)]
pub struct ChromeTargets {
    /// Every name the chrome has used, in the order it first did, with where it was last
    /// listed.
    known: Vec<(SurfaceName, WindowRect, InputRegion)>,
    /// Names refused for being one too many, so each is reported once.
    refused: Vec<SurfaceName>,
}

/// Where one of the chrome's surfaces goes.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    /// The surface.
    pub name: SurfaceName,
    /// Its rectangle, inside the window or empty.
    pub rect: WindowRect,
    /// Whether the chrome lists it now.
    pub visible: bool,
    /// Where it takes pointer input, in its own coordinates.
    pub input: InputRegion,
}

/// The chrome's part of a placement.
#[derive(Debug, PartialEq)]
pub struct Placing {
    /// Where the page goes, inside the window.
    pub page: WindowRect,
    /// Every surface the chrome has used, bottom to top: the hidden ones, then the listed ones
    /// in the chrome's order.
    pub surfaces: Vec<Placed>,
    /// Names refused for the first time: the chrome uses more than `MAX_SURFACES`.
    pub refused: Vec<SurfaceName>,
}

impl ChromeTargets {
    /// Where the surfaces of `layout` go in a window of `window` size. A name listed twice is
    /// placed once.
    pub fn place(&mut self, layout: &ChromeLayout, window: LogicalSize) -> Placing {
        let bounds = WindowRect {
            x: 0,
            y: 0,
            width: window.width,
            height: window.height,
        };
        let mut listed: Vec<Placed> = Vec::new();
        let mut refused = Vec::new();
        for surface in &layout.surfaces {
            if listed.iter().any(|placed| placed.name == surface.name) {
                continue;
            }
            let known = self
                .known
                .iter()
                .position(|(name, ..)| *name == surface.name);
            if known.is_none() && self.known.len() == MAX_SURFACES {
                if !self.refused.contains(&surface.name) {
                    self.refused.push(surface.name);
                    refused.push(surface.name);
                }
                continue;
            }
            let rect = clip(surface.rect, bounds);
            let input = region(&surface.input, surface.rect, rect);
            let entry = (surface.name, rect, input.clone());
            match known {
                Some(i) => self.known[i] = entry,
                None => self.known.push(entry),
            }
            listed.push(Placed {
                name: surface.name,
                rect,
                visible: true,
                input,
            });
        }
        let mut surfaces: Vec<Placed> = self
            .known
            .iter()
            .filter(|(name, ..)| !listed.iter().any(|placed| placed.name == *name))
            .map(|(name, rect, input)| Placed {
                name,
                rect: clip(*rect, bounds),
                visible: false,
                input: input.clone(),
            })
            .collect();
        surfaces.extend(listed);
        Placing {
            page: clip(layout.page, bounds),
            surfaces,
            refused,
        }
    }
}

/// The part of `a` inside `b`, if they overlap.
fn intersect(a: WindowRect, b: WindowRect) -> Option<WindowRect> {
    let left = i64::from(a.x).max(i64::from(b.x));
    let top = i64::from(a.y).max(i64::from(b.y));
    let right = (i64::from(a.x) + i64::from(a.width)).min(i64::from(b.x) + i64::from(b.width));
    let bottom = (i64::from(a.y) + i64::from(a.height)).min(i64::from(b.y) + i64::from(b.height));
    // Both corners lie within `a` and `b`, so they fit an i32 and the sizes a u32.
    (right > left && bottom > top).then(|| WindowRect {
        x: left as i32,
        y: top as i32,
        width: (right - left) as u32,
        height: (bottom - top) as u32,
    })
}

/// `rect` clipped to `bounds`; empty where they don't overlap.
fn clip(rect: WindowRect, bounds: WindowRect) -> WindowRect {
    intersect(rect, bounds).unwrap_or(WindowRect {
        width: 0,
        height: 0,
        ..rect
    })
}

/// `input`, given for a surface at `listed`, for the surface at `placed` (`listed` clipped): its
/// rectangles moved with the surface's corner and clipped to it.
fn region(input: &InputArea, listed: WindowRect, placed: WindowRect) -> InputRegion {
    let InputArea::Rects(rects) = input else {
        return InputRegion::Whole;
    };
    let (dx, dy) = (placed.x - listed.x, placed.y - listed.y);
    let surface = WindowRect {
        x: 0,
        y: 0,
        width: placed.width,
        height: placed.height,
    };
    let inside = |r: &SurfaceRect| {
        let moved = WindowRect {
            x: r.x - dx,
            y: r.y - dy,
            width: r.width,
            height: r.height,
        };
        intersect(moved, surface).map(|r| SurfaceRect {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
        })
    };
    InputRegion::Rects(rects.iter().filter_map(inside).collect())
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use sighurt_api::engine::{EngineCapabilities, EngineCommand};
    use sighurt_api::events::EngineEvent;
    use sighurt_core::{update, BrowserEvent, Effect, Environment, OpenRequest};
    use sighurt_ui::extension::ChromeSurface;

    use super::*;

    fn rect(x: i32, y: i32, width: u32, height: u32) -> WindowRect {
        WindowRect {
            x,
            y,
            width,
            height,
        }
    }

    const WINDOW: LogicalSize = LogicalSize {
        width: 800,
        height: 600,
    };

    fn layout(surfaces: &[(SurfaceName, WindowRect)]) -> ChromeLayout {
        let surfaces = surfaces
            .iter()
            .map(|&(name, at)| ChromeSurface::new(name, at));
        ChromeLayout::new(rect(0, 84, 800, 516), surfaces.collect())
    }

    fn shown(placing: &Placing) -> Vec<(SurfaceName, bool)> {
        let entries = placing.surfaces.iter();
        entries
            .map(|placed| (placed.name, placed.visible))
            .collect()
    }

    #[test]
    fn names_get_targets_in_the_chromes_order() {
        let mut targets = ChromeTargets::default();
        let (top, page) = (rect(0, 0, 800, 84), rect(0, 84, 800, 516));
        let placing = targets.place(&layout(&[("page", page), ("chrome", top)]), WINDOW);
        assert_eq!(shown(&placing), [("page", true), ("chrome", true)]);
        assert_eq!(placing.surfaces[1].rect, top);
        assert_eq!(placing.surfaces[1].input, InputRegion::Whole);
        // Listed in another order, placed in that order; a name listed twice, once.
        let twice = layout(&[("chrome", top), ("page", page), ("chrome", page)]);
        let placing = targets.place(&twice, WINDOW);
        assert_eq!(shown(&placing), [("chrome", true), ("page", true)]);
        assert_eq!(placing.surfaces[0].rect, top);
    }

    #[test]
    fn a_ninth_name_is_refused_and_reported_once() {
        let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
        let surfaces: Vec<_> = names.iter().map(|&n| (n, rect(0, 0, 10, 10))).collect();
        let mut targets = ChromeTargets::default();
        let placing = targets.place(&layout(&surfaces), WINDOW);
        assert_eq!(placing.surfaces.len(), MAX_SURFACES);
        assert_eq!(placing.refused, ["i"]);
        let placing = targets.place(&layout(&surfaces), WINDOW);
        assert_eq!((placing.surfaces.len(), placing.refused.len()), (8, 0));
        // A known name stays placeable while the ninth is refused.
        let placing = targets.place(
            &layout(&[("i", rect(0, 0, 1, 1)), ("a", rect(0, 0, 1, 1))]),
            WINDOW,
        );
        let visible: Vec<_> = placing
            .surfaces
            .iter()
            .filter(|p| p.visible)
            .map(|p| p.name)
            .collect();
        assert_eq!(visible, ["a"]);
    }

    #[test]
    fn rectangles_are_clipped_to_the_window() {
        let mut targets = ChromeTargets::default();
        let mut wide = layout(&[
            ("bar", rect(-10, 590, 200, 50)),
            ("gone", rect(900, 0, 50, 50)),
        ]);
        wide.page = rect(0, 84, 900, 600);
        let placing = targets.place(&wide, WINDOW);
        assert_eq!(placing.surfaces[0].rect, rect(0, 590, 190, 10));
        assert!(placing.surfaces[1].rect.size().is_empty(), "no room: empty");
        assert!(placing.surfaces[1].visible);
        assert_eq!(placing.page, rect(0, 84, 800, 516));
    }

    #[test]
    fn input_rectangles_are_clipped_to_their_surface() {
        let mut targets = ChromeTargets::default();
        let mut overlay = ChromeSurface::new("overlay", rect(-50, 500, 400, 20));
        overlay.input = InputArea::Rects(vec![
            SurfaceRect {
                x: 40,
                y: 0,
                width: 100,
                height: 40,
            },
            // Left of the window: nothing of it is on the surface.
            SurfaceRect {
                x: 0,
                y: 0,
                width: 50,
                height: 20,
            },
        ]);
        let mut none = ChromeSurface::new("status", rect(0, 580, 800, 20));
        none.input = InputArea::Rects(Vec::new());
        let layout = ChromeLayout::new(rect(0, 0, 800, 580), vec![overlay, none]);
        let placing = targets.place(&layout, WINDOW);
        let expected = SurfaceRect {
            x: 0,
            y: 0,
            width: 90,
            height: 20,
        };
        assert_eq!(
            placing.surfaces[0].input,
            InputRegion::Rects(vec![expected])
        );
        assert_eq!(placing.surfaces[1].input, InputRegion::Rects(Vec::new()));
    }

    #[test]
    fn a_name_no_longer_listed_is_hidden_with_its_last_rectangle() {
        let mut targets = ChromeTargets::default();
        let (top, page) = (rect(0, 0, 800, 84), rect(0, 84, 800, 516));
        targets.place(&layout(&[("page", page), ("chrome", top)]), WINDOW);
        let later = rect(0, 0, 800, 40);
        let placing = targets.place(&layout(&[("chrome", later)]), WINDOW);
        assert_eq!(shown(&placing), [("page", false), ("chrome", true)]);
        assert_eq!(placing.surfaces[0].rect, page);
        // Clipped when the window shrinks, so the host never sees it outside.
        let small = LogicalSize {
            width: 500,
            height: 300,
        };
        let placing = targets.place(&layout(&[("chrome", later)]), small);
        assert_eq!(placing.surfaces[0].rect, rect(0, 84, 500, 216));
    }

    #[test]
    fn a_chrome_gets_its_own_commands_and_a_notice_says_what_it_lacks() {
        let kind = ChromeKind {
            commands: &["toggle-tabs", "zoom"],
            ..sighurt_ui::standard::KIND
        };
        let toggle = command(&kind, "toggle-tabs".into());
        assert_eq!(toggle, Ok(ChromeEvent::Command("toggle-tabs".into())));
        assert_eq!(
            command(&kind, "toggle-tab".into()),
            Err(
                "The standard chrome has no command chrome.toggle-tab; it has \
                 chrome.toggle-tabs, chrome.zoom."
                    .into()
            )
        );
        let none = ChromeKind {
            commands: &[],
            ..kind
        };
        let error = command(&none, "zoom".into()).unwrap_err();
        assert!(error.ends_with("chrome.zoom; it has none."), "{error}");
    }

    #[test]
    fn the_snapshot_contains_what_pages_say() {
        let engines = [("demo".to_owned(), EngineCapabilities::default())].into();
        let env = Environment {
            engines,
            ..Environment::default()
        };
        let config = "version = 1\n[routing.demo]\nschemes = [\"demo\"]";
        let config = Config::parse(Some(config), &env).unwrap();
        let mut core = BrowserState::new(config, env);
        let open = ["demo://pattern", "sig://home"].map(|input| OpenRequest {
            input: input.into(),
            engine: None,
        });
        let effects = update(
            &mut core,
            BrowserEvent::Start {
                session: None,
                open: open.into(),
            },
        );
        let (view, navigation) = effects
            .iter()
            .find_map(|effect| match effect {
                Effect::Engine {
                    view,
                    command: EngineCommand::Navigate { navigation, .. },
                } => Some((*view, *navigation)),
                _ => None,
            })
            .expect("the page is opened");
        // Neither a tab whose page hasn't committed yet nor sig://home names an engine.
        let engines = |core: &BrowserState| {
            let snapshot = snapshot(core, HomeInfo::default(), |_| None);
            let tabs = snapshot.tabs.into_iter();
            tabs.map(|tab| (tab.content, tab.engine))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            engines(&core),
            [(TabContent::Blank, None), (TabContent::Internal, None)]
        );
        let document = sighurt_api::ids::DocumentGeneration::next();
        let title = format!("a\u{202e}gpj.exe\nb{}", "x".repeat(300));
        for event in [
            EngineEvent::ViewReady { view },
            EngineEvent::NavigationCommitted {
                view,
                navigation,
                url: "demo://pattern".into(),
                document,
                history: None,
            },
            EngineEvent::TitleChanged {
                view,
                title: Some(title),
            },
        ] {
            let at = Instant::now();
            let engine = "demo".into();
            update(&mut core, BrowserEvent::Engine { engine, event, at });
        }
        let status = |_: &TabSummary<'_>| Some("\u{2066}hover\r\n".to_owned());
        let snapshot = snapshot(&core, HomeInfo::default(), status);
        let tab = &snapshot.tabs[0];
        assert_eq!(tab.content, TabContent::Engine);
        assert!(tab.title.starts_with("agpj.exe b"), "{:?}", tab.title);
        assert_eq!(tab.title.chars().count(), 200);
        assert_eq!(snapshot.status.as_deref(), Some("hover  "));
        // The engine page names its engine, by its configured name.
        assert_eq!(tab.engine.as_deref(), Some("demo"));
        assert_eq!(snapshot.tabs[1].engine, None);
    }

    /// Time of `snapshot` plus the chrome's equality check per hovered link, with 50 tabs whose
    /// addresses are 32 Ki characters long (bench/HYPOTHESES.md, H-SNAP1). `cargo test --release
    /// -- --ignored --nocapture snapshot_cost`; it prints microseconds per snapshot.
    #[test]
    #[ignore = "a timing, run by hand"]
    fn snapshot_cost_with_many_long_tabs() {
        let engines = [("demo".to_owned(), EngineCapabilities::default())].into();
        let env = Environment {
            engines,
            ..Environment::default()
        };
        let config = "version = 1\n[routing.demo]\nschemes = [\"demo\"]";
        let config = Config::parse(Some(config), &env).unwrap();
        let mut core = BrowserState::new(config, env);
        let open = (0..50).map(|n| OpenRequest {
            input: format!("demo://pattern{n}"),
            engine: None,
        });
        let effects = update(
            &mut core,
            BrowserEvent::Start {
                session: None,
                open: open.collect(),
            },
        );
        let long = format!("demo://pattern/{}", "a%20b".repeat(6500));
        for effect in &effects {
            let Effect::Engine {
                view,
                command: EngineCommand::Navigate { navigation, .. },
            } = effect
            else {
                continue;
            };
            let document = sighurt_api::ids::DocumentGeneration::next();
            for event in [
                EngineEvent::ViewReady { view: *view },
                EngineEvent::NavigationCommitted {
                    view: *view,
                    navigation: *navigation,
                    url: long.clone(),
                    document,
                    history: None,
                },
            ] {
                let at = Instant::now();
                let engine = "demo".into();
                update(&mut core, BrowserEvent::Engine { engine, event, at });
            }
        }
        let mut times = Vec::new();
        let mut previous = snapshot(&core, HomeInfo::default(), |_| None);
        assert_eq!(previous.tabs.len(), 50);
        for round in 0..21 {
            let status = move |_: &TabSummary<'_>| Some(format!("hover {round}"));
            let start = Instant::now();
            let next = snapshot(&core, HomeInfo::default(), status);
            let changed = next != previous;
            times.push(start.elapsed().as_micros());
            assert!(changed);
            previous = next;
        }
        times.sort_unstable();
        eprintln!(
            "snapshot_cost: median {} us, min {} us, max {} us",
            times[10], times[0], times[20]
        );
    }
}
