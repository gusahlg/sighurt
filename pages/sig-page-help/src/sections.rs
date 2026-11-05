//! What the help page says, section by section. The prose is here; every list that could go
//! stale (keys, commands, engines, paths, settings, versions) comes from [`Data`]. Owns
//! content only: no input, scrolling or drawing.

use crate::data::{Data, Slot, State};
use sig_page_kit::ui::{Builder, Column, Font};

/// A section of the page, selected by the URL's path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Start,
    Keys,
    Commands,
    Config,
    Pages,
    Engines,
    TerminalFrontend,
    About,
}

/// Every section, in the sidebar's order.
pub const ALL: [Section; 8] = [
    Section::Start,
    Section::Keys,
    Section::Commands,
    Section::Config,
    Section::Pages,
    Section::Engines,
    Section::TerminalFrontend,
    Section::About,
];

impl Section {
    /// Its place in [`ALL`].
    pub fn index(self) -> usize {
        ALL.iter().position(|&s| s == self).unwrap_or(0)
    }

    /// The last part of its URL; empty for the start.
    pub fn slug(self) -> &'static str {
        match self {
            Self::Start => "",
            Self::Keys => "keys",
            Self::Commands => "commands",
            Self::Config => "config",
            Self::Pages => "pages",
            Self::Engines => "engines",
            Self::TerminalFrontend => "terminal-frontend",
            Self::About => "about",
        }
    }

    /// Its name in the sidebar.
    pub fn label(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::Keys => "Keys",
            Self::Commands => "Commands",
            Self::Config => "Configuration",
            Self::Pages => "Pages",
            Self::Engines => "Engines",
            Self::TerminalFrontend => "Terminal",
            Self::About => "About",
        }
    }

    /// Its URL.
    pub fn url(self) -> String {
        match self.slug() {
            "" => "sig://help".into(),
            slug => format!("sig://help/{slug}"),
        }
    }

    /// The section a URL names; the start for anything else.
    pub fn from_url(url: &str) -> Self {
        let rest = url.strip_prefix("sig://help").unwrap_or("");
        let path = rest
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .trim_matches('/');
        ALL.into_iter()
            .find(|s| s.slug() == path)
            .unwrap_or(Self::Start)
    }
}

/// Lays `section` out.
pub fn build<M: FnMut(&Font, &str) -> f32>(b: &mut Builder<M>, section: Section, d: &Data) {
    b.heading(1, section.title());
    match section {
        Section::Start => start(b),
        Section::Keys => keys(b, d),
        Section::Commands => commands(b, d),
        Section::Config => config(b, d),
        Section::Pages => pages(b),
        Section::Engines => engines(b, d),
        Section::TerminalFrontend => terminal(b),
        Section::About => about(b, d),
    }
    b.gap(48.0);
}

impl Section {
    fn title(self) -> &'static str {
        match self {
            Self::Start => "Sighurt help",
            Self::Config => "The configuration",
            Self::TerminalFrontend => "The terminal frontend",
            Self::Pages => "Internal pages",
            other => other.label(),
        }
    }
}

const LOADING: &str = "Loading\u{2026}";

/// The rows of a slot with at least `min` fields, or a note saying why there are none.
fn rows<M: FnMut(&Font, &str) -> f32>(
    b: &mut Builder<M>,
    slot: &Slot,
    min: usize,
) -> Option<Vec<Vec<String>>> {
    match &slot.state {
        State::Loading => b.muted(LOADING),
        State::Failed(why) => b.muted(&format!("Not available: {why}")),
        State::Ready(rows) => {
            return Some(rows.iter().filter(|r| r.len() >= min).cloned().collect())
        }
    }
    None
}

fn pairs<M: FnMut(&Font, &str) -> f32>(b: &mut Builder<M>, slot: &Slot, left: &str, right: &str) {
    if let Some(rows) = rows(b, slot, 2) {
        let cols = [
            Column {
                title: left,
                frac: 0.34,
                mono: true,
            },
            Column {
                title: right,
                frac: 0.66,
                mono: true,
            },
        ];
        b.table(&cols, &rows);
    }
}

