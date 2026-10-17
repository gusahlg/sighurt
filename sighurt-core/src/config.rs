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
//! [`Config::parse`] checks the file against the build it runs in ([`Environment`]): its
//! keymaps and chromes, whose own checks read their tables (`[keys]` and `[keymap.<name>]`,
//! `[chrome.<name>]`), handed over as `sighurt_api::settings`, and the chrome's color names. A
//! table for a keymap, chrome or engine the build doesn't have is not an error, so that one file
//! serves several builds: it is accepted with a notice ([`Config::notices`]) that names what the
//! build has, and so is a key table no keymap reads. Only the user's file gets these notices.
//! The routing rules of an engine the build lacks are checked and left out, so what they claim
//! goes to the default engine; the user's `[engines] default` must name an engine of the build.
//!
//! `[input] keymap` chooses the keymap (the core's own `bindings`, which reads `[keys]`, unless
//! it names another); `[input] scroll-speed` scales the scrolling pages get, and
//! `[input.buttons]` binds the mouse's side buttons to commands. `[ui]` is what every chrome
//! shares: which chrome draws the window (`chrome`, used from the next start), its font and
//! size, and `[ui.colors]`.
//!
//! `[tui]` is the terminal's: how it shows pages, and its keys ([`TuiConfig`]). The windowed
//! frontend accepts the table and does not use it, so one file serves both. `[tui.keys]`, the
//! user's and the built-in one, is checked against the terminal chrome's commands in either
//! frontend. `[keys]`, `[keymap.<name>]` and `[input.buttons]` are checked against the chrome
//! of the frontend that is running ([`Environment::frontend`]). Every `[tui]` field applies
//! when the file is loaded.
//!
//! `zoom` is the page zoom new tabs start at and `page.zoom-reset` returns to, and
//! `[keywords]` names search templates for the address line ([`crate::router::fixup`]).
//! `[engines.<engine>]` holds an engine's settings from a small neutral vocabulary
//! ([`EngineSettings`]), each checked here for its form, and against what the build says that
//! engine honors (`Environment::engine_settings`): setting what an engine can't honor is an
//! error, never ignored. `[downloads] directory` and `[session] restore` are the executable's,
//! checked here.
//!
//! Must not own: reading or watching files, which the executable does, or deciding what a
//! reload means for open pages ([`crate::state`]).

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Deserialize;
use sighurt_api::commands::{Command, CommandSet};
use sighurt_api::input::PointerButton;
use sighurt_api::keymap::KeymapKind;
use sighurt_api::settings::{Table, Value};

use crate::commands::ZOOM_STEPS;
use crate::keys;
use crate::state::{Environment, Frontend};

/// The built-in configuration, which also documents the format.
pub const DEFAULT_CONFIG: &str = include_str!("default-config.toml");

/// The format version this code reads.
pub const VERSION: i64 = 1;

/// The chrome of `[ui] chrome` when the file names none: sighurt-ui's, in every build.
pub const DEFAULT_CHROME: &str = "standard";

/// The terminal chrome's `chrome.<action>` commands, without the `chrome.` prefix.
///
/// The terminal frontend has no chrome crate of its own yet, so the core checks bindings
/// against this list when the running frontend is the terminal, and whenever it reads
/// `[tui.keys]` (that table is the terminal's, in either frontend).
pub const TERMINAL_CHROME_COMMANDS: &[&str] =
    &["toggle-mode", "text-mode", "graphics-mode", "refresh"];

/// The chrome's font size, in logical pixels, when `[ui] font-size` is left out.
pub const DEFAULT_FONT_SIZE: u16 = 14;

/// Schemes whose routing is fixed: `sig` is Sighurt's, http(s) and file follow the rules for
/// them.
const FIXED_SCHEMES: [&str; 4] = ["sig", "http", "https", "file"];

/// How one setting of `[engines.<engine>]` is read into [`EngineSettings`].
type ReadSetting = fn(&toml::Value, &mut EngineSettings) -> Result<(), String>;

/// The settings `[engines.<engine>]` may hold, by name, each with how it is read. An engine of
/// the build honors those `Environment::engine_settings` lists for it, so a new setting is a
/// line here, a field of [`EngineSettings`], and the names in the registry of the engines that
/// honor it.
const ENGINE_SETTINGS: [(&str, ReadSetting); 2] = [
    ("user-agent", |value, settings| {
        // Printable ASCII is what an HTTP header may hold; Servo's network code unwraps the
        // parse of its user agent as a header.
        let printable = |text: &str| text.bytes().all(|b| (b' '..=b'~').contains(&b));
        match value.as_str() {
            Some(agent) if (1..=512).contains(&agent.len()) && printable(agent) => {
                settings.user_agent = Some(agent.to_owned());
                Ok(())
            }
            _ => Err(format!(
                "expected 1 to 512 printable ASCII characters, not {value}"
            )),
        }
    }),
    ("storage", |value, settings| {
        settings.storage = match value.as_str() {
            Some("profile") => Storage::Profile,
            Some("ephemeral") => Storage::Ephemeral,
            _ => {
                return Err(format!(
                    "expected \"profile\" or \"ephemeral\", not {value}"
                ))
            }
        };
        Ok(())
    }),
];

/// What `[engines.<engine>]` sets for one engine. An engine of the build is started with these
/// once, so they apply when `sig` starts; the configuration never holds one the engine can't
/// honor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineSettings {
    /// `user-agent`: the user agent the engine's requests send and its pages see, instead of
    /// its own.
    pub user_agent: Option<String>,
    /// `storage`: whether the engine keeps cookies and site data.
    pub storage: Storage,
}

/// Where an engine keeps cookies, site data and caches (`[engines.<engine>] storage`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Storage {
    /// In its profile, from one start of `sig` to the next.
    #[default]
    Profile,
    /// Nowhere that outlives `sig`: nothing is kept.
    Ephemeral,
}

/// `[tui]`: the terminal frontend's settings. The windowed frontend accepts the table and
/// does not use it, so one file serves both. Every field applies when the file is loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TuiConfig {
    /// `mode`: how the terminal shows pages.
    pub mode: TuiMode,
    /// `graphics`: which graphics protocol to use when the mode shows pixels.
    pub graphics: GraphicsProtocol,
    /// `fps`: page frames a second in graphics mode, from 1 to 60.
    pub fps: u32,
    /// `mouse`: whether the terminal's mouse is sent to the page.
    pub mouse: bool,
    /// `default-keys`: whether the built-in terminal key table is installed under `[keys]`.
    pub default_keys: bool,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            mode: TuiMode::Auto,
            graphics: GraphicsProtocol::Auto,
            fps: 10,
            mouse: true,
            default_keys: true,
        }
    }
}

/// How the terminal shows pages (`[tui] mode`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TuiMode {
    /// `"auto"`: graphics where the terminal can show them, text otherwise.
    #[default]
    Auto,
    /// `"text"`: reflowed text.
    Text,
    /// `"graphics"`: the page's pixels.
    Graphics,
}

/// Which graphics protocol the terminal uses (`[tui] graphics`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GraphicsProtocol {
    /// `"auto"`: kitty where the terminal speaks it, otherwise sixel, otherwise text.
    #[default]
    Auto,
    /// `"kitty"`.
    Kitty,
    /// `"sixel"`.
    Sixel,
    /// `"none"`: text only.
    None,
}

