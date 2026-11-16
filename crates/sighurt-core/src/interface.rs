//! The interface as the configuration file changes it: the experience's trees with
//! `[chrome.standard]` (whatever chrome `[ui] chrome` names), then `[ui.layout]`
//! and `[tui.layout]`, as operations of `sighurt_api::ui`, applied together and checked once
//! (docs/rewrite/interface-2026-11-16.md, §4 and rulings 5 and 11).
//!
//! The order is fixed, whatever the file's: `[chrome.standard]` `tabs`, `status`, `buttons`,
//! `compact`, `engine-label`; then `[ui.layout]` `address`, `tabs`, `status`, `buttons`,
//! `compact`, `engine-label`, so a canonical setting overrides the same legacy one; then
//! `[tui.layout]` `summary`, `line`. The result must pass `Interface::check`; one that doesn't
//! is a problem, so the configuration is not used and a reload keeps the one before it.
//! `[chrome.standard]` is the standard chrome's own table, which its check reads too; it applies
//! whatever chrome is named, since a chrome changes at the next start and the standard one,
//! the window tree's only reader, may still be running. `[chrome.line]` is the line chrome's alone.
//!
//! Must not own: what a tree may be (`sighurt_api::ui`'s check), or how one is shown (the
//! frontends').

use sighurt_api::keymap::Frontend;
use sighurt_api::settings::{Table, Value};
use sighurt_api::ui::{Anchor, Button, Component, Density, Interface, Kind, Show, Side};

/// One setting of a layout table, as an operation.
#[derive(Clone, Debug, PartialEq)]
enum Setting {
    /// Put the component of `kind` at `side` of the page, or remove it (`None`).
    Place(Frontend, Kind, Option<Side>),
    /// The status line over the page's bottom-left corner, shown while there is status.
    StatusOverlay,
    /// The address bar's buttons.
    Buttons(Vec<Button>),
    /// The density.
    Compact(bool),
    /// The engine's name in the tabs and on the address bar's button.
    EngineLabel(bool),
}

/// `interface` (the experience's) with the layout tables applied: `standard` is
/// `[chrome.standard]`, `ui_layout` and `tui_layout` are
/// `[ui.layout]` and `[tui.layout]`. What a table says wrong, and what the result's check
/// finds, are `problems`; the interface is then the experience's.
pub fn configure(
    interface: &Interface,
    standard: Option<&Table>,
    ui_layout: Option<&Table>,
    tui_layout: Option<&Table>,
    problems: &mut Vec<String>,
) -> Interface {
    let mut settings = Vec::new();
    if let Some(table) = standard {
        // The standard chrome's check reports what is wrong in its table; here a value it
        // can't take is left out.
        let mut ignored = Vec::new();
        settings.extend(read(
            "[chrome.standard]",
            table,
            legacy_setting,
            &mut ignored,
        ));
    }
    if let Some(table) = ui_layout {
        settings.extend(read("[ui.layout]", table, window_setting, problems));
    }
    if let Some(table) = tui_layout {
        settings.extend(read("[tui.layout]", table, terminal_setting, problems));
    }
    settings.sort_by_key(|(rank, _)| *rank);
    let mut changed = interface.clone();
    let result = changed.change(|interface| {
        for (_, setting) in settings {
            apply(interface, setting);
        }
    });
    match result {
        Ok(()) => changed,
        Err(found) => {
            problems.extend(
                found
                    .into_iter()
                    .map(|problem| format!("the layout tables: {problem}")),
            );
            interface.clone()
        }
    }
}

/// What reads one setting of a table: its key and value into its rank and operation.
type Reader = fn(&str, &Value) -> Result<(usize, Setting), String>;

/// The settings of `table`, each read by `setting` into its rank and operation; what it can't
/// read is a problem.
fn read(
    section: &str,
    table: &Table,
    setting: Reader,
    problems: &mut Vec<String>,
) -> Vec<(usize, Setting)> {
    let mut settings = Vec::new();
    for (key, value) in table {
        match setting(key, value) {
            Ok(read) => settings.push(read),
            Err(problem) => problems.push(format!("{section} {key}: {problem}")),
        }
    }
    settings
}

/// Applies one setting; whether the result can be shown is the check's, once, at the end.
fn apply(interface: &mut Interface, setting: Setting) {
    match setting {
        Setting::Place(frontend, kind, Some(side)) => {
            interface.tree_mut(frontend).position(kind, side)
        }
        Setting::Place(frontend, kind, None) => interface.tree_mut(frontend).remove(kind),
        Setting::StatusOverlay => interface.window.overlay(
            Component::new(Kind::StatusBar),
            Anchor::BottomLeft,
            Show::WhenNeeded,
        ),
        Setting::Buttons(buttons) => {
            if let Some(Component::AddressBar(bar)) = interface.window.get_mut(Kind::AddressBar) {
                bar.buttons = buttons;
            }
        }
        Setting::Compact(compact) => {
            interface.density = match compact {
                true => Density::Compact,
                false => Density::Normal,
            };
        }
        Setting::EngineLabel(on) => {
            if let Some(Component::TabBar(tabs)) = interface.window.get_mut(Kind::TabBar) {
                tabs.engine_label = on;
            }
            if let Some(Component::AddressBar(bar)) = interface.window.get_mut(Kind::AddressBar) {
                bar.engine_button = on;
            }
        }
    }
}