fn start<M: FnMut(&Font, &str) -> f32>(b: &mut Builder<M>) {
    b.para(
        "Sighurt (the program is called sig) is a small, configurable browser. Its web engines are \
         compiled in: Servo, WPE WebKit and a WebAssembly engine in a default build, and each page \
         opens in one of them. One binary has two frontends: a window on Wayland, and a terminal \
         frontend that runs inside your terminal.",
    );
    b.heading(2, "NORMAL and INSERT");
    b.para(
        "The default keymap has two modes. In NORMAL mode, which sig starts in, keys are commands \
         and none of them reaches the page: j and k scroll, t opens a tab, o opens the address \
         line. Escape switches to INSERT mode, where every key goes to the page, so you can type \
         into a form. Escape in INSERT returns to NORMAL.",
    );
    b.heading(2, "The ten keys to know");
    let keys: Vec<Vec<String>> = [
        ("o", "Open the address line"),
        (":", "Open the command line"),
        ("t", "New tab"),
        ("x or d", "Close the tab"),
        ("J / K", "Next / previous tab"),
        ("H / L", "Back / forward"),
        ("j / k", "Scroll down / up"),
        (
            "/",
            "Find in the page (n and N for the next and previous match)",
        ),
        ("? or F1", "This help (the configured command help)"),
        ("q", "Quit"),
        ("Escape", "Switch between NORMAL and INSERT"),
    ]
    .iter()
    .map(|(k, v)| vec![k.to_string(), v.to_string()])
    .collect();
    let cols = [
        Column {
            title: "Key",
            frac: 0.22,
            mono: true,
        },
        Column {
            title: "Does",
            frac: 0.78,
            mono: false,
        },
    ];
    b.table(&cols, &keys);
    b.para("These are the example keymap's bindings, which are your keys unless you chose another keymap. The Keys section lists every binding of the keymap you are running.");
    b.link("All keys \u{2192}", "sig://help/keys");
    b.link("All commands \u{2192}", "sig://help/commands");
    b.link("The configuration \u{2192}", "sig://help/config");
}

fn keys<M: FnMut(&Font, &str) -> f32>(b: &mut Builder<M>, d: &Data) {
    let keymap = d.about.value("keymap").unwrap_or("?");
    let frontend = d.about.value("frontend").unwrap_or("?");
    b.para(&format!(
        "The bindings of the {keymap} keymap in this {frontend} frontend, as the keymap reports \
         them. A key sequence such as y y is typed one key after the other. Applies says where \
         a binding works (for example only when a page, not a text field, has the keyboard)."
    ));
    let Some(rows) = rows(b, &d.keys, 3) else {
        return;
    };
    let mut modes: Vec<&str> = Vec::new();
    for r in &rows {
        if !modes.contains(&r[0].as_str()) {
            modes.push(&r[0]);
        }
    }
    let cols = [
        Column {
            title: "Keys",
            frac: 0.25,
            mono: true,
        },
        Column {
            title: "Action",
            frac: 0.37,
            mono: true,
        },
        Column {
            title: "Applies",
            frac: 0.38,
            mono: false,
        },
    ];
    for mode in modes {
        b.heading(2, &format!("{mode} mode"));
        let table: Vec<Vec<String>> = rows
            .iter()
            .filter(|r| r[0] == mode)
            .map(|r| {
                vec![
                    r[1].clone(),
                    r[2].clone(),
                    r.get(3).cloned().unwrap_or_default(),
                ]
            })
            .collect();
        b.table(&cols, &table);
    }
}

fn commands<M: FnMut(&Font, &str) -> f32>(b: &mut Builder<M>, d: &Data) {
    b.para(
        "Everything sig does is a named command, <area>.<action>, with an optional argument. Keys, \
         buttons and the command line (:) all run the same commands. Type : and a command to run it.",
    );
    b.heading(2, "Built-in commands");
    if let Some(rows) = rows(b, &d.commands, 3) {
        let cols = [
            Column {
                title: "Command",
                frac: 0.25,
                mono: true,
            },
            Column {
                title: "Argument",
                frac: 0.13,
                mono: true,
            },
            Column {
                title: "Does",
                frac: 0.62,
                mono: false,
            },
        ];
        b.table(&cols, &rows);
    }
    b.heading(2, "Your own commands");
    b.para(
        "The [commands] table of config.toml defines commands of your own: a bare lower-case word \
         that runs up to 8 built-in commands, in order. The built-in configuration has help = \
         \"tab.new sig://help\", which is what ? and F1 run. They take no argument, and a word \
         [commands] doesn't define is an unknown command.",
    );
    b.code("[commands]\nhelp = \"tab.new sig://help\"\nwork = [\"tab.new https://example.org\", \"tab.new https://example.net\"]");
    if let Some(rows) = rows(b, &d.user_commands, 2) {
        if rows.is_empty() {
            b.muted("No commands of your own are defined.");
        } else {
            let cols = [
                Column {
                    title: "Command",
                    frac: 0.25,
                    mono: true,
                },
                Column {
                    title: "Runs",
                    frac: 0.75,
                    mono: true,
                },
            ];
            b.table(&cols, &rows);
        }
    }
}

