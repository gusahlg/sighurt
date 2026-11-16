//! `sighurt-config`: what a Sighurt configuration is written against
//! (docs/rewrite/experience-2026-11-15.md, §2).
//!
//! An [`Experience`] is data: its keys ([`KeysBuilder`], a `sighurt_api::keymap::Keys` value
//! with the problems found while it was written) and its interface ([`ui::UiBuilder`], the
//! window's and the terminal's trees of components, `sighurt_api::ui::Interface`). The default experience is
//! [`Experience::default`], written in [`defaults`] with nothing but this crate's public API,
//! and a configuration changes it with the same calls:
//!
//! ```
//! use sighurt_config::prelude::*;
//!
//! fn send_mail(_: &dyn Browser) -> Outcome {
//!     Outcome::run(vec![Command::PageSendKey("ctrl-Enter".parse().unwrap())])
//! }
//!
//! let mut sig = Experience::default();
//! sig.keys
//!     .normal()
//!     .bind("e", "page.switch-engine")
//!     .bind("g g", "page.send-key home")
//!     .bind("ctrl-g", Command::PageOpen("https://github.com".into()))
//!     .unbind("x");
//! sig.keys
//!     .mode("insert")
//!     .on("https://mail.example.com")
//!     .bind("ctrl-enter", custom("send the mail", send_mail));
//! sig.keys
//!     .add_mode(Mode::new("resize").indicator("RESIZE").unmatched(Fallback::Take));
//! assert!(sig.keys.problems().is_empty());
//! ```
//!
//! A table handle ([`KeysBuilder::normal`], [`KeysBuilder::mode`], [`KeysBuilder::line`],
//! [`KeysBuilder::everywhere`], [`KeysBuilder::tables`]) binds in each table it names, as the
//! tables are when it is made (a mode added later gets nothing). Its scopes ([`Tables::on`],
//! [`Tables::in_window`], [`Tables::in_terminal`], [`Tables::in_field`],
//! [`Tables::outside_fields`]) narrow where a binding applies. Binding replaces: a binding
//! removes what it replaces by `sighurt_api::keymap::bind`'s rule, so changing one key keeps
//! the rest. An address condition selects shortcuts and grants nothing.
//!
//! **Mistakes are collected, not panics.** Bad keys, unparsable commands and modes that don't
//! exist yet are noted as they are written ([`KeysBuilder::problems`]); what needs the whole
//! picture (a mode never added, an alias the configuration file's `[commands]` lacks, a prefix
//! conflict in a hand-built table) is `Keys::check`'s, which `sig` runs when it loads. An
//! experience with any problem is not used at all.
//!
//! Must not own: anything at run time (the engine, the mode, the presses taken are the core's),
//! or a second vocabulary of actions: commands are [`Command`]s.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod defaults;
pub mod ui;

use sighurt_api::commands::{Command, CommandSet};
use sighurt_api::keymap::{
    bind, unbind, Action, Binding, Browser, Custom, Frontend, KeySeq, Keys, Mode, Outcome,
    UrlPattern, When, LINE,
};
use sighurt_api::ui::Interface;

use crate::ui::UiBuilder;

/// What a configuration imports.
pub mod prelude {
    pub use crate::ui::UiBuilder;
    pub use crate::{custom, Experience, IntoAction, KeysBuilder, Tables};
    pub use sighurt_api::commands::{Command, SentKey};
    pub use sighurt_api::input::KeyEvent;
    pub use sighurt_api::keymap::{
        Action, Browser, Custom, Fallback, Frontend, Mode, Outcome, TabInfo, PLAIN_MODE,
    };
    pub use sighurt_api::ui::{Anchor, Button, Interface, Kind, Node, Side};
}

/// The experience a configuration describes: its keys and its interface now, its events later.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct Experience {
    /// The keys: modes, the line's table, and the bindings of each.
    pub keys: KeysBuilder,
    /// The interface: where the window's components go, and the terminal's lines ([`ui`]).
    pub ui: UiBuilder,
}

