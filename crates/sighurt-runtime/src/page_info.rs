//! What `sig://help` is told about the browser ([`PageRequest::Info`]): the facts of this build,
//! this configuration and this run, as the text records `sighurt_api::services::PageInfo`
//! documents (one per line, fields separated by tabs). And what `sig://config` is told before
//! it starts the editor ([`PageInfo::Editor`]): which editor, which file, and the file's
//! problems.
//!
//! Generated from the browser itself (the command table, the keymap's `describe`, the build's
//! engines, the configuration in use), so the help page can't go stale. Never the
//! configuration's text.
//!
//! Must not own: deciding who may ask (the core), or how the page shows it.
//!
//! [`PageRequest::Info`]: sighurt_api::services::PageRequest::Info

use std::path::Path;

use sighurt_api::commands::DOCS;
use sighurt_api::engine::EngineCapabilities;
use sighurt_api::keymap::Frontend;
use sighurt_api::services::PageInfo;
use sighurt_core::config::{GraphicsProtocol, TuiMode};
use sighurt_core::BrowserState;

use crate::persistence::{self, Paths};

/// The answer to `kind`, for the browser `core` with the files at `paths`.
pub fn info(kind: PageInfo, core: &BrowserState, paths: &Paths) -> String {
    let config = core.config();
    let env = core.environment();
    let records: Vec<Vec<String>> = match kind {
        PageInfo::About => vec![
            pair("version", env!("CARGO_PKG_VERSION")),
            pair("license", env!("CARGO_PKG_LICENSE")),
            pair("authors", env!("CARGO_PKG_AUTHORS")),
            pair(
                "frontend",
                match env.frontend {
                    Frontend::Gui => "window",
                    Frontend::Terminal => "terminal",
                },
            ),
            pair("keymap", config.keymap_name()),
            pair("chrome", config.chrome()),
            pair("renderer", config.renderer()),
        ],
        PageInfo::Engines => env
            .engines
            .iter()
            .map(|(name, capabilities)| {
                vec![
                    name.clone(),
                    yes(config.is_enabled(name)).into(),
                    yes(config.default_engine() == name).into(),
                    yes(env.web_engines.contains(name)).into(),
                    capability_names(capabilities),
                ]
            })
            .collect(),
        PageInfo::Commands => DOCS
            .iter()
            .map(|doc| vec![doc.name.into(), doc.argument.into(), doc.description.into()])
            .collect(),
        PageInfo::UserCommands => config
            .commands()
            .iter()
            .map(|(name, commands)| {
                let commands: Vec<String> = commands.iter().map(ToString::to_string).collect();
                vec![name.clone(), commands.join(" ; ")]
            })
            .collect(),
        PageInfo::Keys => core
            .describe_keys()
            .into_iter()
            .map(|binding| vec![binding.mode, binding.keys, binding.action, binding.applies])
            .collect(),
        PageInfo::Paths => {
            let mut records = Vec::new();
            let mut path = |name: &str, path: Option<&Path>| {
                if let Some(path) = path {
                    records.push(vec![name.to_owned(), path.display().to_string()]);
                }
            };
            path("config", paths.config.as_deref());
            path("config-dir", paths.config.as_deref().and_then(Path::parent));
            path("state-dir", paths.session.as_deref().and_then(Path::parent));
            path("session", paths.session.as_deref());
            let history = paths.session.as_deref().map(persistence::history_path);
            path("history", history.as_deref());
            path("downloads", config.downloads());
            records
        }
        PageInfo::Settings => {
            let tui = config.tui();
            let enabled = env
                .engines
                .keys()
                .filter(|name| config.is_enabled(name))
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            vec![
                pair("home", config.home()),
                pair("search", config.search()),
                pair("zoom", &config.zoom().to_string()),
                pair("engines.default", config.default_engine()),
                pair("engines.enabled", &enabled),
                pair("input.keymap", config.keymap_name()),
                pair("ui.chrome", config.chrome()),
                pair("ui.renderer", config.renderer()),
                pair("session.restore", &config.restore_session().to_string()),
                pair("history.remember", &config.remember_history().to_string()),
                pair(
                    "pages.terminal.enabled",
                    &config.pages().terminal.to_string(),
                ),
                pair("pages.config.enabled", &config.pages().config.to_string()),
                pair(
                    "tui.mode",
                    match tui.mode {
                        TuiMode::Auto => "auto",
                        TuiMode::Text => "text",
                        TuiMode::Graphics => "graphics",
                    },
                ),
                pair(
                    "tui.graphics",
                    match tui.graphics {
                        GraphicsProtocol::Auto => "auto",
                        GraphicsProtocol::Kitty => "kitty",
                        GraphicsProtocol::Sixel => "sixel",
                        GraphicsProtocol::None => "none",
                    },
                ),
            ]
        }
        PageInfo::Editor => {
            let pages = config.pages();
            let vars: Vec<_> = std::env::vars_os().collect();
            let (line, from) = match crate::pty::editor(pages.editor.as_deref(), &vars) {
                Some((argv, from)) => {
                    let words: Vec<_> = argv.iter().map(|w| w.to_string_lossy()).collect();
                    (words.join(" "), from.name())
                }
                None => (String::new(), "none"),
            };
            let mut records = vec![
                pair("enabled", yes(pages.config)),
                pair("editor", &line),
                pair("from", from),
            ];
            if let Some(file) = &paths.config {
                let new = !file.exists();
                records.push(pair("file", &file.display().to_string()));
                records.push(pair("new", yes(new)));
                // A missing file is new, not a problem; one that can't be read is, as for the
                // reload that would refuse it.
                match persistence::load_config(Some(file.as_path()), env) {
                    Err(persistence::LoadError::Config(error)) => {
                        records.extend(error.problems().iter().map(|p| pair("problem", p)));
                    }
                    Err(persistence::LoadError::Other(message)) if !new => {
                        records.push(pair("problem", &message));
                    }
                    _ => {}
                }
            }
            records
        }
    };
    let mut text = String::new();
    for record in records {
        let fields: Vec<String> = record.iter().map(|field| field_of(field)).collect();
        text.push_str(&fields.join("\t"));
        text.push('\n');
    }
    text
}