/// What `value` says, as one of `choices` (`(word, meaning)`).
fn choice<T: Clone>(value: &Value, choices: &[(&str, T)]) -> Result<T, String> {
    let found = value
        .as_str()
        .and_then(|word| choices.iter().find(|(name, _)| *name == word));
    found.map(|(_, meaning)| meaning.clone()).ok_or_else(|| {
        let names: Vec<String> = choices
            .iter()
            .map(|(name, _)| format!("{name:?}"))
            .collect();
        let (last, rest) = names.split_last().expect("at least one choice");
        let given = match value.as_str() {
            Some(text) => format!("{text:?}"),
            None => value.kind().to_owned(),
        };
        format!("expected {} or {last}, not {given}", rest.join(", "))
    })
}

fn boolean(value: &Value) -> Result<bool, String> {
    value
        .as_bool()
        .ok_or_else(|| format!("expected true or false, not {}", value.kind()))
}

const SIDES: &[(&str, Option<Side>)] = &[
    ("top", Some(Side::Top)),
    ("bottom", Some(Side::Bottom)),
    ("left", Some(Side::Left)),
    ("right", Some(Side::Right)),
    ("hidden", None),
];
const ENDS: &[(&str, Option<Side>)] = &[("top", Some(Side::Top)), ("bottom", Some(Side::Bottom))];

/// `[ui.layout]`.
fn window_setting(key: &str, value: &Value) -> Result<(usize, Setting), String> {
    let place = |kind, side| Setting::Place(Frontend::Gui, kind, side);
    Ok(match key {
        "address" => (10, place(Kind::AddressBar, choice(value, ENDS)?)),
        "tabs" => (11, place(Kind::TabBar, choice(value, SIDES)?)),
        "status" => (12, status(value)?),
        "buttons" => (13, Setting::Buttons(buttons(value)?)),
        "compact" => (14, Setting::Compact(boolean(value)?)),
        "engine-label" => (15, Setting::EngineLabel(boolean(value)?)),
        _ => {
            return Err(
                "unknown key ([ui.layout] has address, tabs, status, buttons, compact and \
                 engine-label)"
                    .into(),
            )
        }
    })
}

/// `[chrome.standard]`, in its own words: `tabs` is `"top"`, `"left"` or `"hidden"`, `buttons`
/// true or false. Ranked before `[ui.layout]`.
fn legacy_setting(key: &str, value: &Value) -> Result<(usize, Setting), String> {
    let place = |kind, side| Setting::Place(Frontend::Gui, kind, side);
    let tabs = &[
        ("top", Some(Side::Top)),
        ("left", Some(Side::Left)),
        ("hidden", None),
    ];
    Ok(match key {
        "tabs" => (0, place(Kind::TabBar, choice(value, tabs)?)),
        "status" => (1, status(value)?),
        "buttons" => (2, Setting::Buttons(standard_buttons(boolean(value)?))),
        "compact" => (3, Setting::Compact(boolean(value)?)),
        "engine-label" => (4, Setting::EngineLabel(boolean(value)?)),
        _ => return Err("unknown key".into()),
    })
}

/// `[tui.layout]`.
fn terminal_setting(key: &str, value: &Value) -> Result<(usize, Setting), String> {
    let place = |kind, side| Setting::Place(Frontend::Terminal, kind, side);
    let summary = &[
        ("top", Some(Side::Top)),
        ("bottom", Some(Side::Bottom)),
        ("hidden", None),
    ];
    match key {
        "summary" => Ok((20, place(Kind::Summary, choice(value, summary)?))),
        "line" => Ok((21, place(Kind::CommandLine, choice(value, ENDS)?))),
        _ => Err("unknown key ([tui.layout] has summary and line)".into()),
    }
}

fn status(value: &Value) -> Result<Setting, String> {
    let place = Setting::Place(Frontend::Gui, Kind::StatusBar, Some(Side::Bottom));
    let choices = [("strip", Some(place)), ("overlay", None)];
    Ok(choice(value, &choices)?.unwrap_or(Setting::StatusOverlay))
}

/// The buttons `true` and `false` stand for: back, forward and reload, or none.
fn standard_buttons(on: bool) -> Vec<Button> {
    match on {
        true => vec![Button::Back, Button::Forward, Button::Reload],
        false => Vec::new(),
    }
}