fn config<M: FnMut(&Font, &str) -> f32>(b: &mut Builder<M>, d: &Data) {
    let path = match &d.paths.state {
        State::Ready(rows) => rows
            .iter()
            .find(|r| r[0] == "config" && r.len() > 1)
            .map(|r| r[1].clone()),
        _ => None,
    };
    b.para("sig is configured by one TOML file. It is merged over the built-in defaults, so it holds only what you change.");
    match path {
        Some(path) => {
            b.para("Your configuration file is:");
            b.code(&path);
        }
        None => b.muted(match &d.paths.state {
            State::Loading => LOADING,
            _ => "The location of the configuration file is not available.",
        }),
    }
    b.para(
        "It starts with version = 1. sig --default-config prints the built-in defaults, every \
         setting documented in comments: copy what you want to change. Most settings apply as soon \
         as you save the file; a mistake keeps the previous configuration and is reported. \
         [ui] chrome, [ui] renderer and the [engines.<name>] settings need a restart.",
    );
    b.para(
        "You can also edit the file here, at sig://config: it is checked as you type and the \
         problems are listed with their lines. With the example keymap, press Escape for INSERT \
         mode to type; Ctrl+S (or Save) saves and applies it. A save is refused when the text has \
         a problem, or when the file changed elsewhere since the page read it (Read the file \
         again, then Ctrl+Z brings your text back). A file linked from elsewhere is saved where \
         the link leads.",
    );
    b.link("Open sig://config \u{2192}", "sig://config");
    b.heading(2, "The tables");
    let tables: Vec<Vec<String>> = [
        (
            "[engines]",
            "Which engines may open pages, the default one, and each engine's own settings",
        ),
        (
            "[routing.<engine>]",
            "Rules sending addresses to an engine by scheme, extension, origin or host",
        ),
        ("[input]", "The keymap, and what mouse buttons do"),
        ("[keys]", "Key bindings, for the bindings keymap"),
        ("[ui]", "The chrome and the renderer"),
        ("[commands]", "Commands of your own"),
        ("[pages]", "The internal pages, such as the terminal page"),
        (
            "[tui]",
            "The terminal frontend: text or graphics, frame rate, mouse",
        ),
        ("[keywords]", "Search keywords for the address line"),
        ("[downloads]", "Where downloads go"),
        ("[session]", "Restoring tabs at start"),
        ("[history]", "Remembering visited pages"),
    ]
    .iter()
    .map(|(k, v)| vec![k.to_string(), v.to_string()])
    .collect();
    let cols = [
        Column {
            title: "Table",
            frac: 0.28,
            mono: true,
        },
        Column {
            title: "Holds",
            frac: 0.72,
            mono: false,
        },
    ];
    b.table(&cols, &tables);
    b.heading(2, "Effective values");
    b.para("What these settings are now, after your file and the defaults are merged:");
    pairs(b, &d.settings, "Setting", "Value");
    b.heading(2, "Where sig keeps things");
    pairs(b, &d.paths, "Name", "Path");
    b.link(
        "Edit the configuration in the browser \u{2192}",
        "sig://config",
    );
}

fn pages<M: FnMut(&Font, &str) -> f32>(b: &mut Builder<M>) {
    b.para("sig has pages of its own. They are small programs built into sig that run in the WebAssembly engine, so they need that engine in the build and enabled. Only you open them (from the address line, a command or the session); a web page can't.");
    b.heading(2, "sig://help");
    b.para("This page.");
    b.link("sig://help", "sig://help");
    b.heading(2, "sig://config");
    b.para("View and edit the configuration file in the browser: a plain text editor that checks the file as you type. Ctrl+S or Save saves it, only when it is valid and nobody changed the file meanwhile, and the change applies live where the setting allows it.");
    b.link("sig://config", "sig://config");
    b.heading(2, "sig://terminal");
    b.para(
        "A terminal page. It is off by default: set enabled = true in the [pages.terminal] table to turn it on. \
         It runs your shell with your full rights, like any terminal. The WebAssembly sandbox contains \
         the terminal emulator, not the shell.",
    );
    b.code("[pages.terminal]\nenabled = true");
    b.link("sig://terminal", "sig://terminal");
    b.heading(2, "sig://home");
    b.para("The home page, drawn by the chrome. The home setting chooses what new tabs open.");
    b.link("sig://home", "sig://home");
}

