//! The compiled-in configuration's experience driven as the core drives it: its keys on the
//! core's key engine (`sighurt_core::keys::Engine`), a press's `input.mode` carried out in its
//! place as the core does, and what the press came to named as the `Keymap` trait the engine
//! replaced named it, so these tests read as they did (docs/rewrite/experience-2026-11-15.md,
//! §3: ported, not rewritten). A fake browser's tabs have the identities [`tab_id`] gives, the
//! same for the same tab every time it is asked, as the core's are: the engine ties a sequence
//! to the tab it began in.

#![allow(dead_code)]

use std::sync::LazyLock;

use sighurt_api::commands::Command;
use sighurt_api::ids::TabId;
use sighurt_api::input::KeyEvent;
use sighurt_api::keymap::{Browser, Frontend, KeyBinding, KeyContext, Keys, Outcome};
use sighurt_core::keys::Engine;

/// What a key press comes to. Compared by what it does: `RunAll` of one command is `Run`, and
/// of none is `Take`.
#[derive(Clone, Debug)]
pub enum KeyAction {
    /// The key goes to whatever has the keyboard.
    Pass,
    /// Taken, and nothing runs: part of a sequence, or a change of mode.
    Take,
    /// Taken, and the command runs.
    Run(Command),
    /// The command runs, then the key goes on.
    RunThenPass(Command),
    /// Taken, and these run: what a custom action computed.
    RunAll(Vec<Command>),
}

impl KeyAction {
    fn effect(&self) -> (&[Command], bool) {
        match self {
            Self::Pass => (&[], true),
            Self::Take => (&[], false),
            Self::Run(command) => (std::slice::from_ref(command), false),
            Self::RunThenPass(command) => (std::slice::from_ref(command), true),
            Self::RunAll(commands) => (commands, false),
        }
    }
}

impl PartialEq for KeyAction {
    fn eq(&self, other: &Self) -> bool {
        self.effect() == other.effect()
    }
}

/// The most tabs a fake browser has.
pub const MAX_TABS: usize = 64;

/// The identity of a fake browser's tab `index`: the same on every call.
pub fn tab_id(index: usize) -> TabId {
    static IDS: LazyLock<Vec<TabId>> =
        LazyLock::new(|| (0..MAX_TABS).map(|_| TabId::next()).collect());
    IDS[index]
}

/// The experience `sighurt_config_example::configure` returns, on the engine.
pub struct Example {
    keys: Keys,
    engine: Engine,
}

/// The compiled-in configuration's keys: it has no problem.
pub fn keys() -> Keys {
    let (keys, problems) = sighurt_config_example::configure().finish();
    assert_eq!(problems, Vec::<String>::new());
    keys
}

pub fn example() -> Example {
    let keys = keys();
    Example {
        engine: Engine::new(keys.clone()),
        keys,
    }
}

impl Example {
    /// A reload of the same keys: the mode stays.
    pub fn configure(&mut self) {
        self.engine.configure(self.keys.clone());
    }

    /// A fresh press, or a repeat of a press that was taken, as the core asks the engine.
    pub fn press(&mut self, key: &KeyEvent, context: KeyContext, b: &dyn Browser) -> KeyAction {
        if key.repeat {
            return match self.engine.repeat(key, context, b) {
                Some(mut commands) if commands.len() == 1 => KeyAction::Run(commands.remove(0)),
                Some(commands) => KeyAction::RunAll(commands),
                None => KeyAction::Take,
            };
        }
        let Outcome { commands, pass } = self.engine.press(key, context, b);
        let custom = !commands.is_empty() && self.custom_ran(&commands);
        let switches = !commands.is_empty()
            && commands
                .iter()
                .all(|command| matches!(command, Command::InputMode(_)));
        let mut rest = Vec::new();
        for command in commands {
            match command {
                Command::InputMode(name) => assert!(self.engine.set_mode(&name), "{name}"),
                command => rest.push(command),
            }
        }
        match (pass, rest.len()) {
            (true, 0) => KeyAction::Pass,
            (false, 0) if switches || !custom => KeyAction::Take,
            (true, 1) => KeyAction::RunThenPass(rest.remove(0)),
            (false, 1) if !custom => KeyAction::Run(rest.remove(0)),
            (_, _) => KeyAction::RunAll(rest),
        }
    }

    /// Whether `commands` are what one of the experience's computed actions returns (a
    /// clipboard copy, or closing tabs by number), which the trait named `RunAll`.
    fn custom_ran(&self, commands: &[Command]) -> bool {
        commands.iter().all(|command| {
            matches!(
                command,
                Command::ClipboardCopy(_) | Command::TabClose(Some(_))
            )
        })
    }

    pub fn context(&mut self, _context: KeyContext) {
        self.engine.context_changed();
    }

    /// What is shown changed (another tab, or a new document in it), as the core tells the
    /// engine.
    pub fn shown(&mut self) {
        self.engine.shown_changed();
    }

    pub fn reset(&mut self) {
        self.engine.reset();
    }

    pub fn indicator(&self) -> Option<&str> {
        self.engine.indicator()
    }

    pub fn page_takes_text(&self) -> bool {
        self.engine.page_text()
    }

    pub fn describe(&self, frontend: Frontend) -> Vec<KeyBinding> {
        self.engine.describe(frontend)
    }
}
