//! `example`: the example configuration. A NORMAL and an INSERT mode with the terminal's keys in
//! both frontends (`table`), and three actions computed from the browser's state (`actions`),
//! written in Rust against `sighurt_api::keymap` (docs/rewrite/config-extension.md, §3). Select it
//! with `[input] keymap = "example"`; copy the crate and edit the tables to make your own.
//!
//! - **NORMAL**, the mode it starts in: while the page has the keyboard, keys are commands and
//!   none reaches the page, whether or not a text field of it has the keyboard. A bound key runs
//!   its command (the arrows, PageUp, PageDown, Home, End, Space, Tab, Enter and their shifted
//!   forms send that key to the page, to scroll and to move between and follow links); an
//!   unbound key, a modifier and a dead key are taken and do nothing. Space and Enter, which
//!   would type or submit, are taken too while a text field has the keyboard
//!   ([`Scope::NoField`]). No text reaches the page either ([`Keymap::page_takes_text`]): no
//!   input method, no paste. The indicator is empty, or the keys of an unfinished sequence.
//! - **INSERT**: every key reaches the page, and no binding listens, the `Always` ones neither;
//!   the one key taken is an unmodified Escape. Text and pastes reach the page. The indicator is
//!   `INSERT MODE`.
//! - **Sites** ([`table::SITES`]): on an address a site's pattern matches, a table the site
//!   gives for a mode replaces that mode's general table while the page has the keyboard, so
//!   a site can take Escape away from INSERT and leave it on other keys. The address is the
//!   shown tab's committed one ([`TabInfo::url`]), never one loading or a link; the most
//!   specific matching pattern wins ([`UrlPattern`]). No address is named here: the sites are
//!   data in `table`.
//! - **Escape** switches mode (in the general tables), except while the page shown holds an element fullscreen (a
//!   video): then it runs `page.exit-fullscreen` and the mode stays.
//! - **The address line and the find field.** The modes are the page's: while the chrome has
//!   the keyboard, in either mode, only NORMAL's `Always` bindings whose first key types nothing
//!   apply, and every other key, Escape included, is the line's. The mode stays.
//! - **The mouse** is not typing: clicks reach the page in both modes and change no mode.
//! - **Sequences** are at most 4 keys, no binding the start of another, and never wait for a
//!   timeout. Modifier and dead keys are not keys of a sequence; a key that doesn't continue
//!   one drops it and starts afresh; `context` and `reset` drop it.
//! - **Repeats** run a single-key `Run` again; anything else taken is held quietly.
//!
//! [`TabInfo::url`]: sighurt_api::keymap::TabInfo::url
//!
//! Must not own: the presses taken (the core's) or what a command does. It keeps no more of what
//! the user types than an unfinished sequence, and logs nothing.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod actions;
pub mod table;

use sighurt_api::commands::{Command, CommandSet};
use sighurt_api::keymap::{
    self as keymap, Browser, Custom, Fallback, Frontend, KeySeq, KeyTable, KeymapKind, Keys,
    Outcome, When,
};
use sighurt_api::settings::Table;
use sighurt_core::keys::Configured;

pub use sighurt_api::keymap::MAX_KEYS;
pub use table::{Action, Binding, Mode, Scope, Site, Where};

/// The example configuration, for the registry: `[input] keymap = "example"`.
pub const KIND: KeymapKind = KeymapKind {
    name: "example",
    check,
    new: || Box::new(Configured::new(keys())),
};

fn check(table: &Table, commands: &CommandSet<'_>) -> Vec<String> {
    let settings = table.keys().map(|key| {
        format!(
            "[keymap.example] takes no settings; the example configuration's bindings are in \
             its crate (not {key:?})"
        )
    });
    // Its commands of `[commands]` (help) must be the configuration's.
    let actions = tables().flat_map(|(_, _, table)| table);
    let commands = actions.filter_map(|binding| match binding.action {
        Action::Run(text) | Action::RunThenPass(text)
            if matches!(CommandSet::ANY.parse(text), Ok(Command::Custom(_))) =>
        {
            commands.parse(text).err().map(|e| {
                format!(
                    "the example configuration binds {} to {text}: {e}",
                    binding.keys
                )
            })
        }
        _ => None,
    });
    settings.chain(commands).collect()
}

/// Every table, as `(mode, site, bindings)`: the general NORMAL and INSERT (no site), then each
/// site's tables in [`table::SITES`] order.
pub fn tables() -> impl Iterator<Item = (Mode, Option<&'static str>, &'static [Binding])> {
    let general = [
        (Mode::Normal, None, table::NORMAL),
        (Mode::Insert, None, table::INSERT),
    ];
    let sites = table::SITES.iter().flat_map(|site| {
        [(Mode::Normal, site.normal), (Mode::Insert, site.insert)]
            .into_iter()
            .filter_map(move |(mode, table)| Some((mode, Some(site.url), table?)))
    });
    general.into_iter().chain(sites)
}