/// A complete, valid configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    home: String,
    search: String,
    zoom: f64,
    keywords: BTreeMap<String, String>,
    default_engine: String,
    enabled: Option<Vec<String>>,
    /// `[engines.<engine>]` of each engine that has one.
    engines: BTreeMap<String, EngineSettings>,
    routes: Vec<EngineRoutes>,
    /// The keymap that reads the keyboard.
    pub(crate) keymap: KeymapKind,
    /// Its table: `[keys]` for `bindings`, `[keymap.<name>]` for another.
    pub(crate) keymap_settings: Table,
    scroll_speed: f64,
    /// `[input.buttons]`: the side buttons bound to commands.
    buttons: Vec<(PointerButton, Command)>,
    chrome: String,
    font: Option<String>,
    font_size: u16,
    colors: BTreeMap<String, String>,
    /// `[chrome.<name>]` of each chrome of the build that has one.
    chrome_settings: BTreeMap<String, Table>,
    downloads: Option<PathBuf>,
    restore_session: bool,
    /// `[tui]`. The windowed frontend leaves it unused.
    tui: TuiConfig,
    notices: Vec<String>,
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
    /// The hosts whose http(s) URLs it opens, as patterns: a host (`docs.example.com`, an IP
    /// address), or `*.` and a host name for every host under that one. Lower-case and
    /// IDNA-normalized, as the `url` crate normalizes a URL's host.
    pub hosts: Vec<String>,
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
    zoom: Option<f64>,
    keywords: Option<toml::Table>,
    #[serde(default)]
    engines: RawEngines,
    #[serde(default)]
    routing: BTreeMap<String, RawRoutes>,
    #[serde(default)]
    input: RawInput,
    keys: Option<toml::Table>,
    #[serde(default)]
    keymap: BTreeMap<String, toml::Table>,
    ui: Option<toml::Table>,
    #[serde(default)]
    chrome: BTreeMap<String, toml::Table>,
    #[serde(default)]
    downloads: RawDownloads,
    #[serde(default)]
    session: RawSession,
    /// `[tui]`, including `[tui.keys]` until that table is split out.
    #[serde(default)]
    tui: toml::Table,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawEngines {
    default: Option<String>,
    enabled: Option<Vec<String>>,
    /// `[engines.<engine>]`, by engine: the user's alone (the built-in file sets none).
    #[serde(skip)]
    settings: BTreeMap<String, toml::Table>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawDownloads {
    directory: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawSession {
    restore: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawRoutes {
    schemes: Option<Vec<String>>,
    extensions: Option<Vec<String>>,
    origins: Option<Vec<String>>,
    hosts: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawInput {
    keymap: Option<String>,
    #[serde(rename = "scroll-speed")]
    scroll_speed: Option<f64>,
    #[serde(default)]
    buttons: RawButtons,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawButtons {
    back: Option<String>,
    forward: Option<String>,
}

impl Default for Config {
    /// The built-in configuration, for a build with no engines: so with no routing rules.
    fn default() -> Self {
        Self::parse(None, &Environment::default()).expect("the built-in configuration is valid")
    }
}

impl Config {
    /// Parses `user`, the text of the user's file (`None` if there is none), over
    /// [`DEFAULT_CONFIG`], for a build with what `env` lists.
    pub fn parse(user: Option<&str>, env: &Environment) -> Result<Self, ConfigError> {
        let mut defaults: Raw = toml::from_str(DEFAULT_CONFIG).map_err(toml_error)?;
        let builtin_keys = take_keys(&mut defaults.tui);
        let Some(user) = user else {
            return validate(defaults, builtin_keys, None, env, Vec::new(), Vec::new());
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
        let mut user = sections(table, &mut problems);
        let default = user
            .engines
            .default
            .as_deref()
            .filter(|name| is_engine_name(name));
        if let Some(engine) = default.filter(|engine| !env.engines.contains_key(*engine)) {
            let has = list(env.engines.keys().map(String::as_str));
            problems.push(format!(
                "[engines] default: this build has no engine {engine:?} (it has: {has})"
            ));
        }
        let notices = engine_notices(&user, env);
        let user_keys = take_keys(&mut user.tui);
        validate(
            merge(defaults, user),
            builtin_keys,
            user_keys,
            env,
            problems,
            notices,
        )
    }

    /// The page shown at startup and in new tabs: an absolute URL.
    pub fn home(&self) -> &str {
        &self.home
    }

    /// The search URL template, with `{}` where the query goes.
    pub fn search(&self) -> &str {
        &self.search
    }

    /// `zoom`: the page zoom new tabs start at and `page.zoom-reset` returns to, 0.5 to 3
    /// (1.0 is 100 %). Open tabs keep their own.
    pub fn zoom(&self) -> f64 {
        self.zoom
    }

    /// `[keywords]`: search templates by keyword, each an http(s) URL with `{}` where the query
    /// goes, or without one to open as it is.
    pub fn keywords(&self) -> &BTreeMap<String, String> {
        &self.keywords
    }

    /// The engine for http(s) URLs and whatever no rule claims.
    pub fn default_engine(&self) -> &str {
        &self.default_engine
    }

    /// `[engines.<engine>]`: what `engine` is started with. Only settings the engine honors are
    /// ever set.
    pub fn engine_settings(&self, engine: &str) -> EngineSettings {
        self.engines.get(engine).cloned().unwrap_or_default()
    }

    /// Whether `engine` may open new views.
    pub fn is_enabled(&self, engine: &str) -> bool {
        self.enabled
            .as_ref()
            .is_none_or(|enabled| enabled.iter().any(|name| name == engine))
    }

    /// The routing rules, by engine name: those of the build's engines.
    pub fn routes(&self) -> &[EngineRoutes] {
        &self.routes
    }

    /// `[input] scroll-speed`: how many times as far pages scroll as the wheel or touchpad
    /// says, 0.1 to 10.
    pub fn scroll_speed(&self) -> f64 {
        self.scroll_speed
    }

    /// The command `[input.buttons]` binds `button` to, if any: the side buttons (`Back`,
    /// `Forward`) only.
    pub fn button(&self, button: PointerButton) -> Option<&Command> {
        let mut buttons = self.buttons.iter();
        buttons
            .find(|(b, _)| *b == button)
            .map(|(_, command)| command)
    }

    /// The chrome that draws the window: `[ui] chrome`, a chrome of the build. It is chosen
    /// when `sig` starts.
    pub fn chrome(&self) -> &str {
        &self.chrome
    }

    /// `[ui] font`: the family the chrome's text is set in, if the file names one. It need not
    /// be installed.
    pub fn font(&self) -> Option<&str> {
        self.font.as_deref()
    }

    /// `[ui] font-size`: the chrome's text size in logical pixels, 9 to 32.
    pub fn font_size(&self) -> u16 {
        self.font_size
    }

    /// `[ui.colors]`: the chrome's colors the file sets, by name, each `#rrggbb` or `#rrggbbaa`
    /// and named as the build's chrome names them.
    pub fn colors(&self) -> &BTreeMap<String, String> {
        &self.colors
    }

    /// `[chrome.<chrome>]`, which `chrome`'s check accepted; `None` if the file has none.
    pub fn chrome_settings(&self, chrome: &str) -> Option<&Table> {
        self.chrome_settings.get(chrome)
    }

    /// `[downloads] directory`: where downloads go, an absolute path (`~/` expanded with the
    /// home directory), or `None` for the desktop's download folder.
    pub fn downloads(&self) -> Option<&Path> {
        self.downloads.as_deref()
    }

    /// `[session] restore`: whether `sig` restores the saved tabs at start and saves them. Off,
    /// it starts with the home page and leaves the session file as it is.
    pub fn restore_session(&self) -> bool {
        self.restore_session
    }

    /// `[tui]`: the terminal's settings. The windowed frontend accepts them and does not use
    /// them.
    pub fn tui(&self) -> &TuiConfig {
        &self.tui
    }

    /// What the user should know about the file although it is valid: tables for keymaps,
    /// chromes and engines this build doesn't have, and key tables no keymap reads. One line
    /// each.
    pub fn notices(&self) -> &[String] {
        &self.notices
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
    // The tables the core hands on are taken as TOML parsed them, not through serde, which
    // turns dates into strings.
    fn user_table(
        table: &mut toml::Table,
        key: &str,
        problems: &mut Vec<String>,
    ) -> Option<toml::Table> {
        match table.remove(key)? {
            toml::Value::Table(table) => Some(table),
            other => {
                problems.push(format!("{key}: expected a [{key}] table, not {other}"));
                None
            }
        }
    }
    fn user_tables(
        table: &mut toml::Table,
        key: &str,
        problems: &mut Vec<String>,
    ) -> BTreeMap<String, toml::Table> {
        let tables = user_table(table, key, problems).unwrap_or_default();
        let mut found = BTreeMap::new();
        for (name, value) in tables {
            match value {
                toml::Value::Table(table) => _ = found.insert(name, table),
                other => problems.push(format!(
                    "[{key}] {name}: expected a [{key}.{name}] table, not {other}"
                )),
            }
        }
        found
    }
    // `[engines]`'s own keys, and its tables, `[engines.<engine>]`, apart.
    fn engines(table: &mut toml::Table, problems: &mut Vec<String>) -> RawEngines {
        let Some(mut engines) = user_table(table, "engines", problems) else {
            return RawEngines::default();
        };
        let names: Vec<String> = engines
            .iter()
            .filter(|(_, value)| value.is_table())
            .map(|(name, _)| name.clone())
            .collect();
        let mut settings = BTreeMap::new();
        for name in names {
            if let Some(toml::Value::Table(table)) = engines.remove(&name) {
                settings.insert(name, table);
            }
        }
        let raw = toml::Value::Table(engines).try_into().map_err(|e| {
            problems.push(format!("[engines]: {}", e.to_string().trim_end()));
        });
        RawEngines {
            settings,
            ..raw.unwrap_or_default()
        }
    }
    table.remove("version");
    let raw = Raw {
        version: Some(VERSION),
        home: section(&mut table, "home", problems),
        search: section(&mut table, "search", problems),
        zoom: section(&mut table, "zoom", problems),
        keywords: user_table(&mut table, "keywords", problems),
        engines: engines(&mut table, problems),
        routing: section(&mut table, "routing", problems).unwrap_or_default(),
        input: section(&mut table, "input", problems).unwrap_or_default(),
        keys: user_table(&mut table, "keys", problems),
        keymap: user_tables(&mut table, "keymap", problems),
        ui: user_table(&mut table, "ui", problems),
        chrome: user_tables(&mut table, "chrome", problems),
        downloads: section(&mut table, "downloads", problems).unwrap_or_default(),
        session: section(&mut table, "session", problems).unwrap_or_default(),
        tui: user_table(&mut table, "tui", problems).unwrap_or_default(),
    };
    for key in table.keys() {
        problems.push(match key.as_str() {
            "default_engine" => "default_engine: it is `default` in [engines] now".into(),
            _ => format!("unknown key `{key}`"),
        });
    }
    raw
}

/// `user`'s settings over `defaults`. `[keywords]`, `[keys]`, `[keymap.*]`, `[ui]` and
/// `[chrome.*]` are the user's alone, and so is `[engines.*]`, which the built-in file can't
/// hold. `[tui]` merges key by key; `[tui.keys]` was split out of both tables first, because
/// the user's is a separate layer rather than a replacement of the built-in one.
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
                base.hosts = user.hosts.or(base.hosts.take());
            }
        }
    }
    Raw {
        version: user.version,
        home: user.home.or(defaults.home),
        search: user.search.or(defaults.search),
        zoom: user.zoom.or(defaults.zoom),
        keywords: user.keywords,
        engines: RawEngines {
            default: user.engines.default.or(defaults.engines.default),
            enabled: user.engines.enabled.or(defaults.engines.enabled),
            settings: user.engines.settings,
        },
        routing,
        input: RawInput {
            keymap: user.input.keymap.or(defaults.input.keymap),
            scroll_speed: user.input.scroll_speed.or(defaults.input.scroll_speed),
            buttons: RawButtons {
                back: user.input.buttons.back.or(defaults.input.buttons.back),
                forward: user
                    .input
                    .buttons
                    .forward
                    .or(defaults.input.buttons.forward),
            },
        },
        keys: user.keys,
        keymap: user.keymap,
        ui: user.ui,
        chrome: user.chrome,
        downloads: RawDownloads {
            directory: user.downloads.directory.or(defaults.downloads.directory),
        },
        session: RawSession {
            restore: user.session.restore.or(defaults.session.restore),
        },
        tui: merge_table(defaults.tui, user.tui),
    }
}

/// `user`'s keys over `base`'s. A key the user sets replaces the default; the rest stay.
fn merge_table(mut base: toml::Table, user: toml::Table) -> toml::Table {
    for (key, value) in user {
        base.insert(key, value);
    }
    base
}

/// Removes `[tui.keys]` from `tui` when it is a table. Anything else stays, so [`check_tui`]
/// can say it is not a table.
fn take_keys(tui: &mut toml::Table) -> Option<toml::Table> {
    match tui.remove("keys") {
        Some(toml::Value::Table(keys)) => Some(keys),
        Some(other) => {
            tui.insert("keys".into(), other);
            None
        }
        None => None,
    }
}

/// Notices about the engines the user's file names that the build doesn't have.
fn engine_notices(user: &Raw, env: &Environment) -> Vec<String> {
    let has = list(env.engines.keys().map(String::as_str));
    let missing = |engine: &&String| !env.engines.contains_key(engine.as_str());
    let table = |kind: &str, engine: &String| {
        format!("[{kind}.{engine}] is for an engine this build doesn't have (it has: {has})")
    };
    let settings = user.engines.settings.keys().filter(missing);
    let settings = settings.map(|engine| table("engines", engine));
    let routes = user.routing.keys().filter(missing);
    let routes =
        routes.map(|engine| format!("{}; its rules are not used", table("routing", engine)));
    let enabled = user.engines.enabled.iter().flatten().filter(missing);
    let enabled = enabled.map(|engine| {
        format!("[engines] enabled: this build has no engine {engine:?} (it has: {has})")
    });
    settings.chain(routes).chain(enabled).collect()
}

/// Checks everything in `raw` and turns it into a [`Config`] for a build with what `env`
/// lists, unless it or reading it (`problems`) found a problem. `builtin_keys` is the default
/// file's `[tui.keys]`; `user_tui_keys` is the user's, when they wrote one.
fn validate(
    raw: Raw,
    builtin_keys: Option<toml::Table>,
    user_tui_keys: Option<toml::Table>,
    env: &Environment,
    mut problems: Vec<String>,
    mut notices: Vec<String>,
) -> Result<Config, ConfigError> {
    let home = raw.home.unwrap_or_default();
    if let Err(error) = url::Url::parse(&home) {
        problems.push(format!("home: {home:?} is not a URL ({error})"));
    }
    let search = raw.search.unwrap_or_default();
    if !search.contains("{}") {
        problems.push(format!("search: {search:?} has no {{}} for the query"));
    } else if !is_web_template(&search) {
        problems.push(format!("search: {search:?} is not an http(s) URL"));
    }
    let zoom = raw.zoom.unwrap_or(1.0);
    let (least, most) = (ZOOM_STEPS[0], ZOOM_STEPS[ZOOM_STEPS.len() - 1]);
    if !(f64::from(least) / 100.0..=f64::from(most) / 100.0).contains(&zoom) {
        problems.push(format!(
            "zoom: expected a number from {} to {}, not {zoom}",
            f64::from(least) / 100.0,
            f64::from(most) / 100.0
        ));
    }
    let keywords = check_keywords(raw.keywords.unwrap_or_default(), &mut problems);

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
    let engines = check_engines(raw.engines.settings, env, &mut problems);

    let mut routes = Vec::new();
    // Who claimed each scheme, extension and host pattern, to find the ones claimed twice.
    let mut schemes = BTreeMap::new();
    let mut extensions = BTreeMap::new();
    let mut hosts = BTreeMap::new();
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
            hosts: raw
                .hosts
                .unwrap_or_default()
                .iter()
                .filter_map(|pattern| match host_pattern(pattern) {
                    Ok(pattern) => Some(pattern),
                    Err(error) => {
                        problems.push(format!("{section} hosts: {error}"));
                        None
                    }
                })
                .collect(),
            engine,
        };
        for pattern in &route.hosts {
            claim(&mut hosts, "host", pattern, &route.engine, &mut problems);
        }
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
        // An engine the build lacks opens nothing: what its rules claim goes where it would
        // without them, typed or clicked alike.
        if env.engines.contains_key(&route.engine) {
            routes.push(route);
        }
    }

    let ui = check_ui(raw.ui.unwrap_or_default(), env, &mut problems);
    let tui = check_tui(raw.tui, &mut problems);
    // `[keys]` and the buttons use the running frontend's chrome. `[tui.keys]` is the
    // terminal's in either frontend, so it is checked against the terminal chrome.
    let commands = running_commands(env, &ui.chrome);
    let terminal = CommandSet::new("terminal", TERMINAL_CHROME_COMMANDS);
    let user_wrote_tui_keys = user_tui_keys.is_some();
    let builtin_keys = read_keys(builtin_keys, &terminal, &mut problems);
    let user_tui_keys = read_keys(user_tui_keys, &terminal, &mut problems);
    let chrome_settings = check_chromes(raw.chrome, env, &mut problems, &mut notices);
    let scroll_speed = raw.input.scroll_speed.unwrap_or(1.0);
    if !(0.1..=10.0).contains(&scroll_speed) {
        problems.push(format!(
            "[input] scroll-speed: expected a number from 0.1 to 10, not {scroll_speed}"
        ));
    }
    let buttons = [
        (PointerButton::Back, "back", raw.input.buttons.back),
        (PointerButton::Forward, "forward", raw.input.buttons.forward),
    ];
    let buttons = buttons
        .into_iter()
        .filter_map(|(button, name, text)| {
            let text = text.filter(|text| !text.trim().is_empty())?;
            let command = commands.parse(&text).map_err(|e| {
                problems.push(format!("[input.buttons] {name} = {text:?}: {e}"));
            });
            Some((button, command.ok()?))
        })
        .collect();
    let keymaps = KeyTables {
        selected: raw.input.keymap,
        keys: raw.keys,
        tables: raw.keymap,
    };
    let (keymap, selected) = keymaps.check(env, &commands, &mut problems, &mut notices);
    let keymap_settings = if keymap == keys::KIND {
        let on_terminal = env.frontend == Frontend::Terminal;
        keys::combine(
            if on_terminal {
                user_tui_keys
            } else {
                Table::new()
            },
            selected,
            if on_terminal && tui.default_keys {
                builtin_keys
            } else {
                Table::new()
            },
        )
    } else {
        if env.frontend == Frontend::Terminal && user_wrote_tui_keys {
            notices.push(format!(
                "[tui.keys] is not used: [input] keymap is {:?}, which reads [keymap.{}]",
                keymap.name, keymap.name
            ));
        }
        selected
    };
    let downloads = raw.downloads.directory.and_then(|directory| {
        directory_path(&directory, env.home_dir.as_deref())
            .map_err(|e| problems.push(format!("[downloads] directory: {e}")))
            .ok()
    });

    if !problems.is_empty() {
        return Err(ConfigError { problems });
    }
    Ok(Config {
        home,
        search,
        zoom,
        keywords,
        default_engine,
        enabled: raw.engines.enabled,
        engines,
        routes,
        keymap,
        keymap_settings,
        scroll_speed,
        buttons,
        chrome: ui.chrome,
        font: ui.font,
        font_size: ui.font_size,
        colors: ui.colors,
        chrome_settings,
        downloads,
        restore_session: raw.session.restore.unwrap_or(true),
        tui,
        notices,
    })
}

/// The commands `[keys]` and `[input.buttons]` may bind for the frontend that is running.
fn running_commands<'a>(env: &'a Environment, chrome: &'a str) -> CommandSet<'a> {
    match env.frontend {
        Frontend::Terminal => CommandSet::new("terminal", TERMINAL_CHROME_COMMANDS),
        Frontend::Gui => {
            let found = env
                .chromes
                .iter()
                .find(|candidate| candidate.name == chrome);
            CommandSet::new(chrome, found.map_or(&[], |chrome| chrome.commands))
        }
    }
}

/// `[tui.keys]` as settings, checked against the terminal chrome. An absent table is empty.
fn read_keys(
    table: Option<toml::Table>,
    commands: &CommandSet<'_>,
    problems: &mut Vec<String>,
) -> Table {
    let settings = settings(table.unwrap_or_default(), "[tui.keys]", problems);
    let mut found = Vec::new();
    let _ = keys::Bindings::from_table(&settings, "tui.keys", commands, &mut found);
    problems.extend(found);
    settings
}

/// `[tui]`, key by key, so one bad value does not hide the others. A bad value keeps the
/// default and is reported; any problem rejects the whole configuration.
fn check_tui(table: toml::Table, problems: &mut Vec<String>) -> TuiConfig {
    let mut checked = TuiConfig::default();
    for (key, value) in table {
        match key.as_str() {
            "mode" => match value.as_str() {
                Some("auto") => checked.mode = TuiMode::Auto,
                Some("text") => checked.mode = TuiMode::Text,
                Some("graphics") => checked.mode = TuiMode::Graphics,
                _ => problems.push(format!(
                    "[tui] mode: expected \"auto\", \"text\" or \"graphics\", not {value}"
                )),
            },
            "graphics" => match value.as_str() {
                Some("auto") => checked.graphics = GraphicsProtocol::Auto,
                Some("kitty") => checked.graphics = GraphicsProtocol::Kitty,
                Some("sixel") => checked.graphics = GraphicsProtocol::Sixel,
                Some("none") => checked.graphics = GraphicsProtocol::None,
                _ => problems.push(format!(
                    "[tui] graphics: expected \"auto\", \"kitty\", \"sixel\" or \"none\", not \
                     {value}"
                )),
            },
            "fps" => match value.as_integer() {
                Some(fps @ 1..=60) => checked.fps = fps as u32,
                _ => problems.push(format!(
                    "[tui] fps: expected a whole number from 1 to 60, not {value}"
                )),
            },
            "mouse" => match value.as_bool() {
                Some(mouse) => checked.mouse = mouse,
                None => problems.push(format!("[tui] mouse: expected true or false, not {value}")),
            },
            "default-keys" => match value.as_bool() {
                Some(enabled) => checked.default_keys = enabled,
                None => problems.push(format!(
                    "[tui] default-keys: expected true or false, not {value}"
                )),
            },
            "keys" => problems.push(format!(
                "[tui] keys: expected a [tui.keys] table, not {value}"
            )),
            other => problems.push(format!(
                "[tui] {other}: unknown key ([tui] has mode, graphics, fps, mouse, default-keys \
                 and [tui.keys])"
            )),
        }
    }
    checked
}

/// Whether `template`, with `{}` filled in, is an http(s) URL.
fn is_web_template(template: &str) -> bool {
    let example = template.replace("{}", "sighurt");
    url::Url::parse(&example).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

/// `[keywords]`: names of 1 to 32 lower-case letters, digits and dashes (so never a host name
/// or a URL), but not `localhost`, each an http(s) URL template.
fn check_keywords(table: toml::Table, problems: &mut Vec<String>) -> BTreeMap<String, String> {
    let mut keywords = BTreeMap::new();
    for (name, value) in table {
        let word = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
        if !(1..=32).contains(&name.len()) || !name.chars().all(word) || name == "localhost" {
            problems.push(format!(
                "[keywords] {name:?}: a keyword is 1 to 32 lower-case letters, digits and dashes, \
                 and not localhost"
            ));
            continue;
        }
        match value.as_str() {
            Some(template) if is_web_template(template) => {
                keywords.insert(name, template.to_owned());
            }
            _ => problems.push(format!(
                "[keywords] {name}: expected an http(s) URL, with {{}} where the query goes, not \
                 {value}"
            )),
        }
    }
    keywords
}

/// `[engines.<engine>]` of each engine: the settings of [`ENGINE_SETTINGS`] that the build says
/// the engine honors, and for what an old file kept there, where it went. A table for an engine
/// the build doesn't have is checked for its form only; it gets a notice ([`engine_notices`]).
fn check_engines(
    tables: BTreeMap<String, toml::Table>,
    env: &Environment,
    problems: &mut Vec<String>,
) -> BTreeMap<String, EngineSettings> {
    let names = list(ENGINE_SETTINGS.iter().map(|(name, _)| *name));
    let mut checked = BTreeMap::new();
    for (engine, table) in tables {
        let section = format!("[engines.{engine}]");
        check_name(&section, &engine, problems);
        let honors = env
            .engine_settings
            .get(&engine)
            .copied()
            .unwrap_or_default();
        let in_build = env.engines.contains_key(&engine);
        let mut settings = EngineSettings::default();
        for (key, value) in &table {
            let setting = ENGINE_SETTINGS.iter().find(|(name, _)| name == key);
            let problem = match (setting, key.as_str()) {
                (Some(_), _) if in_build && !honors.contains(&key.as_str()) => format!(
                    "the {engine} engine can't honor it (it honors: {})",
                    list(honors.iter().copied())
                ),
                (Some((_, read)), _) => match read(value, &mut settings) {
                    Ok(()) => continue,
                    Err(problem) => problem,
                },
                // What a file of the GPUI-based sig kept here.
                (None, "command") => {
                    "engines are compiled into sig now, so there is no command to run".into()
                }
                (None, "schemes" | "extensions") => format!("they are in [routing.{engine}] now"),
                (None, "mime") => format!(
                    "there is no MIME probe any more; use [routing.{engine}] extensions and \
                     origins"
                ),
                (None, _) => format!("unknown key (an engine's settings are {names})"),
            };
            problems.push(format!("{section} {key}: {problem}"));
        }
        checked.insert(engine, settings);
    }
    checked
}

/// `[downloads] directory` as a path: absolute, or `~/…` in `home`.
fn directory_path(text: &str, home: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(rest) = text.strip_prefix("~/") {
        return home
            .map(|home| home.join(rest))
            .ok_or_else(|| format!("{text:?} is in the home directory, which sig doesn't know"));
    }
    match Path::new(text).is_absolute() {
        true => Ok(text.into()),
        false => Err(format!(
            "{text:?} is not an absolute path or one starting with ~/"
        )),
    }
}

/// `names`, comma-separated, or "none".
fn list<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let names: Vec<_> = names.collect();
    match names.is_empty() {
        true => "none".into(),
        false => names.join(", "),
    }
}

/// `table` as settings for the implementation it belongs to. A date anywhere in it is a
/// problem: settings have no dates.
pub(crate) fn settings(table: toml::Table, section: &str, problems: &mut Vec<String>) -> Table {
    fn convert(value: toml::Value) -> Option<Value> {
        Some(match value {
            toml::Value::String(text) => Value::String(text),
            toml::Value::Integer(number) => Value::Integer(number),
            toml::Value::Float(number) => Value::Float(number),
            toml::Value::Boolean(boolean) => Value::Boolean(boolean),
            toml::Value::Datetime(_) => return None,
            toml::Value::Array(values) => {
                Value::Array(values.into_iter().map(convert).collect::<Option<_>>()?)
            }
            toml::Value::Table(table) => Value::Table(
                table
                    .into_iter()
                    .map(|(key, value)| Some((key, convert(value)?)))
                    .collect::<Option<_>>()?,
            ),
        })
    }
    let mut settings = Table::new();
    for (key, value) in table {
        match convert(value) {
            Some(value) => _ = settings.insert(key, value),
            None => problems.push(format!("{section} {key}: dates are not settings")),
        }
    }
    settings
}

/// `problems` of a check of `section`, each prefixed with it.
fn prefixed(section: &str, problems: Vec<String>) -> impl Iterator<Item = String> + '_ {
    problems
        .into_iter()
        .map(move |problem| format!("{section} {problem}"))
}

