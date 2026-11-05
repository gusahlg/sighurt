//! Which engine opens a URL, and turning what the user typed into a URL.
//!
//! Routing is deterministic and does no I/O; there is no HEAD or GET probe, and the engine that
//! loads a page owns its networking. The rules, in order:
//!
//! 1. An explicit engine choice wins, if that engine is enabled and may open the URL's scheme
//!    (http, https, file, or one it is configured for). `sig:` never goes to an engine.
//! 2. `sig:` URLs are Sighurt's internal pages: `sig://home` and the ones the build lacks are the
//!    chrome's ([`Route::Internal`], which shows "no such page" for any but home); the pages of
//!    the build ([`Config::internal_pages`]) are Wasm guests ([`Route::Page`]), which open in
//!    the wasm engine ([`PAGE_ENGINE`]) and need it enabled. A page's name is the URL's host in
//!    lower case: `sig:help`, or one with a user name, password or port, is no page.
//!    [`canonical`] gives the address a page view loads. In a build without the wasm engine a
//!    `sig://<name>` other than home says why it can't open.
//! 3. Other schemes go to the engine configured for them, or are an error.
//! 4. A `file:` URL goes to the engine that claims its extension: local apps, which the engine
//!    validates when it loads them.
//! 5. An http(s) URL goes to an engine only through a narrow rule: one of the engine's
//!    configured origins, and one of its extensions,
//! 6. or one of its host patterns (`[routing.<engine>] hosts`). Of the patterns that match a
//!    host, an exact one beats any `*.` wildcard, and a longer wildcard a shorter one.
//! 7. Everything else, ordinary web pages included, goes to the default engine.
//!
//! The engine must be enabled; the rules of an engine the build lacks are left out
//! ([`Config::routes`]). Extensions are hints for choosing a loader, not proof of what
//! the content is. Content the engine can't show becomes a download (the engine asks for one)
//! or a clear error. The same rules decide where a link a user clicks goes (`crate::state`).
//! [`routed`] is that choice for a URL string, and says whether a rule named the engine.
//!
//! Typed text becomes a URL in [`fixup`]: a URL stays one, then search keywords (`[keywords]`),
//! paths, host names, and a search for anything else.

use std::collections::BTreeMap;
use std::path::Path;

use url::Url;

use crate::config::{Config, EngineRoutes};

/// The engine that runs the internal pages ([`Route::Page`]).
pub const PAGE_ENGINE: &str = "wasm";

/// Where a URL goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// A `sig:` URL the chrome draws: `sig://home`, or a page that doesn't exist.
    Internal,
    /// One of the build's internal pages, by name (`help`), in [`PAGE_ENGINE`].
    Page(String),
    /// The engine with this name.
    Engine(String),
    /// Nothing can open it, and why, for the user.
    Error(String),
}

/// Routes `url`, with the engine the user chose if `explicit` names one.
pub fn route(url: &Url, config: &Config, explicit: Option<&str>) -> Route {
    let scheme = url.scheme();
    let routes = config.routes();
    let engine = match explicit {
        Some(_) if scheme == "sig" => {
            return Route::Error("sig: pages are Sighurt's own and open in no engine".into())
        }
        Some(engine) => {
            let listed =
                routes_of(routes, engine).is_some_and(|r| r.schemes.iter().any(|s| s == scheme));
            if !(matches!(scheme, "http" | "https" | "file") || listed) {
                return Route::Error(format!("the {engine} engine doesn't open {scheme}: URLs"));
            }
            engine.to_owned()
        }
        None => return classify(url, config).into(),
    };
    if !config.is_enabled(&engine) {
        return Route::Error(format!(
            "the {engine} engine is not enabled in the configuration"
        ));
    }
    Route::Engine(engine)
}

