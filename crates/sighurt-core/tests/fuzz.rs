//! Seeded random-input tests for what the core reads from the user and from files: the
//! configuration, typed addresses and keywords, the routing of a URL, saved sessions, and the
//! commands and addresses the state machine is driven with. Each holds: no panic, a return
//! within the watchdog's limit, the documented bounds on what comes out, and round trips where
//! they exist. `SIG_FUZZ_ITERATIONS`, `SIG_FUZZ_SEED` and `SIG_FUZZ_CASE` are described in
//! `tools/fuzz/mod.rs`.

#[path = "../../../tools/fuzz/mod.rs"]
mod fuzz;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use fuzz::{iterations, run, Case, Rng};
use sighurt_api::engine::EngineCapabilities;
use sighurt_api::ids::SeatId;
use sighurt_api::input::{
    ButtonState, InputEvent, InputKind, Key, KeyCode, KeyEvent, KeyboardFocus, Modifiers,
};
use sighurt_api::keymap::KeymapKind;
use sighurt_core::config::{Config, DEFAULT_CONFIG, RENDERERS};
use sighurt_core::session::{Session, SessionSegment, SessionTab, MAX_TABS, VERSION};
use sighurt_core::tabs::{MAX_TEXT, MAX_URL};
use sighurt_core::{
    keys, router, update, BrowserEvent, BrowserState, ChromeChecks, Environment, OpenRequest,
};

const LIMIT: Duration = Duration::from_secs(2);

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

/// A build with Servo, WPE (so host routing has a second engine), Wasm and the demo engine, a
/// keymap "example" with the bindings' rules, and the standard chrome.
fn env() -> Environment {
    let full = EngineCapabilities {
        history_traversal: true,
        page_zoom: true,
        ..EngineCapabilities::default()
    };
    let engines = ["servo", "wpe", "wasm", "demo"].map(|e| (e.to_owned(), full));
    let settings: [(_, &'static [&'static str]); 3] = [
        (
            "servo",
            &[
                "user-agent",
                "storage",
                "experimental-features",
                "preferences",
            ],
        ),
        ("wpe", &["user-agent", "storage"]),
        ("wasm", &["storage"]),
    ];
    Environment {
        engines: engines.into(),
        engine_settings: settings.map(|(e, s)| (e.to_owned(), s)).into(),
        home_dir: Some("/home/me".into()),
        keymaps: vec![KeymapKind {
            name: "example",
            ..keys::KIND
        }],
        chromes: vec![ChromeChecks {
            name: "standard",
            check: |table| {
                table
                    .keys()
                    .map(|key| format!("{key}: unknown key"))
                    .collect()
            },
            commands: &["toggle-tabs"],
        }],
        color_names: &COLORS,
        renderers: RENDERERS,
        ..Environment::default()
    }
}

/// The commented-out examples of the default file, uncommented: a valid configuration that
/// touches every table.
fn documented_examples() -> String {
    let examples: String = DEFAULT_CONFIG
        .lines()
        .filter_map(|line| line.strip_prefix("#   "))
        .filter(|line| line.starts_with(['[', '"']) || line.contains(" = "))
        .filter_map(|line| line.split("  #").next())
        .map(|line| format!("{line}\n"))
        .collect();
    format!("version = 1\n{examples}")
}

const RICH_CONFIG: &str = r##"version = 1
home = "https://example.com/"
search = "https://duckduckgo.com/html/?q={}"
zoom = 1.25
[keywords]
w = "https://en.wikipedia.org/w/index.php?search={}"
gh = "https://github.com/"
[engines]
default = "servo"
enabled = ["servo", "wasm", "demo", "wpe"]
[engines.servo]
user-agent = "Mozilla/5.0 Sighurt"
storage = "ephemeral"
experimental-features = true
[engines.servo.preferences]
dom_adoptedstylesheet_enabled = true
shell_background_color_rgba = [1.0, 0.5, 0.5, 1.0]
[engines.wpe]
storage = "profile"
[routing.wasm]
extensions = ["wasm", ".WASM"]
origins = ["http://localhost:8080"]
schemes = ["wasm"]
[routing.wpe]
hosts = ["*.google.com", "google.com", "10.0.0.1", "[::1]"]
[input]
keymap = "bindings"
scroll-speed = 2.5
[input.buttons]
back = "page.back"
forward = "page.forward"
[keys]
"ctrl-t" = "tab.new"
"ctrl-shift-tab" = "tab.previous"
"ctrl-+" = "page.zoom-in"
"ctrl-code:KeyQ" = "browser.quit"
[keys.page]
"f5" = "page.reload"
"alt-j" = "page.send-key down"
[keys.location]
"alt-enter" = "page.open https://example.com"
[tui]
mode = "text"
graphics = "kitty"
fps = 30
mouse = false
[tui.keys]
"m" = "chrome.toggle-mode"
[ui]
chrome = "standard"
renderer = "cpu"
font = "DejaVu Sans"
font-size = 14
[ui.colors]
bg = "#0a0a0b"
selection = "#60a5fa55"
[chrome.standard]
[downloads]
directory = "~/Downloads"
[session]
restore = false
"##;

