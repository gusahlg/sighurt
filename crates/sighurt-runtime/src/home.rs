//! What `sig://home` says, for every frontend: the build's version and configuration file, the
//! internal pages this build and configuration can open, a few keys of the keymap in use, the
//! pages visited last and the engines.
//!
//! Built only from the browser itself (the core's configuration and environment, the keymap's
//! `describe`, the history, the registry's engines), so it can't go stale; every part is bounded
//! in count and length ([`MAX_PAGES`], [`MAX_KEYS`], [`MAX_RECENT`] and the `MAX_*` lengths).
//! [`crate::session::Session::home`] keeps the last one built and builds again only when its
//! inputs changed.
//!
//! Must not own: how a frontend lays it out, contains or draws it (the chromes, the terminal's
//! `home`), or what a link does (a frontend runs `page.open <url>`).

use std::path::Path;

use sighurt_api::keymap::KeyBinding;
use sighurt_core::router::PAGE_ENGINE;
use sighurt_core::BrowserState;

use crate::history::Completion;

/// The most internal pages listed.
pub const MAX_PAGES: usize = 8;
/// The most keys listed.
pub const MAX_KEYS: usize = 12;
/// The most visited pages listed.
pub const MAX_RECENT: usize = 8;
/// The most characters of a link's title or a key's description.
pub const MAX_TITLE: usize = 200;
/// The most characters of a detail: an engine's line, a page's.
pub const MAX_DETAIL: usize = 500;
/// The most characters of a key's keystrokes, all of them together.
pub const MAX_KEYSTROKES: usize = 64;
/// The most characters of the configuration file's path.
pub const MAX_PATH: usize = 1000;
/// The most bytes of a link's address: the longest the history records.
pub const MAX_URL: usize = sighurt_core::state::MAX_VISIT_URL;
/// The most keystrokes listed for one command.
const KEYS_PER_COMMAND: usize = 3;

/// The commands whose keys home lists, in this order, and what each is called there. `help`
/// is the configuration's own command of that name, listed while `[commands]` has it.
const COMMANDS: [(&str, &str); 10] = [
    ("location.focus", "Address or search"),
    ("tab.new", "New tab"),
    ("tab.close", "Close the tab"),
    ("tab.next", "Next tab"),
    ("tab.previous", "Previous tab"),
    ("page.back", "Back"),
    ("page.forward", "Forward"),
    ("page.find", "Find in the page"),
    ("help", "Help"),
    ("browser.quit", "Quit sig"),
];

/// What `sig://home` says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Home {
    /// The build's version.
    pub version: String,
    /// The configuration file, or empty without a configuration directory.
    pub config_path: String,
    /// The internal pages that open here, as links; empty when none does.
    pub pages: Vec<Link>,
    /// Keys of the keymap in use; empty when it lists none of [`COMMANDS`].
    pub keys: Vec<Key>,
    /// The pages visited last, the newest first; empty with `[history] remember` off.
    pub recent: Vec<Link>,
    /// The engines of this build, in the registry's order.
    pub engines: Vec<EngineLine>,
}

/// A link on the home page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Link {
    /// What it opens, exactly; at most [`MAX_URL`] bytes.
    pub url: String,
    /// What it is called; for a visited page its title, which may be empty. At most
    /// [`MAX_TITLE`] characters.
    pub title: String,
    /// One line about it, or empty; at most [`MAX_DETAIL`] characters.
    pub detail: String,
}

/// A key of the keymap and what it does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Key {
    /// Its keystrokes as the configuration writes them, several separated by ", ".
    pub keys: String,
    /// What it does, in a few words.
    pub action: String,
}

/// One engine on the home page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineLine {
    /// Its name in the configuration.
    pub name: String,
    /// "compiled in, <renderer> · default", "disabled in the configuration", why it failed.
    pub detail: String,
    /// Whether it can open pages.
    pub available: bool,
}

/// What the registry knows of an engine, for [`build`].
pub struct EngineFact<'a> {
    /// Its name.
    pub name: &'a str,
    /// How it renders.
    pub renderer: &'a str,
    /// Why it failed, if it did.
    pub failed: Option<&'a str>,
}