/// Where [`route`] sends a URL when the user did not name an engine, and whether a
/// configured rule named that engine.
///
/// Hosts and paths are whatever [`Url`] produced: IDNA for hosts, and the path's
/// percent-encoded form, not a decoded one. An http(s) or file URL that no rule claims
/// is the default engine with `matched` false.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Routed {
    /// A `sig:` URL the chrome draws: `sig://home`, or a page that doesn't exist.
    Internal,
    /// One of the build's internal pages, by name, in [`PAGE_ENGINE`].
    Page(String),
    /// An engine opens it.
    Engine {
        /// The engine's name.
        engine: String,
        /// A scheme, extension, origin or host rule named `engine`. False when no rule
        /// claimed the URL and it falls through to the default engine.
        matched: bool,
    },
    /// Nothing can open it, and why, for the user.
    Error(String),
}

impl From<Routed> for Route {
    fn from(routed: Routed) -> Self {
        match routed {
            Routed::Internal => Self::Internal,
            Routed::Page(name) => Self::Page(name),
            Routed::Engine { engine, .. } => Self::Engine(engine),
            Routed::Error(message) => Self::Error(message),
        }
    }
}

/// Parses `address` with the `url` crate and routes it as [`route`] does when no engine
/// was chosen explicitly.
///
/// `None` means `address` is not a URL. The parse is the one tabs use, so a Unicode host
/// and a percent-encoded path match the same rules there.
pub fn routed(address: &str, config: &Config) -> Option<Routed> {
    Some(classify(&Url::parse(address).ok()?, config))
}

/// [`route`] with no explicit engine, plus whether a rule matched.
fn classify(url: &Url, config: &Config) -> Routed {
    let scheme = url.scheme();
    let routes = config.routes();
    let (engine, matched) = if scheme == "sig" {
        return internal(url, config);
    } else if !matches!(scheme, "http" | "https" | "file") {
        match routes
            .iter()
            .find(|r| r.schemes.iter().any(|s| s == scheme))
        {
            Some(route) => (route.engine.clone(), true),
            None => return Routed::Error(format!("no engine opens {scheme}: URLs")),
        }
    } else {
        let extension = extension(url);
        let claims = |route: &&EngineRoutes| {
            extension
                .as_ref()
                .is_some_and(|e| route.extensions.contains(e))
        };
        let rule = if scheme == "file" {
            routes.iter().find(claims)
        } else {
            let origin = url.origin().ascii_serialization();
            let host = url.host_str().unwrap_or_default();
            routes
                .iter()
                .filter(claims)
                .find(|route| route.origins.contains(&origin))
                .or_else(|| claiming_host(routes, host))
        };
        match rule {
            Some(route) => (route.engine.clone(), true),
            None => (config.default_engine().to_owned(), false),
        }
    };
    if !config.is_enabled(&engine) {
        return Routed::Error(format!(
            "the {engine} engine is not enabled in the configuration"
        ));
    }
    Routed::Engine { engine, matched }
}

/// Where a `sig:` URL goes: a page of the build, or the chrome's.
fn internal(url: &Url, config: &Config) -> Routed {
    let Some(name) = page_name(url) else {
        return Routed::Internal;
    };
    if config.internal_pages().contains(&name) {
        return match config.is_enabled(PAGE_ENGINE) {
            true => Routed::Page(name),
            false => Routed::Error(format!(
                "sig://{name} runs in the {PAGE_ENGINE} engine, which [engines] enabled leaves out."
            )),
        };
    }
    match name.as_str() {
        "home" => Routed::Internal,
        _ if config.internal_pages().is_empty() => Routed::Error(format!(
            "sig://{name} is not a page of this build of sig: its pages besides sig://home run \
             in the {PAGE_ENGINE} engine, which this build doesn't have."
        )),
        _ => Routed::Internal,
    }
}

/// The page a `sig:` URL names: its host, in lower case, if it has one and no user name,
/// password or port.
fn page_name(url: &Url) -> Option<String> {
    let plain = url.username().is_empty() && url.password().is_none() && url.port().is_none();
    let host = url.host_str().filter(|host| plain && !host.is_empty())?;
    Some(host.to_ascii_lowercase())
}