/// The keymap tables of a file: `[input] keymap`, `[keys]` and `[keymap.<name>]`.
struct KeyTables {
    selected: Option<String>,
    keys: Option<toml::Table>,
    tables: BTreeMap<String, toml::Table>,
}

impl KeyTables {
    /// The keymap `[input] keymap` names and its table. Every key table is checked, the
    /// selected keymap's even when the file has none, and the others although no keymap reads
    /// them, so switching to them later is safe.
    fn check(
        self,
        env: &Environment,
        commands: &CommandSet<'_>,
        problems: &mut Vec<String>,
        notices: &mut Vec<String>,
    ) -> (KeymapKind, Table) {
        let bindings = keys::KIND;
        let has = list(std::iter::once(bindings.name).chain(env.keymaps.iter().map(|k| k.name)));
        let name = self.selected.unwrap_or_else(|| bindings.name.into());
        let reads = match name == bindings.name {
            true => "[keys]".to_owned(),
            false => format!("[keymap.{name}]"),
        };
        let unused = |table: &str| {
            format!("{table} is not used: [input] keymap is {name:?}, which reads {reads}")
        };
        let found = env.keymaps.iter().find(|kind| kind.name == name).copied();
        let kind = match found {
            Some(kind) => kind,
            None if name == bindings.name => bindings,
            None => {
                problems.push(format!(
                    "[input] keymap: this build has no keymap {name:?} (it has: {has})"
                ));
                bindings
            }
        };

        let keys_given = self.keys.is_some();
        let keys = settings(self.keys.unwrap_or_default(), "[keys]", problems);
        problems.extend((bindings.check)(&keys, commands));
        if keys_given && kind != bindings {
            notices.push(unused("[keys]"));
        }
        let mut selected = (kind == bindings).then_some(keys);
        for (table_name, table) in self.tables {
            let section = format!("[keymap.{table_name}]");
            if table_name == bindings.name {
                problems.push(format!(
                    "{section}: the bindings keymap reads [keys], [keys.page] and [keys.location]"
                ));
                continue;
            }
            let Some(other) = env.keymaps.iter().find(|kind| kind.name == table_name) else {
                notices.push(format!(
                    "{section} is for a keymap this build doesn't have (it has: {has}); it was \
                     not checked"
                ));
                continue;
            };
            let table = settings(table, &section, problems);
            problems.extend(prefixed(&section, (other.check)(&table, commands)));
            match *other == kind {
                true => selected = Some(table),
                false => notices.push(unused(&section)),
            }
        }
        // A keymap the file selects without a table of its own still says what it misses.
        let selected = selected.unwrap_or_else(|| {
            let section = format!("[keymap.{}]", kind.name);
            problems.extend(prefixed(&section, (kind.check)(&Table::new(), commands)));
            Table::new()
        });
        (kind, selected)
    }
}

