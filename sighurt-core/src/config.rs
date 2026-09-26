//! The configuration: the user's `config.toml` over [`DEFAULT_CONFIG`], parsed once into typed
//! settings and validated completely.
//!
//! The format is versioned (`version = 1`) and documented by the default file itself, which
//! `sig --default-config` prints. Parsing collects the problems of every part of the file, so
//! one reload reports them all (a part whose keys or types are wrong is reported once and
//! checked no further); a configuration with any problem is an error and is never partly used.
//! Engines are compiled into `sig`, so the configuration only says which of them may open pages
//! and which URLs go to which; there are no commands or processes to configure.
//!
//! `[ui]` belongs to the chrome and holds only `[ui.colors]`: the core checks that and the
//! colors' format (`#rrggbb` or `#rrggbbaa`) and hands it on; the chrome knows their names.
//!
//! Must not own: reading or watching files, which the executable does, or deciding what a
//! reload means for open pages ([`crate::state`]).

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::fmt;

use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::keys::Bindings;

/// The built-in configuration, which also documents the format.
pub const DEFAULT_CONFIG: &str = include_str!("default-config.toml");

/// The format version this code reads.
pub const VERSION: i64 = 1;

/// Schemes whose routing is fixed: `sig` is Sighurt's, http(s) and file follow the rules for
/// them.
const FIXED_SCHEMES: [&str; 4] = ["sig", "http", "https", "file"];

/// A complete, valid configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    home: String,
    search: String,
    default_engine: String,
    enabled: Option<Vec<String>>,
    routes: Vec<EngineRoutes>,
    keys: Bindings,
    ui: toml::Table,
}

/// The URLs one engine opens besides those that go to the default engine.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineRoutes {
    /// The engine's name.
    pub engine: String,
    /// URL schemes it opens, in lower case.
    pub schemes: Vec<String>,
    /// File extensions it opens, in lower case and without the dot.
    pub extensions: Vec<String>,
    /// Web origins (`scheme://host[:port]`) whose files with one of `extensions` it opens.
    pub origins: Vec<String>,
}

/// Why a configuration can't be used: every problem found in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    problems: Vec<String>,
}