/// The address a page view loads for `url`, which routes to the page `name`:
/// `sig://<name>`, then the URL's path, query and fragment as given.
pub fn canonical(url: &Url, name: &str) -> String {
    format!("sig://{name}{}", &url[url::Position::BeforePath..])
}

fn routes_of<'a>(routes: &'a [EngineRoutes], engine: &str) -> Option<&'a EngineRoutes> {
    routes.iter().find(|route| route.engine == engine)
}

/// The route with the host pattern that matches `host` most specifically: an exact pattern
/// before any wildcard, a longer wildcard before a shorter one. `*.example.com` matches the
/// hosts under example.com at any depth, not example.com itself. A pattern belongs to one
/// engine, so there is never a tie.
fn claiming_host<'a>(routes: &'a [EngineRoutes], host: &str) -> Option<&'a EngineRoutes> {
    let specificity = |pattern: &String| match pattern.strip_prefix("*.") {
        Some(suffix) => host
            .strip_suffix(suffix)
            .is_some_and(|under| under.ends_with('.'))
            .then_some(suffix.len()),
        None => (*pattern == host).then_some(usize::MAX),
    };
    routes
        .iter()
        .flat_map(|route| route.hosts.iter().map(move |pattern| (pattern, route)))
        .filter_map(|(pattern, route)| Some((specificity(pattern)?, route)))
        .max_by_key(|&(specificity, _)| specificity)
        .map(|(_, route)| route)
}

/// The lower-case extension of the URL's last path segment, if it has one.
fn extension(url: &Url) -> Option<String> {
    let name = url.path_segments()?.next_back()?;
    let (_, extension) = name.rsplit_once('.')?;
    (!extension.is_empty()).then(|| extension.to_lowercase())
}

/// Turns what the user typed (or passed on the command line) into a URL: a URL stays as it is;
/// input whose first word is one of `keywords` uses its template, the rest of the input
/// percent-encoded where its `{}` is (a template without `{}` is used only for the keyword
/// alone); a host name gets a scheme; an absolute path or `~/path` (with `home`, the user's
/// home directory) becomes a `file:` URL; and anything else becomes a search with the `search`
/// template. Empty input stays empty.
pub fn fixup(
    input: &str,
    search: &str,
    keywords: &BTreeMap<String, String>,
    home: Option<&Path>,
) -> String {
    let input = input.trim();
    let has_scheme = |scheme: &str| {
        input
            .get(..scheme.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
    };
    if input.is_empty()
        || input.contains("://")
        || ["sig:", "data:", "about:"].into_iter().any(has_scheme)
    {
        return input.to_owned();
    }
    let (word, rest) = match input.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim_start()),
        None => (input, ""),
    };
    match keywords.get(word) {
        Some(template) if template.contains("{}") => {
            return template.replace("{}", &query_encode(rest))
        }
        Some(template) if rest.is_empty() => return template.clone(),
        _ => {}
    }
    if let (Some(path), Some(home)) = (input.strip_prefix("~/"), home) {
        return file_url(&home.join(path));
    }
    if input.starts_with('/') {
        return file_url(Path::new(input));
    }
    let host = input.split(['/', '?', '#']).next().unwrap_or_default();
    let host_name = host.rsplit_once(':').map_or(host, |(name, _)| name);
    if !input.contains(char::is_whitespace) {
        if matches!(host_name, "localhost" | "127.0.0.1" | "[::1]") {
            return format!("http://{input}");
        }
        if host_name.contains('.') {
            return format!("https://{input}");
        }
    }
    if search.contains("{}") {
        return search.replace("{}", &query_encode(input));
    }
    format!("https://{input}")
}