const TABLES: &[&str] = &[
    "",
    "engines",
    "engines.servo",
    "engines.servo.preferences",
    "engines.wpe",
    "engines.x",
    "routing.wasm",
    "routing.wpe",
    "routing.servo",
    "routing.x",
    "input",
    "input.buttons",
    "keys",
    "keys.page",
    "keys.location",
    "keys.other",
    "keymap.example",
    "tui",
    "tui.keys",
    "tui.keys.page",
    "ui",
    "ui.colors",
    "chrome.standard",
    "chrome.line",
    "downloads",
    "session",
    "keywords",
    "version",
    "a.b.c",
];

const KEYS: &[&str] = &[
    "version",
    "home",
    "search",
    "zoom",
    "default",
    "enabled",
    "user-agent",
    "storage",
    "experimental-features",
    "preferences",
    "dom_adoptedstylesheet_enabled",
    "shell_background_color_rgba",
    "schemes",
    "extensions",
    "origins",
    "hosts",
    "keymap",
    "scroll-speed",
    "back",
    "forward",
    "ctrl-t",
    "\"ctrl-t\"",
    "\"ctrl-+\"",
    "\"code:KeyQ\"",
    "\"f5\"",
    "mode",
    "graphics",
    "fps",
    "mouse",
    "default-keys",
    "chrome",
    "renderer",
    "font",
    "font-size",
    "bg",
    "text",
    "selection",
    "directory",
    "restore",
    "w",
    "gh",
    "localhost",
    "",
    "\"\"",
    "x y",
    "'quoted'",
    "tabs",
    "status",
    "buttons",
    "compact",
    "engine-label",
    "position",
];