/// The bindings that exist in `frontend`, in every table, as `(keys, action)`; a site's
/// bindings once per site.
pub fn bindings(frontend: Frontend) -> impl Iterator<Item = (&'static str, &'static Action)> {
    tables()
        .flat_map(|(_, _, table)| table)
        .filter(move |binding| exists_in(binding.frontend, frontend))
        .map(|binding| (binding.keys, &binding.action))
}

fn exists_in(wanted: Where, frontend: Frontend) -> bool {
    matches!(
        (wanted, frontend),
        (Where::Both, _) | (Where::Terminal, Frontend::Terminal) | (Where::Window, Frontend::Gui)
    )
}

/// The name of `mode` in [`keys`].
fn name(mode: Mode) -> &'static str {
    match mode {
        Mode::Normal => "normal",
        Mode::Insert => "insert",
    }
}

/// The tables as the key engine's [`Keys`]: `normal` (the initial mode: no page text, and a
/// key no binding has is taken) and `insert` (shows `INSERT MODE`). A site's table replaces
/// its mode's general one there, so what the general one binds and the site's doesn't is
/// unbound on the site. A bad built-in key or command is a bug of this crate; the tests parse
/// every entry.
pub fn keys() -> Keys {
    let normal = keymap::Mode::new(name(Mode::Normal))
        .page_text(false)
        .unmatched(Fallback::Take);
    let insert = keymap::Mode::new(name(Mode::Insert)).indicator("INSERT MODE");
    let mut keys = Keys {
        modes: vec![normal, insert],
        initial: name(Mode::Normal).into(),
        line: Vec::new(),
    };
    for (mode, table) in [(Mode::Normal, table::NORMAL), (Mode::Insert, table::INSERT)] {
        for binding in table {
            add(&mut keys, mode, None, binding);
        }
    }
    for site in table::SITES {
        let url: keymap::UrlPattern = site
            .url
            .parse()
            .unwrap_or_else(|e| panic!("built-in site {:?}: {e}", site.url));
        let general = [(Mode::Normal, table::NORMAL), (Mode::Insert, table::INSERT)];
        for ((mode, general), table) in general.into_iter().zip([site.normal, site.insert]) {
            let Some(table) = table else {
                continue;
            };
            for binding in general
                .iter()
                .filter(|b| !table.iter().any(|t| t.keys == b.keys))
            {
                let when = When {
                    url: Some(url.clone()),
                    ..When::default()
                };
                let _ = keys.unbind(KeyTable::Mode(name(mode)), sequence(binding), when);
            }
            for binding in table {
                add(&mut keys, mode, Some(&url), binding);
            }
        }
    }
    keys
}

fn sequence(binding: &Binding) -> KeySeq {
    binding
        .keys
        .parse()
        .unwrap_or_else(|e| panic!("built-in keys {:?}: {e}", binding.keys))
}

/// Binds `binding` of `mode`'s table, on `url` if given; one that applies `Always` and whose
/// first key types nothing also in the line.
fn add(keys: &mut Keys, mode: Mode, url: Option<&keymap::UrlPattern>, binding: &Binding) {
    let command = |text: &str| {
        CommandSet::ANY
            .parse(text)
            .unwrap_or_else(|e| panic!("built-in command {text:?}: {e}"))
    };
    let seq = sequence(binding);
    let when = When {
        frontend: match binding.frontend {
            Where::Both => None,
            Where::Terminal => Some(Frontend::Terminal),
            Where::Window => Some(Frontend::Gui),
        },
        url: url.cloned(),
        field: (binding.scope == Scope::NoField).then_some(false),
    };
    let action = match binding.action {
        Action::Run(text) => keymap::Action::Run(vec![command(text)]),
        Action::RunThenPass(text) => keymap::Action::RunThenPass(vec![command(text)]),
        Action::Mode(to) => keymap::Action::Run(vec![Command::InputMode(name(to).into())]),
        Action::ExitFullscreenOr(to) => {
            let describe = format!(
                "{} mode (or leave a page's fullscreen)",
                name(to).to_uppercase()
            );
            keymap::Action::Custom(Custom::new(describe, move |browser: &dyn Browser| {
                let tab = browser.tab(browser.active_tab());
                Outcome::run(vec![match tab.is_some_and(|tab| tab.fullscreen) {
                    true => Command::PageExitFullscreen,
                    false => Command::InputMode(name(to).into()),
                }])
            }))
        }
        Action::Custom(f) => keymap::Action::Custom(Custom::new(
            describe_custom(binding.keys),
            move |browser: &dyn Browser| Outcome::run(f(browser)),
        )),
    };
    let line = binding.scope == Scope::Always && url.is_none() && !seq.first().types();
    let bound = keymap::Binding {
        keys: seq,
        when,
        action,
    };
    if line {
        keymap::bind(&mut keys.line, bound.clone());
    }
    let _ = keys.bind(KeyTable::Mode(name(mode)), bound);
}

/// What a binding computed from the browser does, for the help page.
fn describe_custom(keys: &str) -> &'static str {
    match keys {
        "y y" => "copy the page's address",
        "y f" => "copy the link under the pointer",
        "c o" => "close the other tabs",
        _ => "an action of the example configuration",
    }
}