fn pair(name: &str, value: &str) -> Vec<String> {
    vec![name.to_owned(), value.to_owned()]
}

fn yes(flag: bool) -> &'static str {
    match flag {
        true => "yes",
        false => "no",
    }
}

/// `field` with its tabs and line breaks made spaces, so a record stays one line.
fn field_of(field: &str) -> String {
    field
        .chars()
        .map(|c| match c {
            '\t' | '\n' | '\r' => ' ',
            c => c,
        })
        .collect()
}

/// What an engine supports, as words: `history page-zoom find semantics ...`.
fn capability_names(c: &EngineCapabilities) -> String {
    let names = [
        (c.history_traversal, "history"),
        (c.text_input, "text-input"),
        (c.page_zoom, "page-zoom"),
        (c.find_in_page, "find"),
        (c.semantics, "semantics"),
        (c.fullscreen, "fullscreen"),
        (c.site_data.any(), "site-data"),
        (c.process_per_view, "process-per-view"),
    ];
    let names: Vec<&str> = names
        .iter()
        .filter(|(has, _)| *has)
        .map(|(_, name)| *name)
        .collect();
    names.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sighurt_core::config::Config;
    use sighurt_core::Environment;

    fn core() -> BrowserState {
        let env = Environment {
            engines: ["servo", "wasm"]
                .map(|e| (e.to_owned(), EngineCapabilities::default()))
                .into(),
            ..Environment::default()
        };
        BrowserState::new(Config::parse(None, &env).unwrap(), env)
    }

    #[test]
    fn records_are_lines_of_tab_separated_fields() {
        let core = core();
        let paths = Paths {
            config: Some("/c/sighurt/config.toml".into()),
            session: Some("/s/sighurt/session.toml".into()),
        };
        let commands = info(PageInfo::Commands, &core, &paths);
        assert_eq!(commands.lines().count(), DOCS.len());
        assert!(commands.starts_with("tab.new\t[address]\t"));
        let about = info(PageInfo::About, &core, &paths);
        assert!(about.contains("version\t") && about.contains("license\tAGPL-3.0-or-later"));
        let user = info(PageInfo::UserCommands, &core, &paths);
        assert!(user.contains("help\ttab.new sig://help"), "{user}");
        let found = info(PageInfo::Paths, &core, &paths);
        assert!(
            found.contains("history\t/s/sighurt/history.toml"),
            "{found}"
        );
        assert!(info(PageInfo::Settings, &core, &paths).contains("pages.terminal.enabled\tfalse"));
        let engines = info(PageInfo::Engines, &core, &paths);
        assert!(engines.contains("servo\tyes\tyes\t"), "{engines}");
        assert!(engines.contains("wasm\tyes\tno\t"), "{engines}");
        assert_eq!(field_of("a\tb\nc"), "a b c");
    }

    #[test]
    fn the_editor_is_named_with_its_file_and_the_files_problems() {
        let env = core().environment().clone();
        let config = "version = 1\n[pages.config]\neditor = \"/usr/bin/hx --vsplit\"";
        let core = BrowserState::new(Config::parse(Some(config), &env).unwrap(), env);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        let paths = Paths {
            config: Some(file.clone()),
            session: None,
        };
        let answer = info(PageInfo::Editor, &core, &paths);
        let shown = file.display().to_string();
        assert_eq!(
            answer,
            format!(
                "enabled\tyes\neditor\t/usr/bin/hx --vsplit\nfrom\tsetting\nfile\t{shown}\nnew\tyes\n"
            )
        );
        std::fs::write(&file, "version = 1\nzoom = 99\n").unwrap();
        let answer = info(PageInfo::Editor, &core, &paths);
        assert!(answer.contains("new\tno\nproblem\t"), "{answer}");
        assert!(answer.contains("zoom"), "{answer}");
        assert!(info(PageInfo::Settings, &core, &paths).contains("pages.config.enabled\ttrue"));
    }

    #[test]
    fn a_file_that_cant_be_read_is_a_problem_and_a_missing_one_is_new() {
        use std::os::unix::fs::PermissionsExt;
        let core = core();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        let paths = Paths {
            config: Some(file.clone()),
            session: None,
        };
        let problems = |answer: &str| -> Vec<String> {
            answer
                .lines()
                .filter_map(|line| line.strip_prefix("problem\t"))
                .map(str::to_owned)
                .collect()
        };
        let answer = info(PageInfo::Editor, &core, &paths);
        assert!(answer.contains("new\tyes\n"), "{answer}");
        assert_eq!(problems(&answer), Vec::<String>::new(), "missing is new");

        std::fs::write(&file, b"version = 1\n# \xff\xfe\n").unwrap();
        let answer = info(PageInfo::Editor, &core, &paths);
        assert!(answer.contains("new\tno\n"), "{answer}");
        let found = problems(&answer);
        assert_eq!(found.len(), 1, "{answer}");
        assert!(found[0].contains("cannot read"), "{answer}");

        // Root reads it anyway.
        if rustix::process::geteuid().is_root() {
            return;
        }
        std::fs::write(&file, "version = 1\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        let answer = info(PageInfo::Editor, &core, &paths);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(answer.contains("new\tno\n"), "{answer}");
        let found = problems(&answer);
        assert_eq!(found.len(), 1, "{answer}");
        assert!(found[0].contains("cannot read"), "{answer}");
    }
}