impl Default for Experience {
    /// The default experience ([`defaults`]).
    fn default() -> Self {
        defaults::experience()
    }
}

impl Experience {
    /// No experience at all: one mode that passes every key and gives the page text, binding
    /// nothing (`Keys::plain`), and the standard interface.
    pub fn empty() -> Self {
        Self {
            keys: KeysBuilder {
                keys: Keys::plain(),
                problems: Vec::new(),
            },
            ui: UiBuilder::default(),
        }
    }

    /// The keys, and every problem noted while they were written.
    pub fn finish(self) -> (Keys, Vec<String>) {
        (self.keys.keys, self.keys.problems)
    }

    /// The interface, and every problem applying it found, its check's among them
    /// ([`UiBuilder::finish`]).
    pub fn interface(&self) -> (Interface, Vec<String>) {
        self.ui.finish()
    }
}

/// The keys of an [`Experience`], being written.
#[derive(Clone, Debug)]
pub struct KeysBuilder {
    keys: Keys,
    problems: Vec<String>,
}

/// A table a [`Tables`] handle binds in.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    Line,
    Mode(String),
}

impl KeysBuilder {
    /// The mode `normal`'s table.
    pub fn normal(&mut self) -> Tables<'_> {
        self.mode("normal")
    }

    /// The mode `name`'s table. A mode not added yet is a problem, noted now.
    pub fn mode(&mut self, name: &str) -> Tables<'_> {
        self.tables([name])
    }

    /// The line's table: while the address line or the find field has the keyboard.
    pub fn line(&mut self) -> Tables<'_> {
        self.tables([LINE])
    }

    /// Every mode there is now, and the line.
    pub fn everywhere(&mut self) -> Tables<'_> {
        let mut targets: Vec<Target> = (self.keys.modes.iter())
            .map(|mode| Target::Mode(mode.name.clone()))
            .collect();
        targets.push(Target::Line);
        Tables {
            builder: self,
            targets,
            when: When::default(),
        }
    }

    /// The tables named: modes by name, and the line as `"line"`.
    pub fn tables<'n>(&mut self, names: impl IntoIterator<Item = &'n str>) -> Tables<'_> {
        let mut targets = Vec::new();
        for name in names {
            match name {
                LINE => targets.push(Target::Line),
                name if self.keys.mode(name).is_some() => targets.push(Target::Mode(name.into())),
                name => self.problems.push(format!(
                    "keys.mode({name:?}): there is no mode {name:?} (add it first)"
                )),
            }
        }
        Tables {
            builder: self,
            targets,
            when: When::default(),
        }
    }

    /// Adds `mode`. A name another mode has is a problem.
    pub fn add_mode(&mut self, mode: Mode) -> &mut Self {
        match self.keys.mode(&mode.name) {
            Some(_) => self
                .problems
                .push(format!("add_mode: there is a mode {:?} already", mode.name)),
            None => self.keys.modes.push(mode),
        }
        self
    }

    /// Removes the mode `name` and its bindings. A mode there is none of is a problem.
    pub fn remove_mode(&mut self, name: &str) -> &mut Self {
        match self.keys.modes.iter().position(|mode| mode.name == name) {
            Some(index) => _ = self.keys.modes.remove(index),
            None => self
                .problems
                .push(format!("remove_mode: there is no mode {name:?}")),
        }
        self
    }

    /// Makes `name` the mode a session starts in (checked when the keys are loaded).
    pub fn initial(&mut self, name: &str) -> &mut Self {
        self.keys.initial = name.into();
        self
    }

    /// The keys as they are now.
    pub fn value(&self) -> &Keys {
        &self.keys
    }

    /// The keys, to change by hand: their fields are public data. `Keys::check` sees them when
    /// they are loaded.
    pub fn value_mut(&mut self) -> &mut Keys {
        &mut self.keys
    }

    /// The problems noted so far, one line each.
    pub fn problems(&self) -> &[String] {
        &self.problems
    }
}