/// `buttons`: true, false, or a list of `"back"`, `"forward"` and `"reload"`.
fn buttons(value: &Value) -> Result<Vec<Button>, String> {
    let names = [
        ("back", Button::Back),
        ("forward", Button::Forward),
        ("reload", Button::Reload),
    ];
    match value {
        Value::Boolean(on) => Ok(standard_buttons(*on)),
        Value::Array(list) => list.iter().map(|name| choice(name, &names)).collect(),
        _ => Err(format!(
            "expected true, false or a list of \"back\", \"forward\" and \"reload\", not {}",
            value.kind()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sighurt_api::ui::Node;

    fn table(entries: &[(&str, Value)]) -> Table {
        entries
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    fn text(word: &str) -> Value {
        Value::String(word.into())
    }

    fn configured(
        standard: Option<Table>,
        ui: Option<Table>,
        tui: Option<Table>,
    ) -> (Interface, Vec<String>) {
        let mut problems = Vec::new();
        let interface = configure(
            &Interface::standard(),
            standard.as_ref(),
            ui.as_ref(),
            tui.as_ref(),
            &mut problems,
        );
        (interface, problems)
    }

    fn leaf(kind: Kind) -> Node {
        Node::Leaf(Component::new(kind))
    }

    #[test]
    fn the_old_options_are_operations() {
        let left = table(&[("tabs", text("left"))]);
        let (interface, problems) = configured(Some(left.clone()), None, None);
        assert_eq!(problems, Vec::<String>::new());
        let mut expected = Interface::standard();
        expected.window.position(Kind::TabBar, Side::Left);
        assert_eq!(interface, expected);
        // [ui.layout] says the same, and more.
        let (same, _) = configured(None, Some(left), None);
        assert_eq!(same, interface);
        let (right, problems) = configured(
            None,
            Some(table(&[
                ("tabs", text("right")),
                ("status", text("overlay")),
                ("compact", Value::Boolean(true)),
                ("buttons", Value::Array(vec![text("reload"), text("back")])),
                ("engine-label", Value::Boolean(false)),
            ])),
            None,
        );
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!(right.density, Density::Compact);
        let Some(Component::AddressBar(bar)) = right.window.get(Kind::AddressBar) else {
            panic!()
        };
        assert_eq!(bar.buttons, [Button::Reload, Button::Back]);
        assert!(!bar.engine_button);
        let layered = right.window.components();
        assert!(layered
            .iter()
            .any(|(c, layer)| c.kind() == Kind::StatusBar && layer.is_some()));
    }

    /// Review 5: the legacy table and the canonical one together; the canonical setting wins
    /// whatever the order of the keys, and the same tables again give the same interface.
    #[test]
    fn canonical_settings_override_legacy_ones_in_a_fixed_order() {
        let legacy = table(&[
            ("tabs", text("left")),
            ("engine-label", Value::Boolean(false)),
        ]);
        let canonical = table(&[
            ("tabs", text("bottom")),
            ("engine-label", Value::Boolean(true)),
            ("address", text("bottom")),
        ]);
        let (interface, problems) = configured(Some(legacy.clone()), Some(canonical.clone()), None);
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!(
            interface.window,
            Node::Column(vec![
                leaf(Kind::PageView),
                leaf(Kind::AddressBar),
                leaf(Kind::TabBar),
                leaf(Kind::StatusBar),
            ])
        );
        for _ in 0..3 {
            let again = configured(Some(legacy.clone()), Some(canonical.clone()), None);
            assert_eq!(again, (interface.clone(), Vec::new()));
        }
        // A legacy value the chrome's own check reports is left out here, not reported twice.
        let (_, problems) = configured(Some(table(&[("tabs", text("right"))])), None, None);
        assert_eq!(problems, Vec::<String>::new());
    }

    #[test]
    fn the_terminal_layout() {
        let bottom = table(&[("summary", text("bottom"))]);
        let (interface, problems) = configured(None, None, Some(bottom));
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!(
            interface.terminal,
            Node::Column(vec![
                leaf(Kind::PageView),
                leaf(Kind::Summary),
                leaf(Kind::CommandLine)
            ])
        );
        let hidden = table(&[("summary", text("hidden")), ("line", text("top"))]);
        let (interface, _) = configured(None, None, Some(hidden));
        assert_eq!(
            interface.terminal,
            Node::Column(vec![leaf(Kind::CommandLine), leaf(Kind::PageView)])
        );
    }

    #[test]
    fn what_a_tree_cannot_take_is_a_problem() {
        let twice = Value::Array(vec![text("back"), text("back")]);
        for (ui, tui, expected) in [
            (
                Some(table(&[("tabs", text("middle"))])),
                None,
                "[ui.layout] tabs: expected \"top\"",
            ),
            (
                Some(table(&[("buttons", Value::Array(vec![text("home")]))])),
                None,
                "[ui.layout] buttons: expected",
            ),
            (
                Some(table(&[("buttons", twice)])),
                None,
                "the layout tables: the window's interface: the Back button twice",
            ),
            (
                Some(table(&[("colour", Value::Integer(1))])),
                None,
                "[ui.layout] colour: unknown key",
            ),
            (
                None,
                Some(table(&[("summary", text("left"))])),
                "[tui.layout] summary: expected",
            ),
        ] {
            let (interface, problems) = configured(None, ui.clone(), tui);
            assert!(
                problems.iter().any(|p| p.starts_with(expected)),
                "{ui:?}: {problems:?}"
            );
            // What was refused changed nothing.
            assert_eq!(interface, Interface::standard());
        }
    }
}
