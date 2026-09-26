//! User configuration: `{config_dir}/sighurt/config.toml` layered over [`DEFAULT_CONFIG`].
//!
//! The core reads the engines. The `[ui]` section belongs to the UI, which documents and
//! checks it. The default file doubles as documentation, so the format is described there.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

/// The built-in configuration. User settings are merged on top of it.
pub const DEFAULT_CONFIG: &str = include_str!("default-config.toml");

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub default_engine: String,
    /// Sorted by name, which is also the order routing rules are tried in.
    pub engines: Vec<EngineConfig>,
    /// The `[ui]` section, for the UI crate to interpret. The core does not look inside.
    pub ui: toml::Table,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EngineConfig {
    pub name: String,
    /// Program and arguments. Every engine is a separate process; an empty command is reported
    /// when a page tries to start it.
    pub command: Vec<String>,
    pub schemes: Vec<String>,
    pub extensions: Vec<String>,
    pub mime: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    default_engine: Option<String>,
    #[serde(default)]
    engines: BTreeMap<String, RawEngine>,
    #[serde(default)]
    ui: toml::Table,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawEngine {
    command: Option<Vec<String>>,
    schemes: Option<Vec<String>>,
    extensions: Option<Vec<String>>,
    mime: Option<Vec<String>>,
}

impl Config {
    /// `{config_dir}/sighurt/config.toml`.
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("sighurt")
            .join("config.toml")
    }

    /// Loads the user's config over the defaults. A missing file means defaults; a broken one
    /// is reported and ignored so the browser still starts.
    pub fn load() -> Self {
        let path = Self::path();
        let user = match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                tracing::warn!("cannot read {}: {e}", path.display());
                None
            }
        };
        Self::parse(user.as_deref()).unwrap_or_else(|e| {
            tracing::warn!("ignoring {}: {e}", path.display());
            Self::parse(None).expect("built-in config is valid")
        })
    }

    /// Parses `user` (if any) merged over [`DEFAULT_CONFIG`].
    pub fn parse(user: Option<&str>) -> Result<Self, String> {
        let mut merged: RawConfig = toml::from_str(DEFAULT_CONFIG).map_err(|e| e.to_string())?;
        if let Some(user) = user {
            let user: RawConfig = toml::from_str(user).map_err(|e| e.to_string())?;
            merged.default_engine = user.default_engine.or(merged.default_engine);
            for (name, engine) in user.engines {
                let base = merged.engines.entry(name).or_default();
                base.command = engine.command.or(base.command.take());
                base.schemes = engine.schemes.or(base.schemes.take());
                base.extensions = engine.extensions.or(base.extensions.take());
                base.mime = engine.mime.or(base.mime.take());
            }
            // The UI merges [ui] over defaults of its own.
            merged.ui = user.ui;
        }

        let lower = |v: Option<Vec<String>>| -> Vec<String> {
            v.unwrap_or_default()
                .into_iter()
                .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
                .collect()
        };
        let engines = merged
            .engines
            .into_iter()
            .map(|(name, e)| EngineConfig {
                name,
                command: e.command.unwrap_or_default(),
                schemes: lower(e.schemes),
                extensions: lower(e.extensions),
                mime: lower(e.mime),
            })
            .collect();
        Ok(Self {
            default_engine: merged.default_engine.unwrap_or_default(),
            engines,
            ui: merged.ui,
        })
    }

    pub fn engine(&self, name: &str) -> Option<&EngineConfig> {
        self.engines.iter().find(|e| e.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse() {
        let c = Config::parse(None).unwrap();
        assert_eq!(c.default_engine, "servo");
        let wasm = c.engine("wasm").unwrap();
        assert_eq!(wasm.command, ["sig-wasm"]);
        assert_eq!(wasm.extensions, ["wasm"]);
        assert_eq!(c.engine("servo").unwrap().command, ["sig-servo"]);
        assert!(c.ui.is_empty(), "the UI brings its own defaults");
    }

    #[test]
    fn user_values_merge_over_defaults() {
        let c = Config::parse(Some(
            r#"
            default_engine = "gemini"
            [engines.servo]
            command = ["/opt/sig-servo", "--flag"]
            [engines.gemini]
            command = ["sig-gemini"]
            schemes = ["gemini"]
            "#,
        ))
        .unwrap();
        assert_eq!(c.default_engine, "gemini");
        let servo = c.engine("servo").unwrap();
        assert_eq!(servo.command, ["/opt/sig-servo", "--flag"]);
        // Fields the user didn't set keep their defaults.
        assert!(servo.mime.contains(&"text/html".to_string()));
        assert_eq!(c.engine("gemini").unwrap().schemes, ["gemini"]);
    }

    #[test]
    fn engine_lists_are_normalized() {
        let c = Config::parse(Some(
            r#"
            [engines.docs]
            command = ["sig-docs"]
            extensions = [".MD", "Txt"]
            mime = ["Text/Markdown"]
            "#,
        ))
        .unwrap();
        let docs = c.engine("docs").unwrap();
        assert_eq!(docs.extensions, ["md", "txt"]);
        assert_eq!(docs.mime, ["text/markdown"]);
    }

    #[test]
    fn the_ui_section_is_kept_for_the_ui() {
        let c = Config::parse(Some("[ui.colors]\nbg = \"#000000\"")).unwrap();
        assert_eq!(c.ui["colors"]["bg"].as_str(), Some("#000000"));
        assert_eq!(c.engine("servo").unwrap().command, ["sig-servo"]);
    }

    #[test]
    fn typos_are_errors() {
        assert!(Config::parse(Some("default_engin = \"x\"")).is_err());
        // What used to be the core's is the UI's now.
        assert!(Config::parse(Some("[keys]\n\"ctrl-t\" = \"tab.new\"")).is_err());
        assert!(Config::parse(Some("[engines.x]\ncomand = [\"y\"]")).is_err());
    }
}