/// Some tables of the keys, and a condition: what [`Tables::bind`] binds in and when it
/// applies.
#[derive(Debug)]
pub struct Tables<'a> {
    builder: &'a mut KeysBuilder,
    targets: Vec<Target>,
    when: When,
}

impl Tables<'_> {
    /// Binds `keys` (one to four keystrokes: `"ctrl-o"`, `"y y"`) to `action` in each table,
    /// replacing what that replaces.
    pub fn bind(&mut self, keys: &str, action: impl IntoAction) -> &mut Self {
        let written = format!("bind({keys:?})");
        let (Some(seq), Some(action)) = (
            self.keys(keys, &written),
            action
                .into_action()
                .map_err(|e| self.builder.problems.push(format!("{written}: {e}")))
                .ok(),
        ) else {
            return self;
        };
        let when = self.when.clone();
        self.each(|bindings| {
            bind(
                bindings,
                Binding {
                    keys: seq.clone(),
                    when: when.clone(),
                    action: action.clone(),
                },
            )
        });
        self
    }

    /// Unbinds `keys` in each table: what binding them would replace goes, and where the
    /// handle has a condition a broader binding of exactly these keys stops applying too, so
    /// the key falls to the mode's `unmatched`.
    pub fn unbind(&mut self, keys: &str) -> &mut Self {
        let Some(seq) = self.keys(keys, &format!("unbind({keys:?})")) else {
            return self;
        };
        let when = self.when.clone();
        self.each(|bindings| unbind(bindings, seq.clone(), when.clone()));
        self
    }

    /// Binds `keys` to passing to whatever has the keyboard, running nothing.
    pub fn pass(&mut self, keys: &str) -> &mut Self {
        self.bind(keys, Action::RunThenPass(Vec::new()))
    }

    /// Binds `keys` to being taken, running nothing.
    pub fn take(&mut self, keys: &str) -> &mut Self {
        self.bind(keys, Action::Run(Vec::new()))
    }

    /// The same tables, only on the addresses `url` matches (`scheme://host` and an optional
    /// path, `*.` hosts): the shown tab's committed address. It selects shortcuts and grants
    /// nothing; it ignores the port.
    pub fn on(&mut self, url: &str) -> Tables<'_> {
        let mut when = self.when.clone();
        match url.parse::<UrlPattern>() {
            Ok(pattern) => when.url = Some(pattern),
            Err(e) => {
                self.builder.problems.push(format!("on({url:?}): {e}"));
                return self.nothing();
            }
        }
        self.narrower(when)
    }

    /// The same tables, only in the window.
    pub fn in_window(&mut self) -> Tables<'_> {
        self.frontend(Frontend::Gui)
    }

    /// The same tables, only in the terminal.
    pub fn in_terminal(&mut self) -> Tables<'_> {
        self.frontend(Frontend::Terminal)
    }

    /// The same tables, only while a text field of the page has the keyboard.
    pub fn in_field(&mut self) -> Tables<'_> {
        self.field(true)
    }

    /// The same tables, only while no text field of the page has the keyboard.
    pub fn outside_fields(&mut self) -> Tables<'_> {
        self.field(false)
    }

    fn frontend(&mut self, frontend: Frontend) -> Tables<'_> {
        let when = When {
            frontend: Some(frontend),
            ..self.when.clone()
        };
        self.narrower(when)
    }

    fn field(&mut self, field: bool) -> Tables<'_> {
        let when = When {
            field: Some(field),
            ..self.when.clone()
        };
        self.narrower(when)
    }

    fn narrower(&mut self, when: When) -> Tables<'_> {
        Tables {
            targets: self.targets.clone(),
            builder: &mut *self.builder,
            when,
        }
    }

    /// A handle that binds nothing: its scope was a mistake, already noted.
    fn nothing(&mut self) -> Tables<'_> {
        Tables {
            targets: Vec::new(),
            builder: &mut *self.builder,
            when: self.when.clone(),
        }
    }

    fn keys(&mut self, keys: &str, written: &str) -> Option<KeySeq> {
        keys.parse::<KeySeq>()
            .map_err(|e| self.builder.problems.push(format!("{written}: {e}")))
            .ok()
    }

    fn each(&mut self, mut f: impl FnMut(&mut Vec<Binding>)) {
        for target in &self.targets {
            let keys = &mut self.builder.keys;
            let bindings = match target {
                Target::Line => Some(&mut keys.line),
                Target::Mode(name) => keys
                    .modes
                    .iter_mut()
                    .find(|mode| mode.name == *name)
                    .map(|mode| &mut mode.bindings),
            };
            // A mode removed since the handle was made gets nothing.
            if let Some(bindings) = bindings {
                f(bindings);
            }
        }
    }
}

