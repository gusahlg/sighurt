//! Seeded random `[keys]` tables read by the configuration parser. Valid tables (disjoint
//! keys, sequences, commands and `false`) must load, give `Keys` that pass `check`, and hold
//! each bound key where its table says and no binding for an unbound one; tables with one
//! injected defect (a key that starts another, five keystrokes, an unknown mode, a bad
//! command, a wrong value type, one key spelled twice) must be refused. Garbage never panics.
//! `SIG_FUZZ_ITERATIONS`, `SIG_FUZZ_SEED` and `SIG_FUZZ_CASE` are in `tools/fuzz/mod.rs`.

#[path = "../../../tools/fuzz/mod.rs"]
mod fuzz;

use std::time::Duration;

use fuzz::{iterations, run, Rng};
use sighurt_api::commands::CommandSet;
use sighurt_api::engine::EngineCapabilities;
use sighurt_api::keymap::{Action, Fallback, Frontend, KeySeq, KeyTable, Keys, Mode, UrlPattern};
use sighurt_core::config::{Config, RENDERERS};
use sighurt_core::Environment;

const LIMIT: Duration = Duration::from_secs(2);

fn env() -> Environment {
    let engines = ["servo", "demo"].map(|e| (e.to_owned(), EngineCapabilities::default()));
    Environment {
        engines: engines.into(),
        keys: Keys {
            modes: vec![
                Mode::new("normal")
                    .page_text(false)
                    .unmatched(Fallback::Take),
                Mode::new("insert"),
            ],
            initial: "normal".into(),
            line: Vec::new(),
        },
        renderers: RENDERERS,
        ..Environment::default()
    }
}

const COMMANDS: &[&str] = &[
    "page.reload",
    "page.back",
    "tab.new",
    "tab.new https://example.com/",
    "tab.select 3",
    "page.zoom-in",
    "input.mode insert",
    "input.mode normal",
    "page.find hello",
    "browser.quit",
];

const PATTERNS: &[&str] = &[
    "https://example.com",
    "https://*.example.org/docs",
    "http://localhost/app",
    "sig://help",
];