fn engines<M: FnMut(&Font, &str) -> f32>(b: &mut Builder<M>, d: &Data) {
    b.para("The engines compiled into this build:");
    if let Some(rows) = rows(b, &d.engines, 4) {
        let cols = [
            Column {
                title: "Engine",
                frac: 0.14,
                mono: true,
            },
            Column {
                title: "Enabled",
                frac: 0.12,
                mono: false,
            },
            Column {
                title: "Default",
                frac: 0.12,
                mono: false,
            },
            Column {
                title: "Web",
                frac: 0.1,
                mono: false,
            },
            Column {
                title: "Capabilities",
                frac: 0.52,
                mono: true,
            },
        ];
        b.table(&cols, &rows);
    }
    b.heading(2, "Which engine opens a page");
    b.para("Servo is the default engine. Rules decide otherwise: a [routing.<engine>] table sends addresses to that engine by scheme, file extension, origin or host. Wasm applications, for example, go to the Wasm engine.");
    b.code("[routing.wpe]\nhosts = [\"*.google.com\", \"google.com\"]");
    b.para("To open one page in another engine, run page.open-in <engine> [url]. To move the current page, press e, or run page.switch-engine: it cycles through the enabled web engines. Pages already open stay in the engine they are in when the configuration changes.");
    b.para("sig --default-config documents the routing rules in full.");
}

fn terminal<M: FnMut(&Font, &str) -> f32>(b: &mut Builder<M>) {
    b.para(
        "When stdout is a terminal (and TERM isn't dumb), sig runs inside it; sig --tui forces it, \
         sig --gui forces the window. The terminal shows the browser's chrome as two lines of text and the page between them in one of two modes.",
    );
    b.heading(2, "Graphics and text");
    b.bullet("Graphics shows the page's pixels as kitty graphics (kitty, Rio, ghostty, WezTerm), or as sixel where the terminal has sixel but not kitty graphics (tmux 3.4 or later, foot, xterm with sixel).");
    b.bullet("Text reflows the page's content into terminal lines, with links and controls you move to with Tab and Shift-Tab and activate with Enter. It works in any terminal, and needs an engine that gives page text: Servo, WPE WebKit and the demo engine do; the Wasm engine, CEF, Ladybird and NetSurf do not.");
    b.para("m switches between them. [tui] mode = \"auto\", \"text\" or \"graphics\" chooses the start; auto uses text where the terminal can't show images. sig:// pages are WebAssembly applications with no page text, so in a terminal they need graphics.");
    b.heading(2, "Printing a page");
    b.code("sig --dump https://example.com --width 100");
    b.para("sig --dump URL prints the page as reflowed text with numbered links and exits, with no terminal or window.");
    b.heading(2, "tmux and Rio");
    b.para("Inside tmux, images are sixel and need tmux 3.4 or later. Rio 0.5.28 can lose its GPU device, and exit with everything running in it, when an image it shows is erased or redrawn; tmux redrawing the pane does that. Until Rio fixes it, its CPU renderer avoids the crash. In ~/.config/rio/config.toml:");
    b.code("[renderer]\nuse-cpu = true");
}

fn about<M: FnMut(&Font, &str) -> f32>(b: &mut Builder<M>, d: &Data) {
    let version = d.about.value("version").unwrap_or("unknown");
    let license = d.about.value("license").unwrap_or("AGPL-3.0-or-later");
    b.para(&format!("Sighurt {version}, licensed {license}."));
    b.para("Sighurt is a fork of Oxide by Nikhil Ranjan (github.com/niklabh/oxide, Apache-2.0). The attribution is kept in LICENSE, LICENSE-APACHE and NOTICE.");
    b.link(
        "github.com/niklabh/oxide",
        "https://github.com/niklabh/oxide",
    );
    b.para("sighurt-sdk and the examples stay Apache-2.0.");
    b.heading(2, "This build");
    pairs(b, &d.about, "Fact", "Value");
    b.heading(2, "Files");
    pairs(b, &d.paths, "Name", "Path");
}