/// What [`Tables::bind`] takes: a [`Command`], a command as text, a list of commands, an
/// [`Action`], or a [`Custom`] action ([`custom`]).
pub trait IntoAction {
    /// The action, or why there is none (a command as text that doesn't parse).
    fn into_action(self) -> Result<Action, String>;
}

impl IntoAction for Action {
    fn into_action(self) -> Result<Action, String> {
        Ok(self)
    }
}

impl IntoAction for Command {
    fn into_action(self) -> Result<Action, String> {
        Ok(Action::Run(vec![self]))
    }
}

impl IntoAction for Vec<Command> {
    fn into_action(self) -> Result<Action, String> {
        Ok(Action::Run(self))
    }
}

impl<const N: usize> IntoAction for [Command; N] {
    fn into_action(self) -> Result<Action, String> {
        Ok(Action::Run(self.into()))
    }
}

/// A command as the configuration file writes it (`"tab.select 3"`, `"help"`). A `[commands]`
/// alias is checked against the file's table when the keys are loaded.
impl IntoAction for &str {
    fn into_action(self) -> Result<Action, String> {
        Ok(Action::Run(vec![CommandSet::ANY.parse(self)?]))
    }
}

impl IntoAction for String {
    fn into_action(self) -> Result<Action, String> {
        self.as_str().into_action()
    }
}

impl IntoAction for Custom {
    fn into_action(self) -> Result<Action, String> {
        Ok(Action::Custom(self))
    }
}

/// A custom action: `run`, a function of what the browser shows before the press, described
/// as `describe` on the help page. It does no I/O, reads no clock and logs nothing; what it
/// reads of a page it passes on only as a command's operand.
pub fn custom(
    describe: &str,
    run: impl Fn(&dyn Browser) -> Outcome + Send + Sync + 'static,
) -> Custom {
    Custom::new(describe, run)
}

#[cfg(test)]
mod tests {
    use sighurt_api::keymap::{Fallback, KeyTable, When};

    use super::*;