/// What home says for the browser `core`, with the configuration file at `config`, `engines`,
/// and the pages visited last, `recent` (the history's, newest first).
pub fn build<'a>(
    core: &BrowserState,
    config: Option<&Path>,
    engines: impl IntoIterator<Item = EngineFact<'a>>,
    recent: Vec<Completion>,
) -> Home {
    let config_path = config.map_or_else(String::new, |path| path.display().to_string());
    Home {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        config_path: cut(&config_path, MAX_PATH),
        pages: pages(core),
        keys: keys(core),
        recent: recent
            .into_iter()
            .filter(|visit| visit.url.len() <= MAX_URL)
            .take(MAX_RECENT)
            .map(|visit| Link {
                title: cut(&visit.title, MAX_TITLE),
                url: visit.url,
                detail: String::new(),
            })
            .collect(),
        engines: engine_lines(core, engines),
    }
}

/// The internal pages that open: those of the build, while their engine is enabled.
fn pages(core: &BrowserState) -> Vec<Link> {
    let config = core.config();
    if !config.is_enabled(PAGE_ENGINE) {
        return Vec::new();
    }
    let link = |url: &str, title: &str, detail: &str| Link {
        url: url.to_owned(),
        title: title.to_owned(),
        detail: cut(detail, MAX_DETAIL),
    };
    let mut found = Vec::new();
    for name in config.internal_pages() {
        match name.as_str() {
            "help" => found.extend([
                link(
                    "sig://help",
                    "Help",
                    "Getting started, the keys, the commands, the configuration",
                ),
                link("sig://help/keys", "Keys", "Every key of the keymap in use"),
                link(
                    "sig://help/commands",
                    "Commands",
                    "Every command, and your own",
                ),
            ]),
            "config" => found.push(link(
                "sig://config",
                "Configuration",
                match config.pages().config {
                    true => "Edit the configuration file in your editor",
                    false => "Off: [pages.config] enabled = true turns it on",
                },
            )),
            "terminal" => found.push(link(
                "sig://terminal",
                "Terminal",
                match config.pages().terminal {
                    true => "A shell in a page, with your rights",
                    false => "Off: [pages.terminal] enabled = true turns it on",
                },
            )),
            name => found.push(link(&format!("sig://{name}"), name, "")),
        }
    }
    // The help page's first, as a reader looks for it.
    found.sort_by_key(|page| !page.url.starts_with("sig://help"));
    found.truncate(MAX_PAGES);
    found
}

/// Keys of the keymap in use for [`COMMANDS`], in its first mode (all of them for a keymap
/// without modes), and how to enter the mode its first mode switches to and leave it again.
fn keys(core: &BrowserState) -> Vec<Key> {
    let bindings = core.describe_keys();
    let Some(first) = bindings.first().map(|binding| binding.mode.clone()) else {
        return Vec::new();
    };
    let in_first = || bindings.iter().filter(|binding| binding.mode == first);
    let mut found = Vec::new();
    for (index, (command, action)) in COMMANDS.into_iter().enumerate() {
        if command == "help" && core.config().command("help").is_none() {
            continue;
        }
        let keys = keystrokes(in_first().filter(|binding| binding.action == command));
        if !keys.is_empty() {
            found.push(key(&keys, action));
        }
        // The other mode right after the address line's keys.
        if index == 0 {
            found.extend(other_mode(&bindings, &first));
        }
    }
    found.truncate(MAX_KEYS);
    found
}

/// The keys entering the first mode `first`'s bindings switch to ("INSERT mode"), and the
/// first that switches back from it.
fn other_mode(bindings: &[KeyBinding], first: &str) -> Vec<Key> {
    let mut modes: Vec<&str> = Vec::new();
    for binding in bindings {
        if !modes.contains(&binding.mode.as_str()) {
            modes.push(&binding.mode);
        }
    }
    let switches = |binding: &KeyBinding, to: &str| {
        !to.is_empty() && binding.action.starts_with(&format!("{to} mode"))
    };
    for other in modes.into_iter().filter(|mode| *mode != first) {
        let enter = bindings
            .iter()
            .find(|binding| binding.mode == first && switches(binding, other));
        let Some(enter) = enter else {
            continue;
        };
        let mut found = vec![key(&enter.keys, &format!("{other} mode"))];
        let leave = bindings
            .iter()
            .find(|binding| binding.mode == other && switches(binding, first));
        if let Some(leave) = leave {
            found.push(key(&leave.keys, &format!("Back to {first} mode")));
        }
        return found;
    }
    Vec::new()
}