/// Percent-encodes every byte of `text` but ASCII letters and digits.
fn query_encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => char::from(byte).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// A `file:` URL for `path`, which should be absolute: the executable makes command-line paths
/// absolute before it hands them on.
pub fn file_url(path: &Path) -> String {
    Url::from_file_path(path)
        .map(String::from)
        .unwrap_or_else(|()| format!("file://{}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Environment;
    use sighurt_api::engine::EngineCapabilities;

    /// A build with Servo, the Wasm engine and the demo engine.
    fn env() -> Environment {
        let engines = ["servo", "wasm", "demo"];
        Environment {
            engines: engines
                .map(|e| (e.into(), EngineCapabilities::default()))
                .into(),
            ..Environment::default()
        }
    }

    fn config() -> Config {
        Config::parse(
            Some(
                r#"
            version = 1
            [routing.demo]
            schemes = ["demo"]
            [routing.wasm]
            origins = ["http://localhost:8080"]
            "#,
            ),
            &env(),
        )
        .unwrap()
    }

    fn routed(url: &str, explicit: Option<&str>) -> Route {
        route(&Url::parse(url).unwrap(), &config(), explicit)
    }

    fn engine(name: &str) -> Route {
        Route::Engine(name.into())
    }

    #[test]
    fn routes_without_io() {
        let cases = [
            ("sig://home", Route::Internal),
            ("SIG://Home/", Route::Internal),
            ("sig:nope", Route::Internal),
            ("demo:pattern", engine("demo")),
            ("data:text/html,hi", engine("servo")),
            ("about:blank", engine("servo")),
            ("file:///tmp/app.wasm", engine("wasm")),
            ("file:///tmp/APP.WASM", engine("wasm")),
            ("file:///tmp/page.html", engine("servo")),
            ("file:///tmp/dir/", engine("servo")),
            // The web goes to the default engine, whatever the extension says...
            ("https://example.com", engine("servo")),
            ("https://example.com/archive.zip", engine("servo")),
            ("https://a.test/app.wasm", engine("servo")),
            ("http://localhost:8081/app.wasm", engine("servo")),
            // ...unless a rule names the origin.
            ("http://localhost:8080/apps/app.wasm?x=1#y", engine("wasm")),
            ("http://localhost:8080/index.html", engine("servo")),
        ];
        for (url, expected) in cases {
            assert_eq!(routed(url, None), expected, "{url}");
        }
        assert!(matches!(routed("ftp://example.com", None), Route::Error(e) if e.contains("ftp")));
    }

    #[test]
    fn an_explicit_choice_wins_within_its_schemes() {
        assert_eq!(
            routed("https://a.test/app.wasm", Some("wasm")),
            engine("wasm")
        );
        assert_eq!(
            routed("file:///tmp/page.html", Some("demo")),
            engine("demo")
        );
        assert_eq!(routed("demo:x", Some("demo")), engine("demo"));
        assert!(matches!(routed("data:,x", Some("wasm")), Route::Error(_)));
        assert!(matches!(
            routed("sig://home", Some("servo")),
            Route::Error(_)
        ));
    }

    #[test]
    fn host_rules_send_hosts_to_their_engines_the_most_specific_first() {
        let text = r#"
            version = 1
            [routing.wasm]
            origins = ["https://maps.google.com"]
            hosts = ["*.google.com", "docs.example.com", "127.0.0.1", "bücher.de"]
            [routing.demo]
            hosts = ["maps.google.com", "*.mail.google.com"]
            [routing.servo]
            hosts = ["*.a.mail.google.com"]
            "#;
        let config = Config::parse(Some(text), &env()).unwrap();
        let routed = |url: &str, explicit| route(&Url::parse(url).unwrap(), &config, explicit);
        let cases = [
            ("https://docs.example.com/a?b#c", engine("wasm")),
            // An exact pattern is that host alone.
            ("https://a.docs.example.com/", engine("servo")),
            ("https://example.com/", engine("servo")),
            // A wildcard covers the hosts under its domain at any depth, not the domain.
            ("https://www.google.com/search?q=x", engine("wasm")),
            ("http://WWW.Google.COM:8080/", engine("wasm")),
            ("https://a.b.google.com/", engine("wasm")),
            ("https://google.com/", engine("servo")),
            ("https://notgoogle.com/", engine("servo")),
            // An exact pattern beats a wildcard, a longer wildcard a shorter one.
            ("https://maps.google.com/", engine("demo")),
            ("https://x.maps.google.com/", engine("wasm")),
            ("https://mail.google.com/", engine("wasm")),
            ("https://inbox.mail.google.com/", engine("demo")),
            ("https://z.a.mail.google.com/", engine("servo")),
            // IP addresses match exactly; names as the url crate normalizes them.
            ("http://127.0.0.1:9/", engine("wasm")),
            ("http://127.0.0.2/", engine("servo")),
            ("https://Bücher.de/", engine("wasm")),
            ("https://xn--bcher-kva.de/", engine("wasm")),
            // Only http(s) URLs: a local file goes by its extension.
            ("file:///docs.example.com", engine("servo")),
            // An origin's rule with an extension comes first.
            ("https://maps.google.com/app.wasm", engine("wasm")),
        ];
        for (url, expected) in cases {
            assert_eq!(routed(url, None), expected, "{url}");
        }
        // The user's explicit choice wins.
        assert_eq!(
            routed("https://www.google.com/", Some("servo")),
            engine("servo")
        );
        // An engine that isn't enabled opens nothing.
        let disabled = format!("{text}\n[engines]\nenabled = [\"servo\", \"demo\"]");
        let disabled = Config::parse(Some(&disabled), &env()).unwrap();
        let url = Url::parse("https://www.google.com/").unwrap();
        assert!(
            matches!(route(&url, &disabled, None), Route::Error(e) if e.contains("not enabled"))
        );
    }

    #[test]
    fn disabled_engines_open_nothing() {
        let config = "version = 1\n[engines]\nenabled = [\"servo\"]";
        let config = Config::parse(Some(config), &env()).unwrap();
        let url = Url::parse("file:///tmp/app.wasm").unwrap();
        assert!(matches!(route(&url, &config, None), Route::Error(e) if e.contains("not enabled")));
        assert!(matches!(
            route(&url, &config, Some("wasm")),
            Route::Error(_)
        ));
        let web = Url::parse("https://example.com").unwrap();
        assert_eq!(route(&web, &config, None), engine("servo"));
    }

    #[test]
    fn fixes_up_input() {
        let search = "https://s.test/?q={}";
        let home = Some(Path::new("/home/me"));
        let cases = [
            ("  https://example.com ", "https://example.com"),
            ("sig://home", "sig://home"),
            ("Sig:home", "Sig:home"),
            ("data:text/plain,hi", "data:text/plain,hi"),
            ("example.com/a?b", "https://example.com/a?b"),
            ("localhost:8000", "http://localhost:8000"),
            ("127.0.0.1/x", "http://127.0.0.1/x"),
            ("/tmp/app.wasm", "file:///tmp/app.wasm"),
            ("/tmp/a b.wasm", "file:///tmp/a%20b.wasm"),
            ("~/apps/x.wasm", "file:///home/me/apps/x.wasm"),
            ("rust servo", "https://s.test/?q=rust%20servo"),
            ("sighurt", "https://s.test/?q=sighurt"),
            (
                "räksmörgås?",
                "https://s.test/?q=r%C3%A4ksm%C3%B6rg%C3%A5s%3F",
            ),
            ("", ""),
        ];
        let none = BTreeMap::new();
        for (input, url) in cases {
            assert_eq!(fixup(input, search, &none, home), url, "{input:?}");
        }
        // Without a home directory, ~ is just text.
        assert_eq!(
            fixup("~/x", search, &none, None),
            "https://s.test/?q=%7E%2Fx"
        );
    }

    #[test]
    fn keywords_come_before_paths_hosts_and_search() {
        let search = "https://s.test/?q={}";
        let keywords = BTreeMap::from(
            [
                ("w", "https://w.test/search?q={}&lang=sv"),
                ("gh", "https://github.com/"),
                ("two", "http://t.test/{}/{}"),
                ("tmp", "https://k.test/?q={}"),
                ("localhost2", "https://l.test/{}"),
            ]
            .map(|(k, v)| (k.to_owned(), v.to_owned())),
        );
        let home = Some(Path::new("/home/me"));
        let cases = [
            // A keyword with a query, percent-encoded as a search's.
            (
                "w räksmörgås",
                "https://w.test/search?q=r%C3%A4ksm%C3%B6rg%C3%A5s&lang=sv",
            ),
            (
                "  w   rust  borrow ",
                "https://w.test/search?q=rust%20%20borrow&lang=sv",
            ),
            ("two a/b", "http://t.test/a%2Fb/a%2Fb"),
            // A keyword alone: an empty query, or a template without {} as it is.
            ("w", "https://w.test/search?q=&lang=sv"),
            ("gh", "https://github.com/"),
            // A template without {} isn't used for a query: that is a search.
            ("gh servo", "https://s.test/?q=gh%20servo"),
            // URLs, internal pages, paths and hosts are never keywords.
            ("w://x y", "w://x y"),
            ("gh://servo", "gh://servo"),
            ("sig://home", "sig://home"),
            ("/tmp x", "file:///tmp%20x"),
            ("~/tmp", "file:///home/me/tmp"),
            ("tmp.example", "https://tmp.example"),
            ("localhost:8000", "http://localhost:8000"),
            ("w.test/x", "https://w.test/x"),
            // Only the first word, exactly.
            ("W rust", "https://s.test/?q=W%20rust"),
            ("rust w", "https://s.test/?q=rust%20w"),
        ];
        for (input, url) in cases {
            assert_eq!(fixup(input, search, &keywords, home), url, "{input:?}");
        }
    }

    #[test]
    fn routed_uses_the_url_parser_and_says_whether_a_rule_matched() {
        let text = r#"
            version = 1
            [routing.wasm]
            hosts = ["bücher.de"]
            [routing.demo]
            schemes = ["demo"]
            "#;
        let config = Config::parse(Some(text), &env()).unwrap();
        let decided = |address: &str| super::routed(address, &config).unwrap();
        let engine = |name: &str, matched: bool| Routed::Engine {
            engine: name.into(),
            matched,
        };
        // IDNA, the same host a tab's Url sees: the Unicode form and the punycode form.
        assert_eq!(decided("https://Bücher.de/"), engine("wasm", true));
        assert_eq!(decided("https://xn--bcher-kva.de/a"), engine("wasm", true));
        assert_eq!(decided("https://BÜCHER.DE/x"), engine("wasm", true));
        // No rule: the default engine, and the caller can tell it was not a rule.
        assert_eq!(decided("https://other.test/"), engine("servo", false));
        assert_eq!(decided("file:///tmp/page.html"), engine("servo", false));
        // The built-in wasm extension. A percent-encoded `%77` is `w`, but the path keeps
        // the encoding, so this is not the wasm rule.
        assert_eq!(decided("file:///tmp/app.wasm"), engine("wasm", true));
        assert_eq!(
            decided("file:///tmp/app.%77asm"),
            engine("servo", false),
            "{:?}",
            Url::parse("file:///tmp/app.%77asm").unwrap().path()
        );
        assert_eq!(decided("demo:pattern"), engine("demo", true));
        assert_eq!(decided("sig://home"), Routed::Internal);
        assert!(matches!(decided("ftp://example.com"), Routed::Error(_)));
        assert!(super::routed("not a url", &config).is_none());
        // The same destination as route(), including when the engine is disabled.
        for address in [
            "https://Bücher.de/",
            "https://other.test/",
            "file:///tmp/app.wasm",
            "file:///tmp/app.%77asm",
            "demo:pattern",
            "sig://home",
            "ftp://example.com",
        ] {
            let url = Url::parse(address).unwrap();
            assert_eq!(
                route(&url, &config, None),
                Route::from(super::routed(address, &config).unwrap()),
                "{address}"
            );
        }
        let disabled = format!("{text}\n[engines]\nenabled = [\"servo\", \"demo\"]");
        let disabled = Config::parse(Some(&disabled), &env()).unwrap();
        assert!(matches!(
            super::routed("https://Bücher.de/", &disabled),
            Some(Routed::Error(message)) if message.contains("not enabled")
        ));
    }
}