    fn table<'k>(keys: &'k Keys, name: &str) -> &'k [Binding] {
        let table = match name {
            LINE => KeyTable::Line,
            name => KeyTable::Mode(name),
        };
        keys.table(table).unwrap()
    }

    #[test]
    fn mistakes_are_collected_not_panics() {
        let mut sig = Experience::default();
        sig.keys
            .normal()
            .bind("g hyper-x", "tab.new")
            .bind("z", "tab.nope")
            .on("terminal")
            .bind("x", "tab.new");
        sig.keys.mode("visual").bind("v", "tab.new");
        sig.keys.add_mode(Mode::new("normal"));
        sig.keys.remove_mode("visual");
        let problems = sig.keys.problems().to_vec();
        let expected = [
            "bind(\"g hyper-x\"): unknown modifier",
            "bind(\"z\"): unknown command \"tab.nope\"",
            "on(\"terminal\"): an address pattern is scheme://host",
            "keys.mode(\"visual\"): there is no mode \"visual\"",
            "add_mode: there is a mode \"normal\" already",
            "remove_mode: there is no mode \"visual\"",
        ];
        assert_eq!(problems.len(), expected.len(), "{problems:#?}");
        for (problem, part) in problems.iter().zip(expected) {
            assert!(problem.contains(part), "{part:?} not in {problem:?}");
        }
        // What needs the whole picture is the check's: a mode switch to a mode never added.
        let mut sig = Experience::default();
        sig.keys.normal().bind("r", "input.mode resize");
        let (keys, problems) = sig.finish();
        assert!(problems.is_empty());
        assert!(keys.check(&CommandSet::ANY, Frontend::Gui)[0].contains("no mode \"resize\""));
    }

    #[test]
    fn a_mode_added_later_may_be_named_first_and_tables_are_those_there_are() {
        let mut sig = Experience::default();
        // Review finding 12: `input.mode resize` before the mode exists is no mistake.
        sig.keys.normal().bind("r", "input.mode resize");
        // Review finding 13: `everywhere()` binds in the tables there are when it is made.
        sig.keys.everywhere().bind("ctrl-q", "browser.quit");
        sig.keys.add_mode(
            Mode::new("resize")
                .indicator("RESIZE")
                .unmatched(Fallback::Take),
        );
        let (keys, problems) = sig.finish();
        assert!(problems.is_empty(), "{problems:?}");
        assert!(keys.check(&CommandSet::ANY, Frontend::Gui).is_empty());
        let bound = |name| {
            table(&keys, name)
                .iter()
                .any(|b| b.keys.to_string() == "ctrl-q")
        };
        assert!(bound("normal") && bound("insert") && bound(LINE));
        assert!(!bound("resize"), "a mode added later gets nothing");
    }

    #[test]
    fn scopes_narrow_and_actions_convert() {
        let mut sig = Experience::empty();
        let mut all = sig.keys.tables([PLAIN, LINE]);
        all.in_terminal()
            .outside_fields()
            .bind("ctrl-k", "tab.close");
        all.on("https://*.a.test").take("ctrl-j").pass("ctrl-p");
        all.in_window()
            .bind("ctrl-n", [Command::TabNew(None), Command::PageReload]);
        all.in_field()
            .bind("ctrl-u", custom("clear", |_| Outcome::take()));
        let (keys, problems) = sig.finish();
        assert!(problems.is_empty(), "{problems:?}");
        let plain = table(&keys, PLAIN);
        let find = |keys: &str| plain.iter().find(|b| b.keys.to_string() == keys).unwrap();
        assert_eq!(
            find("ctrl-k").when,
            When {
                frontend: Some(Frontend::Terminal),
                field: Some(false),
                url: None,
            }
        );
        assert_eq!(find("ctrl-j").action, Action::Run(Vec::new()));
        assert_eq!(find("ctrl-p").action, Action::RunThenPass(Vec::new()));
        assert!(find("ctrl-p").when.url.is_some());
        assert_eq!(
            find("ctrl-n").action.clone(),
            Action::Run(vec![Command::TabNew(None), Command::PageReload])
        );
        assert!(matches!(find("ctrl-u").action, Action::Custom(_)));
        assert_eq!(table(&keys, LINE).len(), plain.len());
        // Unbinding on a scope leaves a tombstone there; a plain unbind removes.
        let mut sig = Experience::default();
        sig.keys.normal().unbind("x");
        sig.keys.normal().in_window().unbind("q");
        let (keys, _) = sig.finish();
        let normal = table(&keys, "normal");
        assert!(!normal.iter().any(|b| b.keys.to_string() == "x"));
        let q: Vec<_> = normal
            .iter()
            .filter(|b| b.keys.to_string() == "q")
            .collect();
        assert_eq!(q.len(), 2, "the general q, and the window's tombstone");
        assert!(q.iter().any(|b| b.action == Action::Unbound));
    }

    const PLAIN: &str = sighurt_api::keymap::PLAIN_MODE;
}
