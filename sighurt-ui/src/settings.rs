//! The UI's settings: the config's `[ui]` section layered over [`DEFAULT_UI`], which documents
//! them.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::Colors;

/// The built-in `[ui]` section, printed by `sig --default-config` after the core's defaults.
pub const DEFAULT_UI: &str = include_str!("default-ui.toml");

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Page shown at startup and in new tabs.
    pub home: String,
    /// URL template for text typed into the URL bar that isn't a URL; `{}` is the query.
    pub search: String,
    pub keys: Vec<KeyBinding>,
    pub colors: Colors,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeyBinding {
    pub keystrokes: String,
    pub command: String,
    /// `None` for global bindings, otherwise the key context ("page" or "location").
    pub context: Option<String>,
}

#[derive(Deserialize)]
struct RawDefaults {
    ui: RawSettings,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawSettings {
    home: Option<String>,
    search: Option<String>,
    #[serde(default)]
    keys: BTreeMap<String, RawKey>,
    #[serde(default)]
    colors: toml::Table,
}

/// A `[ui.keys]` entry: either `keystroke = "command"` or a `[ui.keys.<context>]` table.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawKey {
    Command(String),
    Context(BTreeMap<String, String>),
}

impl Settings {
    /// Reads `ui`, the config's `[ui]` section, over the defaults. A broken section is reported
    /// and ignored, like a broken config file.
    pub fn new(ui: &toml::Table) -> Self {
        Self::parse(ui).unwrap_or_else(|e| {
            tracing::warn!("ignoring [ui]: {e}");
            Self::parse(&toml::Table::new()).expect("built-in [ui] is valid")
        })
    }

    fn parse(ui: &toml::Table) -> Result<Self, String> {
        let defaults: RawDefaults = toml::from_str(DEFAULT_UI).map_err(|e| e.to_string())?;
        let user: RawSettings = ui.clone().try_into().map_err(|e| e.to_string())?;

        let mut keys = Vec::new();
        for (key, value) in user.keys {
            match value {
                RawKey::Command(command) => keys.push(KeyBinding {
                    keystrokes: key,
                    command,
                    context: None,
                }),
                RawKey::Context(table) => {
                    keys.extend(table.into_iter().map(|(k, command)| KeyBinding {
                        keystrokes: k,
                        command,
                        context: Some(key.clone()),
                    }))
                }
            }
        }

        Ok(Self {
            home: user.home.or(defaults.ui.home).unwrap_or_default(),
            search: user.search.or(defaults.ui.search).unwrap_or_default(),
            keys,
            colors: Colors::new(&user.colors),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(ui: &str) -> Result<Settings, String> {
        Settings::parse(&toml::from_str(ui).unwrap())
    }

    #[test]
    fn defaults_parse() {
        let s = parse("").unwrap();
        assert_eq!(s.home, "sig://home");
        assert!(s.search.contains("{}"));
        assert!(s.keys.is_empty(), "the built-in config binds no keys");
        assert_eq!(s.colors, Colors::default());
    }

    #[test]
    fn user_values_merge_over_defaults() {
        let s = parse(
            r#"
            home = "https://example.com"
            [keys]
            "ctrl-n" = "tab.new"
            [keys.page]
            "j" = "page.forward"
            "#,
        )
        .unwrap();
        assert_eq!(s.home, "https://example.com");
        assert_eq!(s.search, parse("").unwrap().search);
        assert!(s
            .keys
            .iter()
            .any(|k| k.keystrokes == "ctrl-n" && k.command == "tab.new" && k.context.is_none()));
        assert!(s.keys.iter().any(|k| k.keystrokes == "j"
            && k.command == "page.forward"
            && k.context.as_deref() == Some("page")));
    }

    #[test]
    fn the_printed_defaults_are_a_valid_config() {
        let printed = format!("{}{DEFAULT_UI}", sighurt_core::config::DEFAULT_CONFIG);
        let config = sighurt_core::config::Config::parse(Some(&printed)).unwrap();
        assert_eq!(Settings::parse(&config.ui), parse(""));
    }

    #[test]
    fn typos_are_errors() {
        assert!(parse("hme = \"x\"").is_err());
        assert_eq!(
            Settings::new(&toml::from_str("hme = 1").unwrap()).home,
            "sig://home"
        );
    }
}