/// `[chrome.<name>]` of each chrome of the build, checked by that chrome. A table for a chrome
/// the build doesn't have gets a notice.
fn check_chromes(
    tables: BTreeMap<String, toml::Table>,
    env: &Environment,
    problems: &mut Vec<String>,
    notices: &mut Vec<String>,
) -> BTreeMap<String, Table> {
    let has = list(env.chromes.iter().map(|chrome| chrome.name));
    let mut checked = BTreeMap::new();
    for (name, table) in tables {
        let section = format!("[chrome.{name}]");
        let Some(chrome) = env.chromes.iter().find(|chrome| chrome.name == name) else {
            notices.push(format!(
                "{section} is for a chrome this build doesn't have (it has: {has}); it was not \
                 checked"
            ));
            continue;
        };
        let table = settings(table, &section, problems);
        problems.extend(prefixed(&section, (chrome.check)(&table)));
        checked.insert(name, table);
    }
    checked
}

/// Whether `name` is an engine's name: lower-case letters, digits and dashes, starting with a
/// letter.
fn is_engine_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn check_name(section: &str, name: &str, problems: &mut Vec<String>) {
    if !is_engine_name(name) {
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

/// `text` as a host pattern: a host name or IP address, or `*.` and a host name, normalized as
/// the `url` crate normalizes a URL's host (lower case, IDNA); or why it isn't one.
fn host_pattern(text: &str) -> Result<String, String> {
    let (wildcard, host) = match text.strip_prefix("*.") {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let parsed = match host.contains('*') {
        true => None,
        false => url::Host::parse(host).ok(),
    };
    match (wildcard, parsed) {
        (true, Some(url::Host::Domain(domain))) => Ok(format!("*.{domain}")),
        (false, Some(host)) => Ok(host.to_string()),
        _ => Err(format!(
            "{text:?} is not a host pattern such as \"docs.example.com\" or \"*.example.com\": \
             a host name or IP address with no scheme, port, path or user, and * only as the \
             whole first label of a host name"
        )),
    }
}

/// What `[ui]` sets.
struct Ui {
    chrome: String,
    font: Option<String>,
    font_size: u16,
    colors: BTreeMap<String, String>,
}

/// `[ui]`, which every chrome shares: the chrome (one of the build's), its font and size, and
/// its colors (names the build's chrome has, values `#rrggbb` or `#rrggbbaa`). What an older
/// sig kept in `[ui]` is reported with where it went.
fn check_ui(ui: toml::Table, env: &Environment, problems: &mut Vec<String>) -> Ui {
    let mut checked = Ui {
        chrome: DEFAULT_CHROME.into(),
        font: None,
        font_size: DEFAULT_FONT_SIZE,
        colors: BTreeMap::new(),
    };
    for (key, value) in ui {
        match key.as_str() {
            "chrome" => match value.as_str() {
                Some(name) if env.chromes.iter().any(|chrome| chrome.name == name) => {
                    checked.chrome = name.into()
                }
                Some(name) => problems.push(format!(
                    "[ui] chrome: this build has no chrome {name:?} (it has: {})",
                    list(env.chromes.iter().map(|chrome| chrome.name))
                )),
                None => problems.push(format!(
                    "[ui] chrome: expected a chrome's name, not {value}"
                )),
            },
            "font" => match value.as_str() {
                Some(family) if (1..=128).contains(&family.len()) => {
                    checked.font = Some(family.into())
                }
                _ => problems.push(format!(
                    "[ui] font: expected a font family's name of 1 to 128 bytes, not {value}"
                )),
            },
            "font-size" => match value.as_integer() {
                Some(size @ 9..=32) => checked.font_size = size as u16,
                _ => problems.push(format!(
                    "[ui] font-size: expected a whole number from 9 to 32, not {value}"
                )),
            },
            "colors" => match value {
                toml::Value::Table(colors) => checked.colors = check_colors(colors, env, problems),
                _ => problems.push("[ui] colors: expected a [ui.colors] table".into()),
            },
            "home" | "search" => problems.push(format!(
                "[ui] {key}: it is a top-level setting now, `{key} = ...` before the first table"
            )),
            "keys" => problems.push(
                "[ui.keys]: key bindings are in [keys], [keys.page] and [keys.location] now".into(),
            ),
            other => problems.push(format!(
                "[ui] {other}: unknown key ([ui] has chrome, font, font-size and [ui.colors])"
            )),
        }
    }
    checked
}

/// `[ui.colors]`: each a color the build's chrome has, `#rrggbb` or `#rrggbbaa`.
fn check_colors(
    colors: toml::Table,
    env: &Environment,
    problems: &mut Vec<String>,
) -> BTreeMap<String, String> {
    let mut checked = BTreeMap::new();
    for (name, value) in colors {
        let hex = value.as_str().and_then(|v| v.strip_prefix('#'));
        let valid = hex.is_some_and(|hex| {
            matches!(hex.len(), 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit())
        });
        if !env.color_names.contains(&name.as_str()) {
            problems.push(format!(
                "[ui.colors] {name}: unknown color (the colors are {})",
                list(env.color_names.iter().copied())
            ));
        } else if let (true, Some(text)) = (valid, value.as_str()) {
            checked.insert(name, text.to_owned());
        } else {
            problems.push(format!(
                "[ui.colors] {name}: expected \"#rrggbb\" or \"#rrggbbaa\", not {value}"
            ));
        }
    }
    checked
}

#[cfg(test)]
mod tests {
    use sighurt_api::engine::EngineCapabilities;

    use super::*;
    use crate::state::{ChromeChecks, Frontend};

    /// A keymap of the build besides `bindings`: its table maps keys to commands.
    const FAKE: KeymapKind = KeymapKind {
        name: "fake",
        check: |table, commands| {
            let problem = |(key, value): (&String, &Value)| match value.as_str() {
                Some(command) => commands
                    .parse(command)
                    .err()
                    .map(|e| format!("{key:?}: {e}")),
                None => Some(format!("{key:?}: expected a command, not {}", value.kind())),
            };
            table.iter().filter_map(problem).collect()
        },
        new: || Box::new(keys::Bindings::default()),
    };

    /// The standard chrome as the core sees it, with no options yet.
    const STANDARD: ChromeChecks = ChromeChecks {
        name: "standard",
        check: |table| {
            table
                .keys()
                .map(|key| format!("{key}: unknown key"))
                .collect()
        },
        commands: &[],
    };

    /// sighurt-ui's color names, which the default file documents.
    const COLORS: [&str; 11] = [
        "bg",
        "surface",
        "hover",
        "tab",
        "border",
        "focus",
        "text",
        "text_muted",
        "text_dim",
        "selection",
        "error",
    ];

    /// A build with three engines (Servo honoring both engine settings, Wasm only `storage`,
    /// the demo engine none), the fake keymap, the standard chrome and its colors, for a user
    /// whose home is /home/me.
    fn env() -> Environment {
        let engines =
            ["servo", "wasm", "demo"].map(|e| (e.to_owned(), EngineCapabilities::default()));
        let settings: [(_, &'static [&'static str]); 2] = [
            ("servo", &["user-agent", "storage"]),
            ("wasm", &["storage"]),
        ];
        Environment {
            engines: engines.into(),
            engine_settings: settings.map(|(e, s)| (e.to_owned(), s)).into(),
            home_dir: Some("/home/me".into()),
            keymaps: vec![FAKE],
            chromes: vec![STANDARD],
            color_names: &COLORS,
            frontend: Frontend::Gui,
        }
    }

    fn parse(user: &str) -> Result<Config, ConfigError> {
        Config::parse(Some(user), &env())
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

    fn notices(user: &str) -> Vec<String> {
        parse(user).unwrap().notices
    }

    #[test]
    fn defaults_parse() {
        let config = Config::parse(None, &env()).unwrap();
        assert_eq!(config.home(), "sig://home");
        assert!(config.search().contains("{}"));
        assert_eq!(config.default_engine(), "servo");
        assert!(config.is_enabled("servo") && config.is_enabled("demo"));
        let wasm = config.routes().iter().find(|r| r.engine == "wasm").unwrap();
        assert_eq!(wasm.extensions, ["wasm"]);
        assert!(wasm.origins.is_empty());
        assert_eq!(config.keymap, keys::KIND);
        assert!(config.keymap_settings.is_empty(), "no default bindings");
        assert_eq!(config.tui(), &TuiConfig::default());
        assert_eq!(config.chrome(), DEFAULT_CHROME);
        assert_eq!(
            (config.font(), config.font_size()),
            (None, DEFAULT_FONT_SIZE)
        );
        assert!(config.colors().is_empty() && config.notices().is_empty());
        assert_eq!(config.zoom(), 1.0);
        assert!(config.keywords().is_empty());
        assert_eq!(config.engine_settings("servo"), EngineSettings::default());
        assert_eq!(config.downloads(), None);
        assert!(config.restore_session());
        assert_eq!(parse("version = 1"), Ok(config));
    }

    fn terminal_env() -> Environment {
        Environment {
            frontend: Frontend::Terminal,
            ..env()
        }
    }

    #[test]
    fn the_terminal_installs_its_default_keys_and_the_window_does_not() {
        let config = Config::parse(None, &terminal_env()).unwrap();
        assert_eq!(config.tui(), &TuiConfig::default());
        assert_eq!(config.keymap_settings.len(), 31);
        let bound = |key: &str| config.keymap_settings[key].as_str();
        assert_eq!(bound("q"), Some("browser.quit"));
        assert_eq!(bound("j"), Some("page.send-key ArrowDown"));
        assert_eq!(bound("ctrl-r"), Some("page.reload"));
        assert_eq!(bound("ctrl-l"), Some("chrome.refresh"));
        assert_eq!(bound("m"), Some("chrome.toggle-mode"));
        assert_eq!(bound("alt-1"), Some("tab.select 1"));
        assert_eq!(bound("alt-9"), Some("tab.select 9"));
        assert_eq!(bound("+"), Some("page.zoom-in"));
        assert_eq!(bound(":"), Some("location.focus"));
        assert_eq!(bound("-"), Some("page.zoom-out"));
        // The window keeps an empty [keys] table, and still accepts the terminal's commands
        // where only the terminal reads them.
        let window = parse(
            "version = 1\n[tui.keys]\n\"m\" = \"chrome.toggle-mode\"\n[keys]\n\"j\" = \"tab.next\"",
        )
        .unwrap();
        assert_eq!(window.keymap_settings.len(), 1);
        assert_eq!(window.keymap_settings["j"].as_str(), Some("tab.next"));
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
        assert_eq!(config.colors()["selection"], "#60a5fa55");
        assert_eq!(config.chrome(), "standard");
        assert_eq!(config.keymap, keys::KIND);
        assert!(!config.keymap_settings.is_empty());
        let enabled = "version = 1\n[engines]\nenabled = [\"servo\", \"wasm\"]\n";
        assert!(!parse(enabled).unwrap().is_enabled("demo"));
        let origins = "version = 1\n[routing.wasm]\norigins = [\"http://localhost:8080\"]\n";
        let config = parse(origins).unwrap();
        let wasm = config.routes().iter().find(|r| r.engine == "wasm").unwrap();
        assert_eq!(wasm.origins, ["http://localhost:8080"]);
        let mut wpe = env();
        wpe.engines
            .insert("wpe".into(), EngineCapabilities::default());
        let config = Config::parse(Some(&format!("version = 1\n{examples}")), &wpe).unwrap();
        let wpe = config.routes().iter().find(|r| r.engine == "wpe").unwrap();
        assert_eq!(wpe.hosts, ["*.google.com", "google.com"]);
        assert_eq!(config.keywords().len(), 2);
        assert_eq!(config.engine_settings("servo").storage, Storage::Ephemeral);
        assert_eq!(config.downloads(), Some(Path::new("/home/me/Downloads")));
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
            "[engines.wasm] command: engines are compiled into sig now, so there is no command \
             to run",
            "[engines.wasm] extensions: they are in [routing.wasm] now",
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
        assert_problem(
            "version = 1\n[input]\nkeymaps = \"fake\"",
            "[input]: unknown field",
        );
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
    fn host_rules_are_host_names_or_wildcards_of_one_engine() {
        let config = parse(
            r#"
            version = 1
            [routing.wasm]
            hosts = ["Docs.Example.COM", "*.EXAMPLE.org", "Bücher.de", "*.bücher.de", "127.0.0.1",
                     "[0:0::1]", "docs.example.com"]
            [routing.servo]
            hosts = ["maps.example.org"]
            "#,
        )
        .unwrap();
        let route = |engine| config.routes().iter().find(|r| r.engine == engine).unwrap();
        // Lower-cased and IDNA-normalized like a URL's host; listed twice by one engine is fine.
        let wasm = &route("wasm").hosts;
        assert_eq!(
            wasm,
            &[
                "docs.example.com",
                "*.example.org",
                "xn--bcher-kva.de",
                "*.xn--bcher-kva.de",
                "127.0.0.1",
                "[::1]",
                "docs.example.com"
            ]
        );
        // Merged field by field over the built-in rules, as the others are.
        assert_eq!(route("servo").hosts, ["maps.example.org"]);
        assert_eq!(route("servo").schemes, ["data", "about"]);

        let refused = [
            "example.com:8080",
            "example.com/path",
            "https://example.com",
            "user@example.com",
            "exa mple.com",
            "",
            "*",
            "*.",
            "*.*.example.com",
            "a.*.example.com",
            "w*.example.com",
            "*.127.0.0.1",
            "*.[::1]",
        ];
        for pattern in refused {
            let user = format!("version = 1\n[routing.wasm]\nhosts = [{pattern:?}]");
            let problems = problems(&user);
            assert_eq!(problems.len(), 1, "{pattern:?}: {problems:#?}");
            assert!(
                problems[0].starts_with("[routing.wasm] hosts: ")
                    && problems[0].contains("is not a host pattern"),
                "{pattern:?}: {problems:#?}"
            );
        }
        // A pattern belongs to one engine, however it is written.
        assert_problem(
            "version = 1\n[routing.wasm]\nhosts = [\"*.Example.com\"]\n\
             [routing.demo]\nhosts = [\"*.example.COM\"]",
            "[routing] the host \"*.example.com\" belongs to both demo and wasm",
        );
    }

    #[test]
    fn keys_and_ui_are_the_users_alone() {
        let config = parse(
            r##"
            version = 1
            [keys]
            "ctrl-t" = "tab.new"
            [ui]
            font = "Inter"
            [ui.colors]
            bg = "#000000"
            "##,
        )
        .unwrap();
        assert!(!config.keymap_settings.is_empty());
        assert_eq!(config.colors()["bg"], "#000000");
        assert_eq!(config.font(), Some("Inter"));
        assert!(parse("version = 1\n[ui]\ncolors = 3").is_err());
        assert_problem(
            "version = 1\n[ui]\nfnt = \"Inter\"",
            "[ui] fnt: unknown key",
        );
    }

    // ── Keymaps, chromes and notices ──────────────────────────────────────────────────────

    #[test]
    fn the_selected_keymap_reads_its_own_table() {
        let config =
            parse("version = 1\n[input]\nkeymap = \"fake\"\n[keymap.fake]\n\"x\" = \"tab.new\"")
                .unwrap();
        assert_eq!(config.keymap, FAKE);
        assert_eq!(
            config.keymap_settings,
            Table::from([("x".into(), Value::String("tab.new".into()))])
        );
        assert_eq!(config.notices(), [] as [String; 0]);
        // Saying so is the same as saying nothing.
        assert_eq!(
            parse("version = 1\n[input]\nkeymap = \"bindings\""),
            parse("version = 1")
        );
    }

    #[test]
    fn an_unknown_keymap_is_an_error_that_lists_the_builds() {
        assert_problem(
            "version = 1\n[input]\nkeymap = \"modl\"",
            "[input] keymap: this build has no keymap \"modl\" (it has: bindings, fake)",
        );
    }

    #[test]
    fn keymap_bindings_points_to_keys() {
        assert_problem(
            "version = 1\n[keymap.bindings]\n\"ctrl-t\" = \"tab.new\"",
            "[keymap.bindings]: the bindings keymap reads [keys], [keys.page] and [keys.location]",
        );
    }

    #[test]
    fn a_keymaps_problems_are_prefixed_and_in_the_one_list() {
        let problems = problems(
            "version = 1\nhome = \"nope\"\n[input]\nkeymap = \"fake\"\n[keymap.fake]\n\
             \"x\" = \"tab.nope\"\n\"y\" = 3\n[keys]\n\"ctrl-t\" = \"tab.nwe\"",
        );
        let expected = [
            "home:",
            "[keymap.fake] \"x\": unknown command \"tab.nope\"",
            "[keymap.fake] \"y\": expected a command, not a whole number",
            // An unused table is still checked.
            "[keys] \"ctrl-t\" = \"tab.nwe\": unknown command",
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
    fn a_key_table_nobody_reads_gets_a_notice() {
        let keys = "version = 1\n[input]\nkeymap = \"fake\"\n[keys]\n\"ctrl-t\" = \"tab.new\"";
        assert_eq!(
            notices(keys),
            ["[keys] is not used: [input] keymap is \"fake\", which reads [keymap.fake]"]
        );
        let fake = "version = 1\n[keymap.fake]\n\"x\" = \"tab.new\"";
        assert_eq!(
            notices(fake),
            ["[keymap.fake] is not used: [input] keymap is \"bindings\", which reads [keys]"]
        );
    }

    #[test]
    fn tables_for_what_the_build_lacks_get_a_notice() {
        let notices = notices(
            r#"
            version = 1
            [engines]
            enabled = ["servo", "wpe"]
            [routing.wpe]
            schemes = ["wpe-test"]
            [keymap.modl]
            x = 1
            [chrome.line]
            position = 3
            "#,
        );
        assert_eq!(
            notices,
            [
                "[routing.wpe] is for an engine this build doesn't have (it has: demo, servo, \
                 wasm); its rules are not used",
                "[engines] enabled: this build has no engine \"wpe\" (it has: demo, servo, wasm)",
                "[chrome.line] is for a chrome this build doesn't have (it has: standard); it \
                 was not checked",
                "[keymap.modl] is for a keymap this build doesn't have (it has: bindings, fake); \
                 it was not checked",
            ]
        );
        // The built-in file's tables stay silent: it routes wasm in every build.
        let demo = Environment {
            engines: [("demo".to_owned(), EngineCapabilities::default())].into(),
            ..env()
        };
        let config = Config::parse(Some("version = 1\n[engines]\ndefault = \"demo\""), &demo);
        assert_eq!(config.unwrap().notices(), [] as [String; 0]);
    }

    #[test]
    fn an_engine_the_build_lacks_opens_nothing() {
        // Its rules are checked, and left out: what they claim goes to the default engine.
        let config =
            parse("version = 1\n[routing.wpe]\nhosts = [\"wpe.test\"]\nschemes = [\"wpe-test\"]")
                .unwrap();
        let engines: Vec<_> = config.routes().iter().map(|r| r.engine.as_str()).collect();
        assert_eq!(engines, ["servo", "wasm"]);
        assert_problem(
            "version = 1\n[routing.wpe]\nhosts = [\"https://wpe.test/\"]",
            "[routing.wpe] hosts: ",
        );
        assert_problem(
            "version = 1\n[routing.wpe]\nhosts = [\"a.test\"]\n[routing.wasm]\nhosts = [\"a.test\"]",
            "[routing] the host \"a.test\" belongs to both wasm and wpe",
        );
        // The default engine must be one of the build's: otherwise every web page would fail.
        assert_eq!(
            problems("version = 1\n[engines]\ndefault = \"wpe\""),
            ["[engines] default: this build has no engine \"wpe\" (it has: demo, servo, wasm)"]
        );
    }

    #[test]
    fn ui_is_checked_against_the_build() {
        let config = parse(
            "version = 1\n[ui]\nchrome = \"standard\"\nfont = \"Inter\"\nfont-size = 18\n\
             [chrome.standard]",
        )
        .unwrap();
        assert_eq!(config.chrome(), "standard");
        assert_eq!((config.font(), config.font_size()), (Some("Inter"), 18));
        assert_eq!(config.chrome_settings("standard"), Some(&Table::new()));
        let problems = problems(
            "version = 1\n[ui]\nchrome = \"nope\"\nfont = \"\"\nfont-size = 40\n\
             [ui.colors]\ntext = \"#ff0000\"\nbackground = \"#000000\"\n\
             [chrome.standard]\ntabs = \"left\"",
        );
        let expected = [
            "[ui] chrome: this build has no chrome \"nope\" (it has: standard)",
            "[ui] font: expected a font family's name of 1 to 128 bytes",
            "[ui] font-size: expected a whole number from 9 to 32, not 40",
            "[ui.colors] background: unknown color (the colors are bg, surface,",
            "[chrome.standard] tabs: unknown key",
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
    fn unknown_color_names_are_errors() {
        let problems = problems(
            "version = 1\nhome = \"nope\"\n[ui.colors]\ntext = \"#ff0000\"\nbackground = \"#000000\"",
        );
        assert_eq!(problems.len(), 2, "{problems:#?}");
        assert!(problems[1].contains("[ui.colors] background: unknown color"));
        let config = parse("version = 1\n[ui.colors]\ntext = \"#ff0000\"").unwrap();
        assert_eq!(config.colors()["text"], "#ff0000");
    }

    #[test]
    fn chrome_commands_are_checked_against_the_selected_chrome() {
        let env = Environment {
            chromes: vec![
                ChromeChecks {
                    commands: &["toggle-tabs"],
                    ..STANDARD
                },
                ChromeChecks {
                    name: "line",
                    check: |_| Vec::new(),
                    commands: &[],
                },
            ],
            ..env()
        };
        let parse = |user: &str| Config::parse(Some(&format!("version = 1\n{user}")), &env);
        let tables = "[keys]\n\"ctrl-b\" = \"chrome.toggle-tab\"\n\
                      [input.buttons]\nforward = \"chrome.toggle-tab\"\n\
                      [keymap.fake]\nx = \"chrome.toggle-tab\"\n";
        let lacks = "the standard chrome has no command chrome.toggle-tab; it has \
                     chrome.toggle-tabs";
        let problems = parse(tables).unwrap_err().problems;
        let expected = [
            format!("[keys] \"ctrl-b\" = \"chrome.toggle-tab\": {lacks}"),
            format!("[input.buttons] forward = \"chrome.toggle-tab\": {lacks}"),
            format!("[keymap.fake] \"x\": {lacks}"),
        ];
        assert_eq!(problems.len(), 3, "{problems:#?}");
        for problem in expected {
            assert!(
                problems.contains(&problem),
                "{problem:?} not in {problems:#?}"
            );
        }
        let good = tables.replace("toggle-tab\"", "toggle-tabs\"");
        let config = parse(&good).unwrap();
        let toggle = Command::Chrome("toggle-tabs".into());
        assert_eq!(config.button(PointerButton::Forward), Some(&toggle));
        // The same commands, for a chrome that has none.
        let line = format!("{good}[ui]\nchrome = \"line\"\n");
        let problems = parse(&line).unwrap_err().problems;
        assert_eq!(problems.len(), 3, "{problems:#?}");
        assert!(
            problems
                .iter()
                .all(|p| p
                    .ends_with("the line chrome has no command chrome.toggle-tabs; it has none"))
        );
    }

    #[test]
    fn input_settings_merge_key_by_key_and_are_checked() {
        let config = Config::default();
        assert_eq!(config.scroll_speed(), 1.0);
        assert_eq!(config.button(PointerButton::Back), Some(&Command::PageBack));
        assert_eq!(
            config.button(PointerButton::Forward),
            Some(&Command::PageForward)
        );
        assert_eq!(config.button(PointerButton::Middle), None);
        // "" unbinds a button; one left out keeps its default.
        let config =
            parse("version = 1\n[input]\nscroll-speed = 2\n[input.buttons]\nback = \"\"").unwrap();
        assert_eq!(config.scroll_speed(), 2.0);
        assert_eq!(config.button(PointerButton::Back), None);
        assert_eq!(
            config.button(PointerButton::Forward),
            Some(&Command::PageForward)
        );
        let config = parse("version = 1\n[input.buttons]\nback = \"tab.previous\"").unwrap();
        assert_eq!(
            config.button(PointerButton::Back),
            Some(&Command::TabPrevious)
        );
        assert_eq!(config.scroll_speed(), 1.0);

        let problems = problems(
            "version = 1\n[input]\nscroll-speed = 0.05\n[input.buttons]\nback = \"page.bak\"",
        );
        assert_eq!(
            problems,
            [
                "[input] scroll-speed: expected a number from 0.1 to 10, not 0.05",
                "[input.buttons] back = \"page.bak\": unknown command \"page.bak\"",
            ]
        );
        for speed in ["10.5", "nan", "inf", "-1", "\"fast\""] {
            assert_problem(
                &format!("version = 1\n[input]\nscroll-speed = {speed}"),
                "scroll-speed",
            );
        }
        assert!(parse("version = 1\n[input]\nscroll-speed = 10").is_ok());
        assert_problem(
            "version = 1\n[input.buttons]\nmiddle = \"tab.new\"",
            "unknown field `middle`",
        );
    }

    #[test]
    fn dates_are_not_settings() {
        assert_problem(
            "version = 1\n[keys]\n\"ctrl-t\" = 1979-05-27",
            "[keys] ctrl-t: dates are not settings",
        );
        assert_problem(
            "version = 1\n[keymap.fake]\nx = [{ at = 1979-05-27 }]",
            "[keymap.fake] x: dates are not settings",
        );
        assert_problem(
            "version = 1\n[ui]\nfont = 1979-05-27",
            "[ui] font: expected",
        );
        assert_problem(
            "version = 1\nkeymap = 3",
            "keymap: expected a [keymap] table, not 3",
        );
        assert_problem(
            "version = 1\n[chrome]\nstandard = true",
            "[chrome] standard: expected a [chrome.standard] table, not true",
        );
    }

    // ── Zoom, keywords, engines and files ─────────────────────────────────────────────────

    #[test]
    fn zoom_is_within_the_ladders_ends() {
        assert_eq!(parse("version = 1\nzoom = 1.25").unwrap().zoom(), 1.25);
        assert_eq!(parse("version = 1\nzoom = 3").unwrap().zoom(), 3.0);
        assert_eq!(parse("version = 1\nzoom = 0.5").unwrap().zoom(), 0.5);
        // Off the ladder is fine: zooming in and out takes the next step.
        assert_eq!(parse("version = 1\nzoom = 1.3").unwrap().zoom(), 1.3);
        for zoom in ["0.49", "3.01", "nan", "-inf", "0"] {
            assert_problem(
                &format!("version = 1\nzoom = {zoom}"),
                "zoom: expected a number from 0.5 to 3, not ",
            );
        }
        assert_problem("version = 1\nzoom = \"110%\"", "zoom: invalid type");
    }

    #[test]
    fn keywords_are_names_for_web_templates() {
        let config = parse(
            r#"
            version = 1
            [keywords]
            w = "https://en.wikipedia.org/w/index.php?search={}"
            gh = "https://github.com/"
            rfc-9110 = "http://localhost:8000/rfc/{}.html"
            "#,
        )
        .unwrap();
        assert_eq!(config.keywords().len(), 3);
        assert_eq!(config.keywords()["gh"], "https://github.com/");
        let problems = problems(
            r#"
            version = 1
            [keywords]
            W = "https://w.test/{}"
            "a.b" = "https://w.test/{}"
            "a b" = "https://w.test/{}"
            "" = "https://w.test/{}"
            localhost = "https://w.test/{}"
            this-keyword-is-longer-than-32-ch = "https://w.test/{}"
            f = "file:///tmp/{}"
            g = "sig://home"
            h = "{}"
            i = 3
            "#,
        );
        let expected = [
            "[keywords] \"W\": a keyword is 1 to 32 lower-case letters",
            "[keywords] \"a.b\": a keyword",
            "[keywords] \"a b\": a keyword",
            "[keywords] \"\": a keyword",
            "[keywords] \"localhost\": a keyword",
            "[keywords] \"this-keyword-is-longer-than-32-ch\": a keyword",
            "[keywords] f: expected an http(s) URL, with {} where the query goes, not",
            "[keywords] g: expected an http(s) URL",
            "[keywords] h: expected an http(s) URL",
            "[keywords] i: expected an http(s) URL",
        ];
        for part in expected {
            assert!(
                problems.iter().any(|p| p.contains(part)),
                "{part:?} not in {problems:#?}"
            );
        }
        assert_eq!(problems.len(), expected.len(), "{problems:#?}");
        assert_problem(
            "version = 1\nkeywords = 3",
            "keywords: expected a [keywords] table",
        );
    }

    #[test]
    fn engine_settings_are_checked_against_what_each_engine_honors() {
        let config = parse(
            r#"
            version = 1
            [engines]
            default = "servo"
            [engines.servo]
            user-agent = "Mozilla/5.0 (X11; Linux x86_64) Sighurt/0.1"
            storage = "ephemeral"
            [engines.wasm]
            storage = "profile"
            "#,
        )
        .unwrap();
        let servo = config.engine_settings("servo");
        assert_eq!(
            servo.user_agent.as_deref(),
            Some("Mozilla/5.0 (X11; Linux x86_64) Sighurt/0.1")
        );
        assert_eq!(servo.storage, Storage::Ephemeral);
        assert_eq!(config.engine_settings("wasm"), EngineSettings::default());
        assert_eq!(config.engine_settings("demo"), EngineSettings::default());
        assert_eq!(config.notices(), [] as [String; 0]);

        let problems = problems(&format!(
            r#"
            version = 1
            [engines.servo]
            user-agent = "{}"
            storage = "kept"
            javascript = false
            [engines.wasm]
            user-agent = "Sighurt"
            [engines.demo]
            storage = "ephemeral"
            [engines.Servo]
            "#,
            "x".repeat(513)
        ));
        let expected = [
            "[engines.servo] user-agent: expected 1 to 512 printable ASCII characters, not",
            "[engines.servo] storage: expected \"profile\" or \"ephemeral\", not \"kept\"",
            "[engines.servo] javascript: unknown key (an engine's settings are user-agent, \
             storage)",
            "[engines.wasm] user-agent: the wasm engine can't honor it (it honors: storage)",
            "[engines.demo] storage: the demo engine can't honor it (it honors: none)",
            "[engines.Servo]: \"Servo\" is not an engine name",
        ];
        for part in expected {
            assert!(
                problems.iter().any(|p| p.contains(part)),
                "{part:?} not in {problems:#?}"
            );
        }
        assert_eq!(problems.len(), expected.len(), "{problems:#?}");
        // As TOML writes them: empty, a line break, a tab, a letter beyond ASCII, DEL.
        for agent in [r#""""#, r#""S\n""#, r#""S\t1""#, r#""S ä""#, r#""S\u007F""#] {
            assert_problem(
                &format!("version = 1\n[engines.servo]\nuser-agent = {agent}"),
                "[engines.servo] user-agent: expected 1 to 512 printable ASCII characters",
            );
        }
        // An engine the build doesn't have: the form is checked, and a notice says so.
        let wpe = "version = 1\n[engines.wpe]\nuser-agent = \"Sighurt\"\nstorage = \"ephemeral\"";
        assert_eq!(
            notices(wpe),
            ["[engines.wpe] is for an engine this build doesn't have (it has: demo, servo, wasm)"]
        );
        assert_problem(
            "version = 1\n[engines.wpe]\nstorage = 3",
            "[engines.wpe] storage: expected \"profile\" or \"ephemeral\", not 3",
        );
        // The rest of [engines] is checked as before.
        assert_problem(
            "version = 1\n[engines]\ndefualt = \"servo\"\n[engines.servo]\nstorage = \"profile\"",
            "[engines]: unknown field `defualt`",
        );
    }

    #[test]
    fn the_download_folder_is_absolute_or_in_the_home_directory() {
        let directory = |text: &str| {
            parse(&format!("version = 1\n[downloads]\ndirectory = {text:?}"))
                .map(|config| config.downloads().map(Path::to_owned))
        };
        assert_eq!(directory("~/Hämtade"), Ok(Some("/home/me/Hämtade".into())));
        assert_eq!(directory("/srv/in"), Ok(Some("/srv/in".into())));
        for bad in ["Downloads", "./x", "~", "~other/x", ""] {
            let problems = directory(bad).unwrap_err().problems;
            assert_eq!(
                problems,
                [format!(
                    "[downloads] directory: {bad:?} is not an absolute path or one starting \
                     with ~/"
                )]
            );
        }
        // Without a home directory, ~/ means nothing.
        let homeless = Environment {
            home_dir: None,
            ..env()
        };
        let config = "version = 1\n[downloads]\ndirectory = \"~/x\"";
        let problems = Config::parse(Some(config), &homeless).unwrap_err().problems;
        assert_eq!(
            problems,
            ["[downloads] directory: \"~/x\" is in the home directory, which sig doesn't know"]
        );
        assert_problem(
            "version = 1\n[downloads]\nfolder = \"/x\"",
            "[downloads]: unknown field `folder`",
        );
    }

    #[test]
    fn the_session_is_restored_unless_the_file_says_not() {
        assert!(parse("version = 1\n[session]").unwrap().restore_session());
        let config = parse("version = 1\n[session]\nrestore = false").unwrap();
        assert!(!config.restore_session());
        assert_problem(
            "version = 1\n[session]\nrestore = \"no\"",
            "[session]: invalid type",
        );
        assert_problem(
            "version = 1\n[session]\nsave = false",
            "[session]: unknown field `save`",
        );
    }
}