/// At most [`KEYS_PER_COMMAND`] different keystrokes of `bindings`, joined.
fn keystrokes<'a>(bindings: impl Iterator<Item = &'a KeyBinding>) -> String {
    let mut keys: Vec<&str> = Vec::new();
    for binding in bindings {
        if !keys.contains(&binding.keys.as_str()) && keys.len() < KEYS_PER_COMMAND {
            keys.push(&binding.keys);
        }
    }
    keys.join(", ")
}

fn key(keys: &str, action: &str) -> Key {
    Key {
        keys: cut(keys, MAX_KEYSTROKES),
        action: cut(action, MAX_TITLE),
    }
}

/// The engines' lines: compiled in, how they render, default, disabled, failed.
fn engine_lines<'a>(
    core: &BrowserState,
    engines: impl IntoIterator<Item = EngineFact<'a>>,
) -> Vec<EngineLine> {
    let config = core.config();
    engines
        .into_iter()
        .map(|engine| {
            let mut detail = vec![format!("compiled in, {}", engine.renderer)];
            if engine.name == config.default_engine() {
                detail.push("default".into());
            }
            let enabled = config.is_enabled(engine.name);
            if !enabled {
                detail.push("disabled in the configuration".into());
            }
            if let Some(why) = engine.failed {
                detail.push(format!("failed: {why}"));
            }
            EngineLine {
                name: cut(engine.name, MAX_TITLE),
                detail: cut(&detail.join(" · "), MAX_DETAIL),
                available: enabled && engine.failed.is_none(),
            }
        })
        .collect()
}