fn toml_string(rng: &mut Rng, text: &str) -> String {
    match rng.below(5) {
        0 => format!("'{}'", text.replace(['\'', '\n', '\r'], "")),
        _ => {
            let mut out = String::from("\"");
            for c in text.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    c if (c as u32) < 0x20 || c == '\u{7f}' => {
                        out.push_str(&format!("\\u{:04X}", c as u32))
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
    }
}

const STRINGS: &[&str] = &[
    "servo",
    "wpe",
    "wasm",
    "demo",
    "x",
    "",
    "auto",
    "cpu",
    "wgpu",
    "vulkan",
    "standard",
    "line",
    "text",
    "graphics",
    "kitty",
    "sixel",
    "none",
    "profile",
    "ephemeral",
    "top",
    "left",
    "hidden",
    "tab.new",
    "tab.close 3",
    "page.open https://a.test",
    "page.open-in servo",
    "chrome.toggle-tabs",
    "chrome.nope",
    "page.send-key down",
    "ctrl-t",
    "https://example.com/",
    "https://x.test/?q={}",
    "http://localhost:8080",
    "ftp://x.test/{}",
    "sig://home",
    "file:///tmp",
    "javascript:{}",
    "#fff",
    "#0a0a0b",
    "#60a5fa55",
    "#ggg",
    "~/Downloads",
    "/tmp",
    "relative",
    "*.example.com",
    "example.com",
    "10.0.0.1",
    "[::1]",
    "[::ffff:127.0.0.1]",
    "xn--nxasmq6b",
    "ΕΛΛΗΝΙΚΑ.gr",
    "Mozilla/5.0 (X11)",
    "wasm",
    ".wasm",
    "WASM",
    "a b",
    "data",
    "about",
    "sig",
    "http",
];

fn toml_value(rng: &mut Rng, depth: usize) -> String {
    match rng.below(if depth > 3 { 6 } else { 9 }) {
        0 => rng
            .pick(&[
                "0",
                "1",
                "-1",
                "10",
                "30",
                "60",
                "61",
                "9",
                "32",
                "33",
                "9223372036854775807",
                "-9223372036854775808",
                "0x10",
                "1_000",
            ])
            .to_string(),
        1 => rng
            .pick(&[
                "1.0", "0.0", "-0.0", "nan", "inf", "-inf", "1e308", "1e-320", "2.5", "0.1",
                "10.0", "10.01", "0.25", "5.0", "1e400",
            ])
            .to_string(),
        2 => rng.pick(&["true", "false"]).to_string(),
        3 | 4 => {
            let s = *rng.pick(STRINGS);
            toml_string(rng, s)
        }
        5 => {
            let s = rng.string(20);
            toml_string(rng, &s)
        }
        6 => {
            let n = rng.below(5);
            let items: Vec<String> = (0..n).map(|_| toml_value(rng, depth + 1)).collect();
            format!("[{}]", items.join(", "))
        }
        7 => {
            let n = rng.below(4);
            let items: Vec<String> = (0..n)
                .map(|_| format!("{} = {}", rng.pick(KEYS), toml_value(rng, depth + 1)))
                .collect();
            format!("{{ {} }}", items.join(", "))
        }
        _ => "2026-11-04T12:00:00Z".into(),
    }
}

fn random_config(rng: &mut Rng) -> String {
    let mut text = String::new();
    if !rng.one_in(10) {
        text.push_str("version = 1\n");
    }
    for _ in 0..rng.range(0, 14) {
        let table = rng.pick(TABLES);
        if !table.is_empty() {
            text.push_str(&format!("[{table}]\n"));
        }
        for _ in 0..rng.range(0, 4) {
            text.push_str(&format!("{} = {}\n", rng.pick(KEYS), toml_value(rng, 0)));
        }
    }
    text
}

/// A configuration to try: valid seeds, mutated seeds, a random one in the schema's shape,
/// the seeds' lines shuffled, or arbitrary text.
fn config_text(rng: &mut Rng, examples: &str) -> String {
    match rng.below(10) {
        0 => examples.to_owned(),
        1 => RICH_CONFIG.to_owned(),
        2..=4 => {
            let seed = if rng.one_in(2) { examples } else { RICH_CONFIG };
            rng.mutate(seed)
        }
        5 => {
            let mut text = rng.mutate(examples);
            text = rng.mutate(&text);
            text
        }
        6..=7 => random_config(rng),
        8 => {
            let mut lines: Vec<&str> = RICH_CONFIG.lines().chain(examples.lines()).collect();
            let mut out = String::new();
            for _ in 0..rng.range(1, 40) {
                out.push_str(lines.remove(rng.below(lines.len())));
                out.push('\n');
            }
            out
        }
        _ => format!("version = 1\n{}", rng.string(200)),
    }
}

/// What every valid configuration promises, whatever the file said.
fn check_config(config: &Config) {
    assert!(
        url::Url::parse(config.home()).is_ok(),
        "home {:?}",
        config.home()
    );
    assert!(config.search().contains("{}"));
    assert!(
        config.zoom().is_finite() && (0.25..=5.0).contains(&config.zoom()),
        "zoom {}",
        config.zoom()
    );
    assert!(
        (0.1..=10.0).contains(&config.scroll_speed()),
        "scroll {}",
        config.scroll_speed()
    );
    assert!(
        (9..=32).contains(&config.font_size()),
        "font size {}",
        config.font_size()
    );
    assert!(
        (1..=60).contains(&config.tui().fps),
        "fps {}",
        config.tui().fps
    );
    assert!(RENDERERS.contains(&config.renderer()));
    assert!(config.is_enabled(config.default_engine()));
    for (keyword, template) in config.keywords() {
        assert!((1..=32).contains(&keyword.len()), "{keyword:?}");
        // (The url crate drops tabs and newlines inside a scheme, so "htt\tps:" is one.)
        let url = url::Url::parse(&template.replace("{}", "q")).unwrap();
        assert!(matches!(url.scheme(), "http" | "https"), "{template:?}");
    }
    for route in config.routes() {
        assert!(route.hosts.iter().all(|h| !h.is_empty()), "{route:?}");
    }
    if let Some(font) = config.font() {
        assert!(!font.is_empty());
    }
}

#[test]
fn fuzz_config_parse() {
    let examples = documented_examples();
    let env = env();
    // The seeds themselves are valid.
    Config::parse(Some(&examples), &env).unwrap();
    Config::parse(Some(RICH_CONFIG), &env).unwrap();
    let mut valid = 0;
    let cases = iterations(100);
    run("fuzz_config", cases, LIMIT, |case| {
        let text = config_text(&mut case.rng, &examples);
        case.note(|| format!("{text:?}"));
        let first = Config::parse(Some(&text), &env);
        if let Ok(config) = &first {
            valid += 1;
            check_config(config);
        }
        // Parsing is a pure function of the text and the build.
        assert_eq!(first, Config::parse(Some(&text), &env), "deterministic");
    });
    // The inputs get past the parser's first checks often enough to test the rest.
    eprintln!("fuzz_config: {valid} of {cases} valid");
    assert!(
        cases < 500 || valid * 20 >= cases,
        "{valid} of {cases} valid"
    );
}

/// Arbitrary bytes, as the executable would read a damaged file lossily.
#[test]
fn fuzz_config_parse_bytes() {
    let env = env();
    run("fuzz_config_bytes", iterations(100), LIMIT, |case| {
        let bytes = if case.rng.one_in(2) {
            let n = case.rng.below(400);
            case.rng.bytes(n)
        } else {
            case.rng.mutate_bytes(RICH_CONFIG.as_bytes())
        };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        case.note(|| format!("{text:?}"));
        if let Ok(config) = Config::parse(Some(&text), &env) {
            check_config(&config);
        }
    });
}

/// Deeply nested and very large values: the parser's recursion and allocation are bounded.
#[test]
fn deep_and_large_configs_are_refused_or_read_without_a_crash() {
    let env = env();
    for depth in [100, 1_000, 100_000] {
        let nested = format!(
            "version = 1\nhome = {}{}",
            "[".repeat(depth),
            "]".repeat(depth)
        );
        assert!(Config::parse(Some(&nested), &env).is_err(), "depth {depth}");
        let inline = format!(
            "version = 1\n[keywords]\nw = {}1{}",
            "{ a = ".repeat(depth),
            " }".repeat(depth)
        );
        assert!(
            Config::parse(Some(&inline), &env).is_err(),
            "inline depth {depth}"
        );
        let dotted = format!("version = 1\n[{}]\n", vec!["a"; depth].join("."));
        assert!(
            Config::parse(Some(&dotted), &env).is_err(),
            "dotted depth {depth}"
        );
    }
    let big = format!(
        "version = 1\nhome = \"https://a.test/{}\"\n",
        "x".repeat(10 * 1024 * 1024)
    );
    let _ = Config::parse(Some(&big), &env);
    let many = format!(
        "version = 1\n[keywords]\n{}",
        (0..50_000)
            .map(|i| format!("k{i} = \"https://a.test/{{}}\"\n"))
            .collect::<String>()
    );
    let _ = Config::parse(Some(&many), &env);
    let keys = format!(
        "version = 1\n[keys]\n{}",
        (0..50_000)
            .map(|i| format!("\"ctrl-F{}-{i}\" = \"tab.new\"\n", i % 30))
            .collect::<String>()
    );
    let _ = Config::parse(Some(&keys), &env);
}

fn address(rng: &mut Rng) -> String {
    let pieces: &[&str] = &[
        "example.com",
        "localhost",
        "127.0.0.1",
        "[::1]",
        ":8080",
        "/",
        "?",
        "#",
        " ",
        "  ",
        "\t",
        "\n",
        "https://",
        "http://",
        "sig:",
        "data:",
        "about:",
        "file://",
        "~/",
        "/tmp/",
        "w ",
        "gh",
        "k ",
        "%",
        "%zz",
        "%e2%82",
        "..",
        ".",
        "@",
        "a:b@c",
        "ünï.cöde",
        "\u{202e}",
        "\u{0}",
        "{}",
        "-",
        "xn--",
        "1.2.3.4",
        "javascript:",
        "mailto:",
    ];
    let mut text = String::new();
    for _ in 0..rng.range(0, 6) {
        if rng.one_in(4) {
            text.push_str(&rng.string(6));
        } else {
            text.push_str(rng.pick(pieces));
        }
    }
    text
}

#[test]
fn fuzz_fixup_and_routing() {
    let env = env();
    let config = Config::parse(Some(RICH_CONFIG), &env).unwrap();
    let home = std::path::PathBuf::from("/home/me");
    run("fuzz_fixup", iterations(100) * 3, LIMIT, |case| {
        let input = address(&mut case.rng);
        case.note(|| format!("{input:?}"));
        let fixed = router::fixup(&input, config.search(), config.keywords(), Some(&home));
        case.note(|| format!("{input:?} fixed to {fixed:?}"));
        if input.trim().is_empty() {
            assert!(fixed.is_empty());
        } else {
            assert!(!fixed.is_empty(), "something non-empty stays non-empty");
        }
        // Output never explodes: a query is percent-encoded to at most three times its size.
        assert!(fixed.len() <= config.search().len() + 3 * input.len() + 64 + 3 * 16 * 1024);
        // Fixing up an address that came out of it, parsed as a URL, is a fixed point.
        if let Some(routed) = router::routed(&fixed, &config) {
            let _ = routed;
            let again = router::fixup(&fixed, config.search(), config.keywords(), Some(&home));
            assert_eq!(again, fixed, "an address stays as it is");
        }
        // Routing any URL text, with and without an explicit engine, never panics.
        if let Ok(url) = url::Url::parse(&fixed) {
            let explicit = [None, Some("servo"), Some("wpe"), Some("nope"), Some("")];
            for explicit in explicit {
                let _ = router::route(&url, &config, explicit);
            }
        }
        let _ = router::file_url(std::path::Path::new(&input));
    });
}

/// Hosts, schemes and patterns, against a configuration whose routes claim some of them.
#[test]
fn fuzz_routing_of_raw_urls() {
    let env = env();
    let config = Config::parse(Some(RICH_CONFIG), &env).unwrap();
    run("fuzz_routing", iterations(100) * 2, LIMIT, |case| {
        let mut text = format!(
            "{}://{}{}{}",
            case.rng
                .pick(&["http", "https", "file", "wasm", "sig", "data", "ftp", "HTTPS", "x"]),
            case.rng.pick(&[
                "",
                "google.com",
                "a.google.com",
                "a.b.google.com",
                ".google.com",
                "GOOGLE.com.",
                "10.0.0.1",
                "[::1]",
                "0x0a.0.0.1",
                "localhost:8080",
                "ünï.test"
            ]),
            case.rng.pick(&[
                "",
                "/",
                "/a.wasm",
                "/a.WASM",
                "/a.wasm?x",
                "/.wasm",
                "/a.",
                "/a/b.wasm#f",
                "/%2e%2ewasm",
                "/a..wasm"
            ]),
            case.rng.string(6),
        );
        if case.rng.one_in(3) {
            text = case.rng.mutate(&text);
        }
        case.note(|| format!("{text:?}"));
        let routed = router::routed(&text, &config);
        if let Some(sighurt_core::router::Routed::Engine { engine, .. }) = &routed {
            assert!(config.is_enabled(engine));
        }
    });
}

fn session_text(rng: &mut Rng) -> String {
    let segment = |rng: &mut Rng| {
        let entries = rng.range(0, 4);
        SessionSegment {
            engine: rng
                .one_in(2)
                .then(|| rng.pick(&["servo", "wasm", "demo", "", "x y"]).to_string()),
            entries: (0..entries)
                .map(|_| {
                    rng.pick(&[
                        "https://a.test/",
                        "sig://home",
                        "file:///tmp/a.wasm",
                        "",
                        "x",
                    ])
                    .to_string()
                        + &rng.string(4)
                })
                .collect(),
            current: rng.below(5),
            title: rng.one_in(2).then(|| rng.string(30)),
        }
    };
    let tabs = (0..rng.range(0, 5))
        .map(|_| SessionTab {
            current: rng.below(4),
            zoom: *rng.pick(&[
                1.0,
                1.25,
                0.0,
                -3.0,
                1e300,
                1e-300,
                0.25,
                5.0,
                f64::NAN,
                f64::INFINITY,
            ]),
            segments: (0..rng.range(0, 4)).map(|_| segment(rng)).collect(),
        })
        .collect();
    let session = Session {
        version: VERSION,
        active: rng.below(6),
        tabs,
        notices: Vec::new(),
    };
    session.to_text()
}

#[test]
fn fuzz_session_parse_and_round_trip() {
    run("fuzz_session", iterations(100) * 2, LIMIT, |case| {
        let mut text = session_text(&mut case.rng);
        if case.rng.one_in(2) {
            text = case.rng.mutate(&text);
        }
        if case.rng.one_in(10) {
            text = String::from_utf8_lossy(&case.rng.mutate_bytes(text.as_bytes())).into_owned();
        }
        case.note(|| format!("{text:?}"));
        let Ok(session) = Session::parse(&text) else {
            return;
        };
        assert!(session.tabs.len() <= MAX_TABS);
        assert!(session.active < session.tabs.len() || session.active == 0);
        for tab in &session.tabs {
            assert!(tab.zoom.is_finite());
            assert!(tab.current < tab.segments.len() || tab.segments.is_empty());
        }
        // What was read survives being saved and read again, and reading is idempotent.
        let saved = session.to_text();
        let mut again = Session::parse(&saved)
            .unwrap_or_else(|e| panic!("saved session unreadable: {e}\n{saved}"));
        again.notices = session.notices.clone();
        assert_eq!(again, session, "round trip");
    });
}

#[test]
fn sessions_with_too_many_or_huge_parts_are_bounded() {
    let tab = "[[tabs]]\ncurrent = 0\nzoom = 1.0\n";
    let text = format!("version = 1\nactive = 5\n{}", tab.repeat(MAX_TABS + 500));
    let read = Session::parse(&text).unwrap();
    assert_eq!(read.tabs.len(), MAX_TABS);
    let entries = vec!["\"https://a.test/\""; 100].join(", ");
    let text = format!("version = 1\n[[tabs]]\ncurrent = 0\nzoom = 1.0\n[[tabs.segments]]\nengine = \"servo\"\nentries = [{entries}]\ncurrent = 0\n");
    assert!(
        Session::parse(&text).unwrap().tabs.is_empty(),
        "history over its bound"
    );
    let url = "x".repeat(MAX_URL + 1);
    let text = format!("version = 1\n[[tabs]]\ncurrent = 0\nzoom = 1.0\n[[tabs.segments]]\nengine = \"servo\"\nentries = [\"{url}\"]\ncurrent = 0\n");
    assert!(
        Session::parse(&text).unwrap().tabs.is_empty(),
        "url over its bound"
    );
    let title = "é".repeat(MAX_TEXT);
    let text = format!("version = 1\n[[tabs]]\ncurrent = 0\nzoom = 1.0\n[[tabs.segments]]\nengine = \"servo\"\nentries = [\"https://a.test/\"]\ncurrent = 0\ntitle = \"{title}\"\n");
    assert!(
        Session::parse(&text).unwrap().tabs.is_empty(),
        "title over its bound"
    );
    let nested = format!(
        "version = 1\ntabs = {}{}",
        "[".repeat(100_000),
        "]".repeat(100_000)
    );
    assert!(Session::parse(&nested).is_err());
    let unknown: String = (0..5000).map(|i| format!("field{i} = 1\n")).collect();
    let read = Session::parse(&format!("version = 1\n{unknown}")).unwrap();
    assert!(read.notices.iter().all(|n| n.len() < 2000));
}

fn key_event(rng: &mut Rng) -> KeyEvent {
    let mods = |rng: &mut Rng| Modifiers {
        shift: rng.one_in(3),
        control: rng.one_in(3),
        alt: rng.one_in(3),
        alt_graph: rng.one_in(6),
        logo: rng.one_in(6),
        caps_lock: rng.one_in(4),
        num_lock: rng.one_in(4),
    };
    KeyEvent {
        code: KeyCode(*rng.pick(&[
            1,
            14,
            15,
            16,
            20,
            28,
            57,
            105,
            106,
            103,
            108,
            59,
            100,
            125,
            300,
            0,
            u32::MAX,
        ])),
        key: match rng.below(3) {
            0 => Key::Character(rng.string(2)),
            1 => Key::Named(
                rng.pick(&["Enter", "Tab", "ArrowDown", "F5", "Escape", ""])
                    .to_string(),
            ),
            _ => Key::Character("t".into()),
        },
        state: *rng.pick(&[
            ButtonState::Pressed,
            ButtonState::Pressed,
            ButtonState::Released,
        ]),
        repeat: rng.one_in(5),
        modifiers: mods(rng),
        consumed: mods(rng),
        text: rng.one_in(2).then(|| rng.string(3)),
    }
}

/// The state machine, driven with random commands, addresses, keys and configurations, with
/// no engine answering: it never panics and its effects stay finite.
#[test]
fn fuzz_state_machine_commands() {
    let env = env();
    let rich = Config::parse(Some(RICH_CONFIG), &env).unwrap();
    let examples = documented_examples();
    run(
        "fuzz_state",
        iterations(100) / 2,
        Duration::from_secs(5),
        |case: &mut Case| {
            let config = if case.rng.one_in(2) {
                rich.clone()
            } else {
                Config::parse(Some(&examples), &env).unwrap()
            };
            let mut state = BrowserState::new(config, env.clone());
            let session = case
                .rng
                .one_in(2)
                .then(|| Session::parse(&session_text(&mut case.rng)).ok())
                .flatten();
            let open = (0..case.rng.below(3))
                .map(|_| OpenRequest {
                    input: address(&mut case.rng),
                    engine: case
                        .rng
                        .one_in(3)
                        .then(|| case.rng.pick(&["servo", "wpe", "nope", ""]).to_string()),
                })
                .collect();
            let effects = update(&mut state, BrowserEvent::Start { session, open });
            assert!(effects.len() < 10_000);
            let seat = SeatId::next();
            for _ in 0..case.rng.range(1, 30) {
                let event = match case.rng.below(5) {
                    0 | 1 => {
                        let mut text = case
                            .rng
                            .pick(&sighurt_api::commands::Command::NAMES)
                            .to_string();
                        text.push(' ');
                        text.push_str(&address(&mut case.rng));
                        match text.parse() {
                            Ok(command) => BrowserEvent::Command(command),
                            Err(_) => continue,
                        }
                    }
                    2 => BrowserEvent::Open(OpenRequest {
                        input: address(&mut case.rng),
                        engine: None,
                    }),
                    3 => BrowserEvent::Keyboard {
                        event: InputEvent {
                            seat,
                            kind: InputKind::Key(key_event(&mut case.rng)),
                        },
                        at: Instant::now(),
                    },
                    _ => match Config::parse(Some(&random_config(&mut case.rng)), &env) {
                        Ok(config) => BrowserEvent::ConfigReloaded(Ok(Box::new(config))),
                        Err(error) => BrowserEvent::ConfigReloaded(Err(error)),
                    },
                };
                case.note(|| format!("{event:?}"));
                let effects = update(&mut state, event);
                assert!(effects.len() < 10_000);
            }
            let _ = KeyboardFocus::Page;
        },
    );
}

/// A page.open of a huge address is refused, not sent on.
#[test]
fn a_huge_address_is_refused() {
    let env = env();
    let config = Config::parse(Some(RICH_CONFIG), &env).unwrap();
    let mut state = BrowserState::new(config, env);
    update(
        &mut state,
        BrowserEvent::Start {
            session: None,
            open: Vec::new(),
        },
    );
    let huge = format!("https://a.test/{}", "x".repeat(MAX_URL + 10));
    let effects = update(
        &mut state,
        BrowserEvent::Command(format!("page.open {huge}").parse().unwrap()),
    );
    assert!(
        effects
            .iter()
            .all(|e| !format!("{e:?}").contains(&huge[..200])),
        "the address was passed on"
    );
    let query = "a b ".repeat(MAX_URL / 2);
    let _ = update(
        &mut state,
        BrowserEvent::Command(format!("page.open {query}").parse().unwrap()),
    );
    let _ = BTreeMap::<u8, u8>::new();
}
