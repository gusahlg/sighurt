//! Named commands and the keymap that triggers them.
//!
//! Everything the user can do is a command string such as `tab.new` or `tab.select 3`. Keys
//! from the config and the UI's buttons all dispatch the same [`RunCommand`] action. The UI
//! focuses the URL bar for `location.focus` and hands every other command to
//! [`Browser::run_command`](crate::browser::Browser::run_command).

use gpui::{App, KeyBinding, Keystroke, SharedString};

use crate::config;

/// "Run this command." The only GPUI action Sighurt defines.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = sig, no_json)]
pub struct RunCommand(pub SharedString);

/// Every command, by name. `tab.close` takes an optional tab number (counting from 1),
/// `tab.select` a tab number and `page.open` a URL or search.
pub const COMMANDS: &[&str] = &[
    "tab.new",
    "tab.close",
    "tab.next",
    "tab.previous",
    "tab.select",
    "tab.last",
    "page.open",
    "page.reload",
    "page.stop",
    "page.back",
    "page.forward",
    "page.zoom-in",
    "page.zoom-out",
    "page.zoom-reset",
    "location.focus",
    "browser.home",
    "browser.quit",
];

/// Key contexts the UI sets: "page" on the content area, "location" on the URL bar.
pub const CONTEXTS: &[&str] = &["page", "location"];

/// Splits `"tab.select 3"` into `("tab.select", "3")`.
pub fn split(command: &str) -> (&str, &str) {
    let command = command.trim();
    match command.split_once(char::is_whitespace) {
        Some((name, arg)) => (name, arg.trim()),
        None => (command, ""),
    }
}

fn is_known(command: &str) -> bool {
    COMMANDS.contains(&split(command).0)
}

/// Registers `bindings` with GPUI. Invalid keystrokes, unknown commands and unknown contexts are
/// reported and skipped rather than failing startup.
pub fn install_keys(bindings: &[config::KeyBinding], cx: &mut App) {
    let mut accepted = Vec::new();
    for binding in bindings {
        if let Err(problem) = validate(binding) {
            tracing::warn!("key binding {:?}: {problem}", binding.keystrokes);
            continue;
        }
        accepted.push(KeyBinding::new(
            &binding.keystrokes,
            RunCommand(binding.command.clone().into()),
            binding.context.as_deref(),
        ));
    }
    cx.bind_keys(accepted);
}

fn validate(binding: &config::KeyBinding) -> Result<(), String> {
    if !is_known(&binding.command) {
        return Err(format!("unknown command {:?}", binding.command));
    }
    if let Some(context) = &binding.context {
        if !CONTEXTS.contains(&context.as_str()) {
            return Err(format!(
                "unknown context {context:?} (expected one of {CONTEXTS:?})"
            ));
        }
    }
    if binding.keystrokes.split_whitespace().next().is_none() {
        return Err("empty keystroke".into());
    }
    for keystroke in binding.keystrokes.split_whitespace() {
        Keystroke::parse(keystroke).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_arguments() {
        assert_eq!(split("tab.select 3"), ("tab.select", "3"));
        assert_eq!(
            split(" page.open  https://a.b "),
            ("page.open", "https://a.b")
        );
        assert_eq!(split("tab.new"), ("tab.new", ""));
    }

    #[test]
    fn knows_commands() {
        assert!(is_known("tab.new"));
        assert!(is_known("tab.select 4"));
        assert!(!is_known("tab.explode"));
        assert!(!is_known("appearance.theme dark"));
    }

    #[test]
    fn sample_bindings_are_valid() {
        let config = config::Config::parse(Some(
            r#"
            [keys]
            "secondary-t" = "tab.new"
            "secondary-9" = "tab.select 9"
            "secondary-shift-=" = "page.zoom-in"
            "alt-left" = "page.back"
            "f5" = "page.reload"
            [keys.page]
            "g g" = "location.focus"
            [keys.location]
            "alt-enter" = "page.open https://example.com"
            "#,
        ))
        .unwrap();
        assert_eq!(config.keys.len(), 7);
        for binding in &config.keys {
            assert_eq!(validate(binding), Ok(()), "{binding:?}");
        }
    }

    #[test]
    fn rejects_bad_bindings() {
        let binding = |keys: &str, command: &str, context: Option<&str>| config::KeyBinding {
            keystrokes: keys.into(),
            command: command.into(),
            context: context.map(Into::into),
        };
        assert!(validate(&binding("ctrl-t", "tab.nope", None)).is_err());
        assert!(validate(&binding("ctrl-t", "tab.new", Some("sidebar"))).is_err());
        assert!(validate(&binding("", "tab.new", None)).is_err());
        assert!(validate(&binding("g g", "location.focus", Some("page"))).is_ok());
    }
}