impl ConfigError {
    fn one(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
        }
    }

    /// What is wrong, one line (or TOML error message) each.
    pub fn problems(&self) -> &[String] {
        &self.problems
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid configuration")?;
        for problem in &self.problems {
            write!(f, "\n- {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    version: Option<i64>,
    home: Option<String>,
    search: Option<String>,
    #[serde(default)]
    engines: RawEngines,
    #[serde(default)]
    routing: BTreeMap<String, RawRoutes>,
    keys: Option<toml::Table>,
    ui: Option<toml::Table>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawEngines {
    default: Option<String>,
    enabled: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawRoutes {
    schemes: Option<Vec<String>>,
    extensions: Option<Vec<String>>,
    origins: Option<Vec<String>>,
}

impl Default for Config {
    /// The built-in configuration.
    fn default() -> Self {
        Self::parse(None).expect("the built-in configuration is valid")
    }
}

impl Config {
    /// Parses `user`, the text of the user's file (`None` if there is none), over
    /// [`DEFAULT_CONFIG`].
    pub fn parse(user: Option<&str>) -> Result<Self, ConfigError> {
        let defaults: Raw = toml::from_str(DEFAULT_CONFIG).map_err(toml_error)?;
        let Some(user) = user else {
            return validate(defaults, Vec::new());
        };
        let table: toml::Table = toml::from_str(user).map_err(toml_error)?;
        match table.get("version") {
            Some(toml::Value::Integer(VERSION)) => {}
            Some(other) => {
                return Err(ConfigError::one(format!(
                    "version {other} is not a configuration format this sig reads (it reads \
                     version {VERSION})"
                )))
            }
            None => {
                return Err(ConfigError::one(format!(
                    "the file needs `version = {VERSION}` at the top. Without it, it is in the \
                     format of an older sig, which this one doesn't read; `sig \
                     --default-config` prints the current format"
                )))
            }
        }
        let mut problems = Vec::new();
        let user = sections(table, &mut problems);
        validate(merge(defaults, user), problems)
    }

    /// The page shown at startup and in new tabs: an absolute URL.
    pub fn home(&self) -> &str {
        &self.home
    }

    /// The search URL template, with `{}` where the query goes.
    pub fn search(&self) -> &str {
        &self.search
    }

    /// The engine for http(s) URLs and whatever no rule claims.
    pub fn default_engine(&self) -> &str {
        &self.default_engine
    }

    /// Whether `engine` may open new views.
    pub fn is_enabled(&self, engine: &str) -> bool {
        self.enabled
            .as_ref()
            .is_none_or(|enabled| enabled.iter().any(|name| name == engine))
    }

    /// The routing rules, by engine name.
    pub fn routes(&self) -> &[EngineRoutes] {
        &self.routes
    }

    /// The key bindings.
    pub fn keys(&self) -> &Bindings {
        &self.keys
    }

    /// The `[ui]` section, for the chrome to interpret.
    pub fn ui(&self) -> &toml::Table {
        &self.ui
    }
}

fn toml_error(error: toml::de::Error) -> ConfigError {
    ConfigError::one(error.to_string().trim_end())
}

/// Reads each part of the user's file on its own, so that a misspelt or mistyped key in one
/// doesn't hide the problems of the others. A part with such a problem is reported and keeps
/// its default.
fn sections(mut table: toml::Table, problems: &mut Vec<String>) -> Raw {
    fn section<T: DeserializeOwned>(
        table: &mut toml::Table,
        key: &str,
        problems: &mut Vec<String>,
    ) -> Option<T> {
        let value = table.remove(key)?;
        let name = match value.is_table() {
            true => format!("[{key}]"),
            false => key.to_owned(),
        };
        let error = |error: toml::de::Error| {
            problems.push(format!("{name}: {}", error.to_string().trim_end()));
        };
        value.try_into().map_err(error).ok()
    }
    table.remove("version");
    let raw = Raw {
        version: Some(VERSION),
        home: section(&mut table, "home", problems),
        search: section(&mut table, "search", problems),
        engines: section(&mut table, "engines", problems).unwrap_or_default(),
        routing: section(&mut table, "routing", problems).unwrap_or_default(),
        keys: section(&mut table, "keys", problems),
        ui: section(&mut table, "ui", problems),
    };
    for key in table.keys() {
        problems.push(match key.as_str() {
            "default_engine" => "default_engine: it is `default` in [engines] now".into(),
            _ => format!("unknown key `{key}`"),
        });
    }
    raw
}

/// `user`'s settings over `defaults`.
fn merge(defaults: Raw, user: Raw) -> Raw {
    let mut routing = defaults.routing;
    for (engine, user) in user.routing {
        match routing.entry(engine) {
            Entry::Vacant(entry) => _ = entry.insert(user),
            Entry::Occupied(mut entry) => {
                let base = entry.get_mut();
                base.schemes = user.schemes.or(base.schemes.take());
                base.extensions = user.extensions.or(base.extensions.take());
                base.origins = user.origins.or(base.origins.take());
            }
        }
    }
    Raw {
        version: user.version,
        home: user.home.or(defaults.home),
        search: user.search.or(defaults.search),
        engines: RawEngines {
            default: user.engines.default.or(defaults.engines.default),
            enabled: user.engines.enabled.or(defaults.engines.enabled),
        },
        routing,
        keys: user.keys,
        ui: user.ui,
    }
}

/// Checks everything in `raw` and turns it into a [`Config`], unless it or reading it
/// (`problems`) found a problem.
fn validate(raw: Raw, mut problems: Vec<String>) -> Result<Config, ConfigError> {
    let home = raw.home.unwrap_or_default();
    if let Err(error) = url::Url::parse(&home) {
        problems.push(format!("home: {home:?} is not a URL ({error})"));
    }
    let search = raw.search.unwrap_or_default();
    let example = search.replace("{}", "sighurt");
    if !search.contains("{}") {
        problems.push(format!("search: {search:?} has no {{}} for the query"));
    } else if !url::Url::parse(&example).is_ok_and(|url| matches!(url.scheme(), "http" | "https")) {
        problems.push(format!("search: {search:?} is not an http(s) URL"));
    }

    let default_engine = raw.engines.default.unwrap_or_default();
    check_name("[engines] default", &default_engine, &mut problems);
    if let Some(enabled) = &raw.engines.enabled {
        for (i, name) in enabled.iter().enumerate() {
            check_name("[engines] enabled", name, &mut problems);
            if enabled[..i].contains(name) {
                problems.push(format!("[engines] enabled: {name:?} is listed twice"));
            }
        }
        if !enabled.contains(&default_engine) {
            problems.push(format!(
                "[engines] enabled: the default engine {default_engine:?} must be enabled"
            ));
        }
    }

    let mut routes = Vec::new();
    // Who claimed each scheme and extension, to find the ones claimed twice.
    let mut schemes = BTreeMap::new();
    let mut extensions = BTreeMap::new();
    for (engine, raw) in raw.routing {
        let section = format!("[routing.{engine}]");
        check_name(&section, &engine, &mut problems);
        let lower = |list: Option<Vec<String>>| {
            list.unwrap_or_default()
                .into_iter()
                .map(|item| item.trim().to_lowercase())
                .collect::<Vec<_>>()
        };
        let route = EngineRoutes {
            schemes: lower(raw.schemes),
            extensions: lower(raw.extensions)
                .into_iter()
                .map(|e| e.trim_start_matches('.').to_owned())
                .collect(),
            origins: raw
                .origins
                .unwrap_or_default()
                .iter()
                .filter_map(|origin| match web_origin(origin) {
                    Ok(origin) => Some(origin),
                    Err(error) => {
                        problems.push(format!("{section} origins: {error}"));
                        None
                    }
                })
                .collect(),
            engine,
        };
        for scheme in &route.schemes {
            if FIXED_SCHEMES.contains(&scheme.as_str()) {
                problems.push(format!(
                    "{section} schemes: {scheme:?} can't be routed (sig, http, https and file \
                     have fixed rules)"
                ));
            } else if !is_scheme(scheme) {
                problems.push(format!("{section} schemes: {scheme:?} is not a URL scheme"));
            }
            claim(&mut schemes, "scheme", scheme, &route.engine, &mut problems);
        }
        for extension in &route.extensions {
            let valid = !extension.is_empty()
                && extension
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_');
            if !valid {
                problems.push(format!(
                    "{section} extensions: {extension:?} is not a file extension"
                ));
            }
            claim(
                &mut extensions,
                "extension",
                extension,
                &route.engine,
                &mut problems,
            );
        }
        if !route.origins.is_empty() && route.extensions.is_empty() {
            problems.push(format!(
                "{section} origins: they need `extensions`, the files they open"
            ));
        }
        routes.push(route);
    }

    let keys = Bindings::from_table(&raw.keys.unwrap_or_default(), &mut problems);
    let ui = raw.ui.unwrap_or_default();
    check_ui(&ui, &mut problems);

    if !problems.is_empty() {
        return Err(ConfigError { problems });
    }
    Ok(Config {
        home,
        search,
        default_engine,
        enabled: raw.engines.enabled,
        routes,
        keys,
        ui,
    })
}

/// Engine names are lower-case letters, digits and dashes, starting with a letter.
fn check_name(section: &str, name: &str, problems: &mut Vec<String>) {
    let mut chars = name.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !valid {
        problems.push(format!("{section}: {name:?} is not an engine name"));
    }
}

/// Whether `scheme` is a (lower-case) URL scheme.
fn is_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "+-.".contains(c))
}

/// Records that `engine` claims `item`, or reports that another engine already does.
fn claim(
    claims: &mut BTreeMap<String, String>,
    kind: &str,
    item: &str,
    engine: &str,
    problems: &mut Vec<String>,
) {
    match claims.entry(item.to_owned()) {
        Entry::Vacant(entry) => _ = entry.insert(engine.to_owned()),
        Entry::Occupied(entry) if entry.get() == engine => {}
        Entry::Occupied(entry) => problems.push(format!(
            "[routing] the {kind} {item:?} belongs to both {} and {engine}",
            entry.get()
        )),
    }
}

/// `text` as a web origin, `scheme://host[:port]`, or why it isn't one.
fn web_origin(text: &str) -> Result<String, String> {
    let url = url::Url::parse(text).map_err(|error| format!("{text:?} is not a URL ({error})"))?;
    let bare = url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none();
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() || !bare {
        return Err(format!(
            "{text:?} is not an http(s) origin such as \"http://localhost:8080\""
        ));
    }
    Ok(url.origin().ascii_serialization())
}

/// `[ui]` is the chrome's, and holds only its colors, whose format is checked here. What an
/// older sig kept in `[ui]` is reported with where it went.
fn check_ui(ui: &toml::Table, problems: &mut Vec<String>) {
    for key in ui.keys() {
        match key.as_str() {
            "colors" => {}
            "home" | "search" => problems.push(format!(
                "[ui] {key}: it is a top-level setting now, `{key} = ...` before the first table"
            )),
            "keys" => problems.push(
                "[ui.keys]: key bindings are in [keys], [keys.page] and [keys.location] now".into(),
            ),
            other => problems.push(format!(
                "[ui] {other}: unknown key (the chrome reads only [ui.colors])"
            )),
        }
    }
    let Some(colors) = ui.get("colors") else {
        return;
    };
    let Some(colors) = colors.as_table() else {
        return problems.push("[ui] colors: expected a [ui.colors] table".into());
    };
    for (name, value) in colors {
        let valid = value
            .as_str()
            .and_then(|v| v.strip_prefix('#'))
            .is_some_and(|hex| {
                matches!(hex.len(), 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit())
            });
        if !valid {
            problems.push(format!(
                "[ui.colors] {name}: expected \"#rrggbb\" or \"#rrggbbaa\", not {value}"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use sighurt_api::commands::Command;

    use super::*;

    fn parse(user: &str) -> Result<Config, ConfigError> {
        Config::parse(Some(user))
    }

    fn problems(user: &str) -> Vec<String> {
        parse(user).expect_err("invalid").problems
    }

    fn assert_problem(user: &str, part: &str) {
        let problems = problems(user);
        assert!(
            problems.iter().any(|p| p.contains(part)),
            "{part:?} not in {problems:#?}"
        );
    }

    #[test]
    fn defaults_parse() {
        let config = Config::default();
        assert_eq!(config.home(), "sig://home");
        assert!(config.search().contains("{}"));
        assert_eq!(config.default_engine(), "servo");
        assert!(config.is_enabled("servo") && config.is_enabled("demo"));
        let wasm = config.routes().iter().find(|r| r.engine == "wasm").unwrap();
        assert_eq!(wasm.extensions, ["wasm"]);
        assert!(wasm.origins.is_empty());
        assert_eq!(config.keys(), &Bindings::default(), "no default bindings");
        assert!(config.ui().is_empty());
        assert_eq!(parse("version = 1"), Ok(config));
    }

    #[test]
    fn the_default_file_documents_every_command() {
        for name in Command::NAMES {
            assert!(DEFAULT_CONFIG.contains(name), "{name} is not documented");
        }
    }

    #[test]
    fn the_documented_examples_are_valid() {
        // The commented-out examples in the default file, uncommented.
        let examples: String = DEFAULT_CONFIG
            .lines()
            .filter_map(|line| line.strip_prefix("#   "))
            .filter(|line| line.starts_with(['[', '"']) || line.contains(" = "))
            .filter_map(|line| line.split("  #").next())
            .map(|line| format!("{line}\n"))
            .collect();
        let config = parse(&format!("version = 1\n{examples}")).unwrap();
        assert_eq!(
            config.ui()["colors"]["selection"].as_str(),
            Some("#60a5fa55")
        );
        let enabled = "version = 1\n[engines]\nenabled = [\"servo\", \"wasm\"]\n";
        assert!(!parse(enabled).unwrap().is_enabled("demo"));
        let origins = "version = 1\n[routing.wasm]\norigins = [\"http://localhost:8080\"]\n";
        let config = parse(origins).unwrap();
        let wasm = config.routes().iter().find(|r| r.engine == "wasm").unwrap();
        assert_eq!(wasm.origins, ["http://localhost:8080"]);
    }

    #[test]
    fn user_values_merge_over_defaults() {
        let config = parse(
            r#"
            version = 1
            home = "https://example.com/"
            [engines]
            default = "wasm"
            [routing.servo]
            extensions = [".HTML", "Txt"]
            [routing.demo]
            schemes = ["Demo"]
            "#,
        )
        .unwrap();
        assert_eq!(config.home(), "https://example.com/");
        assert_eq!(config.search(), Config::default().search());
        assert_eq!(config.default_engine(), "wasm");
        let servo = config
            .routes()
            .iter()
            .find(|r| r.engine == "servo")
            .unwrap();
        // Fields left out keep their defaults; lists are normalized.
        assert_eq!(servo.schemes, ["data", "about"]);
        assert_eq!(servo.extensions, ["html", "txt"]);
        let demo = config.routes().iter().find(|r| r.engine == "demo").unwrap();
        assert_eq!(demo.schemes, ["demo"]);
    }

    /// A configuration of the GPUI-based sig.
    const OLD_FORMAT: &str = r##"
        default_engine = "servo"
        [engines.wasm]
        command = ["sig-wasm"]
        extensions = ["wasm"]
        [ui]
        home = "https://example.com/"
        search = "https://s.test/?q={}"
        [ui.keys]
        "secondary-t" = "tab.new"
        [ui.colors]
        bg = "#000000"
    "##;

    #[test]
    fn the_version_is_required() {
        assert_eq!(problems(""), problems(OLD_FORMAT));
        assert_problem(OLD_FORMAT, "needs `version = 1`");
        assert_problem(OLD_FORMAT, "older sig");
        assert_problem("version = 2", "version 2");
        assert_problem("version = \"1\"", "version \"1\"");
    }

    #[test]
    fn an_old_file_with_a_version_says_what_moved() {
        let problems = problems(&format!("version = 1\n{OLD_FORMAT}"));
        let expected = [
            "default_engine: it is `default` in [engines] now",
            "[engines]: unknown field `wasm`",
            "[ui] home: it is a top-level setting now",
            "[ui] search: it is a top-level setting now",
            "[ui.keys]: key bindings are in [keys]",
        ];
        for part in expected {
            assert!(
                problems.iter().any(|p| p.contains(part)),
                "{part:?} not in {problems:#?}"
            );
        }
        assert_eq!(problems.len(), expected.len(), "{problems:#?}");
    }

    #[test]
    fn typos_are_errors() {
        assert_problem("version = 1\nhme = \"x\"", "unknown key `hme`");
        assert_problem(
            "version = 1\n[engines.servo]\ncommand = [\"sig-servo\"]",
            "servo",
        );
        assert_problem(
            "version = 1\n[routing.wasm]\nextension = [\"x\"]",
            "extension",
        );
        assert_problem("version = 1\nhome = 3", "home: invalid type");
        assert_problem("version = 1\nhome = [", "TOML parse error");
        // A broken part doesn't hide the others' problems.
        let problems = problems(
            "version = 1\nhme = 1\n[engines]\ndefualt = \"servo\"\n[keys]\n\"ctrl-t\" = \"tab.nope\"",
        );
        assert_eq!(problems.len(), 3, "{problems:#?}");
    }

    #[test]
    fn every_problem_is_reported() {
        let problems = problems(
            r#"
            version = 1
            home = "not a url"
            search = "https://s.test/?q="
            [engines]
            default = "Servo"
            enabled = ["wasm", "wasm"]
            [routing.demo]
            schemes = ["https", "data", "a b"]
            extensions = ["wasm", "tar.gz"]
            origins = ["http://localhost:8080/app/"]
            [keys]
            "ctrl-t" = "tab.nope"
            [ui.colors]
            bg = "black"
            "#,
        );
        let expected = [
            "home:",
            "no {} for the query",
            "\"Servo\" is not an engine name",
            "listed twice",
            "must be enabled",
            "\"https\" can't be routed",
            "the scheme \"data\" belongs to both demo and servo",
            "\"a b\" is not a URL scheme",
            "the extension \"wasm\" belongs to both demo and wasm",
            "\"tar.gz\" is not a file extension",
            "is not an http(s) origin",
            "unknown command",
            "[ui.colors] bg",
        ];
        for part in expected {
            assert!(
                problems.iter().any(|p| p.contains(part)),
                "{part:?} not in {problems:#?}"
            );
        }
        assert_eq!(problems.len(), expected.len(), "{problems:#?}");
    }

    #[test]
    fn origins_need_extensions() {
        assert_problem(
            "version = 1\n[routing.demo]\norigins = [\"https://a.test\"]",
            "need `extensions`",
        );
    }

    #[test]
    fn keys_and_ui_are_the_users_alone() {
        let config = parse(
            r##"
            version = 1
            [keys]
            "ctrl-t" = "tab.new"
            [ui.colors]
            bg = "#000000"
            "##,
        )
        .unwrap();
        assert_ne!(config.keys(), &Bindings::default());
        assert_eq!(config.ui()["colors"]["bg"].as_str(), Some("#000000"));
        assert!(parse("version = 1\n[ui]\ncolors = 3").is_err());
        assert_problem(
            "version = 1\n[ui]\nfont = \"Inter\"",
            "[ui] font: unknown key",
        );
    }
}
