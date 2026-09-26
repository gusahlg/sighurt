//! Which engine opens a URL, and turning what the user typed into a URL.
//!
//! Routing is deterministic and does no I/O; there is no HEAD or GET probe, and the engine that
//! loads a page owns its networking. The rules, in order:
//!
//! 1. An explicit engine choice wins, if that engine is enabled and may open the URL's scheme
//!    (http, https, file, or one it is configured for). `sig:` never goes to an engine.
//! 2. `sig:` URLs are Sighurt's internal pages.
//! 3. Other schemes go to the engine configured for them, or are an error.
//! 4. A `file:` URL goes to the engine that claims its extension: local apps, which the engine
//!    validates when it loads them.
//! 5. An http(s) URL goes to an engine only through a narrow rule: one of the engine's
//!    configured origins, and one of its extensions.
//! 6. Everything else, ordinary web pages included, goes to the default engine.
//!
//! The engine must be enabled. Extensions are hints for choosing a loader, not proof of what
//! the content is. Content the engine can't show becomes a download (the engine asks for one)
//! or a clear error.

use std::path::Path;

use url::Url;

use crate::config::{Config, EngineRoutes};

/// Where a URL goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// A `sig:` URL: one of Sighurt's internal pages, which the chrome draws.
    Internal,
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
        None if scheme == "sig" => return Route::Internal,
        None if !matches!(scheme, "http" | "https" | "file") => {
            match routes
                .iter()
                .find(|r| r.schemes.iter().any(|s| s == scheme))
            {
                Some(route) => route.engine.clone(),
                None => return Route::Error(format!("no engine opens {scheme}: URLs")),
            }
        }
        None => {
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
                routes
                    .iter()
                    .filter(claims)
                    .find(|route| route.origins.contains(&origin))
            };
            rule.map_or(config.default_engine(), |route| &route.engine)
                .to_owned()
        }
    };
    if !config.is_enabled(&engine) {
        return Route::Error(format!(
            "the {engine} engine is not enabled in the configuration"
        ));
    }
    Route::Engine(engine)
}

fn routes_of<'a>(routes: &'a [EngineRoutes], engine: &str) -> Option<&'a EngineRoutes> {
    routes.iter().find(|route| route.engine == engine)
}

/// The lower-case extension of the URL's last path segment, if it has one.
fn extension(url: &Url) -> Option<String> {
    let name = url.path_segments()?.next_back()?;
    let (_, extension) = name.rsplit_once('.')?;
    (!extension.is_empty()).then(|| extension.to_lowercase())
}

/// Turns what the user typed (or passed on the command line) into a URL: a URL stays as it is,
/// a host name gets a scheme, an absolute path or `~/path` (with `home`, the user's home
/// directory) becomes a `file:` URL, and anything else becomes a search with the `search`
/// template. Empty input stays empty.
pub fn fixup(input: &str, search: &str, home: Option<&Path>) -> String {
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

    fn config() -> Config {
        Config::parse(Some(
            r#"
            version = 1
            [routing.demo]
            schemes = ["demo"]
            [routing.wasm]
            origins = ["http://localhost:8080"]
            "#,
        ))
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
    fn disabled_engines_open_nothing() {
        let config = Config::parse(Some("version = 1\n[engines]\nenabled = [\"servo\"]")).unwrap();
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
        for (input, url) in cases {
            assert_eq!(fixup(input, search, home), url, "{input:?}");
        }
        // Without a home directory, ~ is just text.
        assert_eq!(fixup("~/x", search, None), "https://s.test/?q=%7E%2Fx");
    }
}