/// Where a table lives in the file, and where its bindings land.
#[derive(Clone, Debug)]
enum Scope {
    Global,
    Page,
    Location,
    Mode(&'static str),
    Url(&'static str),
    ModeUrl(&'static str, &'static str),
}

impl Scope {
    fn header(&self, tui: bool) -> String {
        let root = if tui { "tui.keys" } else { "keys" };
        match self {
            Scope::Global => format!("[{root}]"),
            Scope::Page => format!("[{root}.page]"),
            Scope::Location => format!("[{root}.location]"),
            Scope::Mode(m) => format!("[{root}.mode.{m}]"),
            Scope::Url(p) => format!("[{root}.url.\"{p}\"]"),
            Scope::ModeUrl(m, p) => format!("[{root}.mode.{m}.url.\"{p}\"]"),
        }
    }

    /// The tables that must hold the bindings (`None` is the line), and the address.
    fn lands(&self) -> (Vec<Option<&'static str>>, Option<&'static str>) {
        const MODES: [Option<&str>; 2] = [Some("normal"), Some("insert")];
        match self {
            Scope::Global => (vec![Some("normal"), Some("insert"), None], None),
            Scope::Page => (MODES.to_vec(), None),
            Scope::Location => (vec![None], None),
            Scope::Mode(m) => (vec![Some(m)], None),
            Scope::Url(p) => (MODES.to_vec(), Some(p)),
            Scope::ModeUrl(m, p) => (vec![Some(m)], Some(p)),
        }
    }
}

fn scope(rng: &mut Rng) -> Scope {
    let mode = *rng.pick(&["normal", "insert"]);
    let pattern = *rng.pick(PATTERNS);
    match rng.below(6) {
        0 => Scope::Global,
        1 => Scope::Page,
        2 => Scope::Location,
        3 => Scope::Mode(mode),
        4 => Scope::Url(pattern),
        _ => Scope::ModeUrl(mode, pattern),
    }
}

/// Keystrokes with pairwise different first keystrokes, so no two entries of any tables
/// overlap, shuffled.
fn first_keys() -> Vec<String> {
    let mut pool = Vec::new();
    for m in ["", "ctrl-", "alt-", "ctrl-alt-"] {
        for k in 'a'..='z' {
            pool.push(format!("{m}{k}"));
        }
    }
    pool
}

struct Entry {
    scope: Scope,
    tui: bool,
    keys: String,
    /// `None` is `false`.
    command: Option<&'static str>,
}

fn sequence(rng: &mut Rng, first: String, pool: &[String]) -> String {
    let mut text = first;
    for _ in 0..rng.below(4) {
        text.push(' ');
        text.push_str(rng.pick(pool));
    }
    text
}

fn entries(rng: &mut Rng) -> Vec<Entry> {
    let mut pool = first_keys();
    let all = pool.clone();
    let mut out = Vec::new();
    // Each scope (and frontend) is one TOML table, so each is written once.
    let mut seen: Vec<(String, bool)> = Vec::new();
    for _ in 0..rng.range(0, 12) {
        let scope = scope(rng);
        let tui = rng.one_in(3);
        let head = (scope.header(false), tui);
        if seen.contains(&head) {
            continue;
        }
        seen.push(head);
        for _ in 0..rng.range(1, 4) {
            if pool.is_empty() {
                break;
            }
            let first = pool.swap_remove(rng.below(pool.len()));
            out.push(Entry {
                scope: scope.clone(),
                tui,
                keys: sequence(rng, first, &all),
                command: if rng.one_in(4) {
                    None
                } else {
                    Some(*rng.pick(COMMANDS))
                },
            });
        }
    }
    out
}

fn render(entries: &[Entry]) -> String {
    let mut text = String::from("version = 1\n");
    for tui in [false, true] {
        let mut heads: Vec<String> = Vec::new();
        for e in entries.iter().filter(|e| e.tui == tui) {
            let head = e.scope.header(tui);
            if !heads.contains(&head) {
                heads.push(head);
            }
        }
        for head in heads {
            text.push_str(&format!("{head}\n"));
            for e in entries
                .iter()
                .filter(|e| e.tui == tui && e.scope.header(tui) == head)
            {
                match e.command {
                    Some(c) => text.push_str(&format!("{:?} = {:?}\n", e.keys, c)),
                    None => text.push_str(&format!("{:?} = false\n", e.keys)),
                }
            }
        }
    }
    text
}

fn tables(keys: &Keys) -> Vec<(Option<&str>, &[sighurt_api::keymap::Binding])> {
    let mut all = vec![(None, keys.table(KeyTable::Line).unwrap())];
    for mode in &keys.modes {
        all.push((Some(mode.name.as_str()), &mode.bindings[..]));
    }
    all
}

fn check_loaded(text: &str, entries: &[Entry], keys: &Keys) {
    for frontend in [Frontend::Gui, Frontend::Terminal] {
        let problems = keys.check(&CommandSet::ANY, frontend);
        assert!(problems.is_empty(), "{frontend:?}: {problems:?}\n{text}");
    }
    for e in entries {
        let seq: KeySeq = e.keys.parse().unwrap();
        let (lands, url) = e.scope.lands();
        let url: Option<UrlPattern> = url.map(|u| u.parse().unwrap());
        for (name, bindings) in tables(keys) {
            let here: Vec<_> = bindings
                .iter()
                .filter(|b| b.keys == seq && b.when.url == url)
                .collect();
            let belongs = lands.contains(&name);
            match (belongs, e.command) {
                (true, Some(c)) => {
                    let want = Action::Run(vec![CommandSet::ANY.parse(c).unwrap()]);
                    assert_eq!(here.len(), 1, "{:?} in {name:?}\n{text}", e.keys);
                    assert_eq!(here[0].action, want, "{:?} in {name:?}\n{text}", e.keys);
                    let frontend = e.tui.then_some(Frontend::Terminal);
                    assert_eq!(here[0].when.frontend, frontend, "{:?}\n{text}", e.keys);
                }
                (true, None) => assert!(
                    here.iter().all(|b| b.action == Action::Unbound),
                    "{:?} unbound but bound in {name:?}\n{text}",
                    e.keys
                ),
                (false, _) => assert!(here.is_empty(), "{:?} leaked into {name:?}\n{text}", e.keys),
            }
        }
    }
}

#[test]
fn fuzz_key_tables_valid_load_and_bind_where_written() {
    let env = env();
    run("fuzz_key_tables_valid", iterations(100), LIMIT, |case| {
        let list = entries(&mut case.rng);
        let text = render(&list);
        case.note(|| text.clone());
        let config = Config::parse(Some(&text), &env)
            .unwrap_or_else(|e| panic!("a valid table was refused: {e:?}\n{text}"));
        check_loaded(&text, &list, config.keys());
        // The same text gives the same keys.
        assert_eq!(
            config.keys(),
            Config::parse(Some(&text), &env).unwrap().keys()
        );
    });
}

/// Another spelling of `keys` that parses to the same keys, when there is one.
fn respelled(keys: &str) -> Option<String> {
    let alt = keys
        .split(' ')
        .map(|w| match w.strip_prefix("ctrl-alt-") {
            Some(rest) => format!("alt-ctrl-{rest}"),
            None => w
                .to_uppercase()
                .replace("CTRL-", "ctrl-")
                .replace("ALT-", "alt-"),
        })
        .collect::<Vec<_>>()
        .join("  ");
    let (a, b) = (keys.parse::<KeySeq>().ok()?, alt.parse::<KeySeq>().ok()?);
    (a == b && alt != keys).then_some(alt)
}

#[test]
fn fuzz_key_tables_with_a_defect_are_refused() {
    let env = env();
    let mut refused_kinds = [0usize; 7];
    run("fuzz_key_tables_defect", iterations(100), LIMIT, |case| {
        let list = entries(&mut case.rng);
        let tui = case.rng.one_in(3);
        let root = if tui { "tui.keys" } else { "keys" };
        let kind = case.rng.below(7);
        // The defect goes in a table of its own, last, so TOML stays well formed.
        let extra = match kind {
            // A key that starts another in one table (either order, either length).
            0 => {
                let long = format!("{} {}", "ctrl-alt-q", "ctrl-alt-w");
                match case.rng.below(2) {
                    0 => format!("[{root}.mode.insert]\n\"ctrl-alt-q\" = \"page.reload\"\n\"{long}\" = \"page.back\"\n"),
                    _ => format!("[{root}.page]\n\"{long}\" = \"page.back\"\n\"ctrl-alt-q\" = false\n"),
                }
            }
            // Five keystrokes (or six).
            1 => format!(
                "[{root}]\n\"ctrl-alt-q a b c d{}\" = \"page.reload\"\n",
                if case.rng.one_in(2) { " e" } else { "" }
            ),
            // Unknown or invalid mode.
            2 => format!(
                "[{root}.mode.{}]\n\"ctrl-alt-q\" = \"page.reload\"\n",
                case.rng.pick(&["nonexistent", "visual", "x-y-z"])
            ),
            // A bad command.
            3 => format!(
                "[{root}.location]\n\"ctrl-alt-q\" = {:?}\n",
                case.rng.pick(&[
                    "page.nope",
                    "",
                    "tab.new\u{0}",
                    "page.zoom-in 99999 x",
                    "bogus",
                    "page.reload; page.back"
                ])
            ),
            // A wrong value type (`true` is not `false`).
            4 => format!(
                "[{root}]\n\"ctrl-alt-q\" = {}\n",
                case.rng
                    .pick(&["true", "5", "[\"page.reload\"]", "1.5", "{ a = 1 }"])
            ),
            // One key twice, spelled differently.
            5 => {
                let first = "ctrl-alt-q";
                match respelled(first) {
                    Some(alt) => format!(
                        "[{root}.page]\n\"{first}\" = \"page.reload\"\n\"{alt}\" = \"page.back\"\n"
                    ),
                    None => return,
                }
            }
            // An empty key.
            _ => format!("[{root}]\n\"\" = \"page.reload\"\n"),
        };
        // The injected tables must not collide with the generated ones: the generated ones
        // never use `ctrl-alt-q`'s first keystroke... they may, so drop such entries.
        let filtered: Vec<Entry> = list
            .into_iter()
            .filter(|e| !e.keys.starts_with("ctrl-alt-q"))
            .collect();
        let mut text = render(&filtered);
        // A TOML table written twice is a different defect: skip a clash of headers.
        let head = extra.lines().next().unwrap().to_owned();
        if text.lines().any(|l| l == head) {
            return;
        }
        text.push_str(&extra);
        case.note(|| format!("kind {kind}\n{text}"));
        refused_kinds[kind] += 1;
        let result = Config::parse(Some(&text), &env);
        assert!(result.is_err(), "defect {kind} was accepted:\n{text}");
    });
    eprintln!("fuzz_key_tables_defect: kinds {refused_kinds:?}");
}

#[test]
fn fuzz_key_tables_garbage_never_panics() {
    let env = env();
    run("fuzz_key_tables_garbage", iterations(100), LIMIT, |case| {
        let mut text = String::from("version = 1\n");
        for _ in 0..case.rng.range(0, 6) {
            let head = match case.rng.below(8) {
                0 => "[keys]".to_owned(),
                1 => "[keys.page]".to_owned(),
                2 => "[keys.location]".to_owned(),
                3 => format!(
                    "[keys.mode.{}]",
                    case.rng
                        .string_from(&["normal", "insert", "x", "-", "."], 3)
                ),
                4 => format!("[keys.url.{:?}]", case.rng.string(12)),
                5 => "[tui.keys]".to_owned(),
                6 => format!(
                    "[tui.keys.mode.{}]",
                    case.rng.pick(&["normal", "insert", "nope"])
                ),
                _ => format!("[keys.mode.normal.url.{:?}]", case.rng.pick(PATTERNS)),
            };
            text.push_str(&head);
            text.push('\n');
            for _ in 0..case.rng.below(5) {
                let key: String = (0..case.rng.range(0, 6))
                    .map(|_| match case.rng.below(3) {
                        0 => case.rng.string(5),
                        _ => case
                            .rng
                            .pick(&["ctrl-a", "alt-b", "x", "Enter", "f5", "ctrl-shift-t", "a"])
                            .to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let value = match case.rng.below(5) {
                    0 => "false".to_owned(),
                    1 => "true".to_owned(),
                    2 => format!("{:?}", case.rng.string(10)),
                    _ => format!("{:?}", case.rng.pick(COMMANDS)),
                };
                text.push_str(&format!("{key:?} = {value}\n"));
            }
        }
        if case.rng.one_in(3) {
            text = case.rng.mutate(&text);
        }
        case.note(|| text.clone());
        if let Ok(config) = Config::parse(Some(&text), &env) {
            for frontend in [Frontend::Gui, Frontend::Terminal] {
                let problems = config.keys().check(&CommandSet::ANY, frontend);
                assert!(problems.is_empty(), "{problems:?}\n{text}");
            }
        }
    });
}