/// `text`, or its first `max - 1` characters and an ellipsis when it has more than `max`.
fn cut(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_owned(),
        Some(_) => {
            let end = text
                .char_indices()
                .nth(max.saturating_sub(1))
                .map_or(text.len(), |(at, _)| at);
            format!("{}…", &text[..end])
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use sighurt_api::engine::EngineCapabilities;
    use sighurt_api::keymap::Frontend;
    use sighurt_api::services::PageGrant;
    use sighurt_core::config::Config;
    use sighurt_core::Environment;

    use super::*;

    const ALL_PAGES: [&str; 3] = ["help", "config", "terminal"];

    fn env(engines: &[&str], pages: &[&str]) -> Environment {
        let grants: &'static [PageGrant] = &[];
        Environment {
            engines: engines
                .iter()
                .map(|e| ((*e).to_owned(), EngineCapabilities::default()))
                .collect(),
            pages: pages
                .iter()
                .map(|p| ((*p).to_owned(), grants))
                .collect::<BTreeMap<_, _>>(),
            keymaps: vec![sighurt_config_example::KIND],
            frontend: Frontend::Terminal,
            ..Environment::default()
        }
    }

    fn core_with(env: Environment, config: &str) -> BrowserState {
        BrowserState::new(Config::parse(Some(config), &env).unwrap(), env)
    }

    fn home(core: &BrowserState) -> Home {
        let engines = core.environment().engines.keys().map(|name| EngineFact {
            name,
            renderer: "CPU",
            failed: (name == "demo").then_some("no display"),
        });
        build(
            core,
            Some(Path::new("/c/sighurt/config.toml")),
            engines,
            Vec::new(),
        )
    }

    fn urls(links: &[Link]) -> Vec<&str> {
        links.iter().map(|link| link.url.as_str()).collect()
    }

    #[test]
    fn pages_are_listed_only_when_they_open() {
        let core = core_with(env(&["servo", "wasm"], &ALL_PAGES), "version = 1");
        let found = home(&core);
        assert_eq!(
            urls(&found.pages),
            [
                "sig://help",
                "sig://help/keys",
                "sig://help/commands",
                "sig://config",
                "sig://terminal"
            ]
        );
        assert_eq!(found.pages[0].title, "Help");
        assert_eq!(found.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(found.config_path, "/c/sighurt/config.toml");
        // A build without the pages' engine has none.
        let core = core_with(env(&["servo"], &[]), "version = 1");
        assert!(home(&core).pages.is_empty());
        // The engine left out of [engines] enabled: none opens.
        let core = core_with(
            env(&["servo", "wasm"], &ALL_PAGES),
            "version = 1\n[engines]\nenabled = [\"servo\"]\n",
        );
        assert!(home(&core).pages.is_empty());
        // A build with only some of the guests lists only those.
        let core = core_with(env(&["servo", "wasm"], &["config"]), "version = 1");
        assert_eq!(urls(&home(&core).pages), ["sig://config"]);
    }

    #[test]
    fn the_terminal_says_it_is_off_and_names_the_setting() {
        let env = || env(&["servo", "wasm"], &ALL_PAGES);
        let off = home(&core_with(env(), "version = 1"));
        let terminal = off
            .pages
            .iter()
            .find(|p| p.url == "sig://terminal")
            .unwrap();
        assert!(terminal.detail.starts_with("Off"), "{terminal:?}");
        assert!(terminal.detail.contains("[pages.terminal] enabled = true"));
        let on = home(&core_with(
            env(),
            "version = 1\n[pages.terminal]\nenabled = true\n",
        ));
        let terminal = on.pages.iter().find(|p| p.url == "sig://terminal").unwrap();
        assert!(!terminal.detail.contains("Off"), "{terminal:?}");
    }

    #[test]
    fn keys_come_from_the_example_keymaps_description() {
        let core = core_with(env(&["servo", "wasm"], &ALL_PAGES), "version = 1");
        let keys = home(&core).keys;
        let of = |action: &str| {
            keys.iter()
                .find(|key| key.action == action)
                .map(|key| key.keys.as_str())
        };
        // What sighurt-config-example's table binds in NORMAL, nothing written here.
        assert_eq!(of("Address or search"), Some("o, ctrl-o"));
        assert_eq!(of("INSERT mode"), Some("escape"));
        assert_eq!(of("Back to NORMAL mode"), Some("escape"));
        assert_eq!(of("Close the tab"), Some("x, d"));
        assert_eq!(of("Help"), Some("?, f1"));
        assert_eq!(of("Quit sig"), Some("q, ctrl-c"));
        assert_eq!(keys[1].action, "INSERT mode", "{keys:?}");
        assert!(keys.len() <= MAX_KEYS);
    }

    #[test]
    fn keys_come_from_the_bindings_keymaps_description() {
        let core = core_with(
            env(&["servo"], &[]),
            "version = 1\n[input]\nkeymap = \"bindings\"\n[keys]\n\"ctrl-t\" = \"tab.new\"\n\
             \"ctrl-w\" = \"tab.close\"\n\"alt-left\" = \"page.back\"\n\"ctrl-l\" = \"location.focus\"\n\
             \"f1\" = \"help\"\n",
        );
        let keys = home(&core).keys;
        let pairs: Vec<(&str, &str)> = keys
            .iter()
            .map(|key| (key.keys.as_str(), key.action.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("ctrl-l", "Address or search"),
                ("ctrl-t", "New tab"),
                ("ctrl-w", "Close the tab"),
                ("alt-left", "Back"),
                ("f1", "Help"),
            ]
        );
        // A keymap that binds none of them: no keys.
        let core = core_with(
            env(&["servo"], &[]),
            "version = 1\n[input]\nkeymap = \"bindings\"\n[keys]\n\"f5\" = \"page.reload\"\n",
        );
        assert!(home(&core).keys.is_empty());
    }

    #[test]
    fn recent_pages_are_bounded_and_engines_say_what_they_are() {
        let core = core_with(env(&["demo", "servo"], &[]), "version = 1");
        let visits = (0..20).map(|i| Completion {
            url: format!("https://a.test/{i}"),
            title: "t".repeat(500),
        });
        let mut visits: Vec<Completion> = visits.collect();
        visits.push(Completion {
            url: format!("https://a.test/{}", "x".repeat(MAX_URL)),
            title: String::new(),
        });
        visits.rotate_right(1);
        let found = build(&core, None, [], visits);
        assert_eq!(found.recent.len(), MAX_RECENT);
        assert_eq!(
            found.recent[0].url, "https://a.test/0",
            "too long is left out"
        );
        assert_eq!(found.recent[0].title.chars().count(), MAX_TITLE);
        assert!(found.recent[0].title.ends_with('…'));
        assert!(found.config_path.is_empty());
        let engines = home(&core).engines;
        assert_eq!(engines[0].name, "demo");
        assert!(!engines[0].available);
        assert!(engines[0].detail.contains("failed: no display"));
        assert_eq!(engines[1].detail, "compiled in, CPU · default");
        assert!(engines[1].available);
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(cut("abcd", 4), "abcd");
    }
}
