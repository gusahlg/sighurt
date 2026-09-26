//! Decides what handles a URL: the UI, one of the configured engines, or the download manager.
//! The rules are documented in the default config.

use std::time::Duration;

use crate::config::Config;

#[derive(Clone, Debug, PartialEq)]
pub enum Route {
    /// A `sig:` URL. The scheme is reserved for the UI's own pages: engines never get these,
    /// and pages can't open them.
    Ui,
    Engine(String),
    Download,
    /// Only the server can tell: look up the Content-Type with [`content_type`], then call
    /// [`route_mime`].
    Probe,
    Invalid(String),
}

/// Routes `url` without doing any I/O.
pub fn route(url: &str, config: &Config) -> Route {
    let parsed = match url::Url::parse(url) {
        Ok(parsed) => parsed,
        Err(e) => return Route::Invalid(format!("{url}: {e}")),
    };
    let scheme = parsed.scheme();
    if scheme == "sig" {
        return Route::Ui;
    }
    if !matches!(scheme, "http" | "https" | "file") {
        return match config
            .engines
            .iter()
            .find(|e| e.schemes.iter().any(|s| s == scheme))
        {
            Some(engine) => Route::Engine(engine.name.clone()),
            None => Route::Invalid(format!("no engine handles {scheme}: URLs")),
        };
    }
    let extension = parsed
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext.to_ascii_lowercase());
    if let Some(ext) = extension {
        if let Some(engine) = config.engines.iter().find(|e| e.extensions.contains(&ext)) {
            return Route::Engine(engine.name.clone());
        }
    }
    if scheme == "file" {
        return default_engine(config);
    }
    Route::Probe
}

/// Routes an http(s) URL by its Content-Type. `None` (the probe failed) goes to the default
/// engine, which will report the actual error.
pub fn route_mime(content_type: Option<&str>, config: &Config) -> Route {
    let Some(content_type) = content_type else {
        return default_engine(config);
    };
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let (kind, _) = essence.split_once('/').unwrap_or((&essence, ""));
    let matches = |pattern: &String| {
        *pattern == essence || pattern.strip_suffix("/*").is_some_and(|p| p == kind)
    };
    match config.engines.iter().find(|e| e.mime.iter().any(matches)) {
        Some(engine) => Route::Engine(engine.name.clone()),
        None => Route::Download,
    }
}

fn default_engine(config: &Config) -> Route {
    match config.engine(&config.default_engine) {
        Some(engine) => Route::Engine(engine.name.clone()),
        None => Route::Invalid(format!(
            "default engine {:?} is not configured",
            config.default_engine
        )),
    }
}

/// An HTTP client that names Sighurt and gives up after `timeout`.
pub(crate) fn http_client(timeout: Duration) -> reqwest::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .user_agent(concat!("Sighurt/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Asks the server for `url`'s Content-Type. Blocking; run it off the UI thread.
pub fn content_type(url: &str) -> Option<String> {
    let client = http_client(Duration::from_secs(10)).ok()?;
    let header = |response: reqwest::blocking::Response| {
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    match client.head(url).send() {
        Ok(response) if response.status().is_success() => header(response),
        // Some servers refuse HEAD. A GET that is dropped after the headers costs little.
        _ => client.get(url).send().ok().and_then(header),
    }
}

/// A `file://` URL for `path`, which is made absolute first.
pub fn file_url(path: &std::path::Path) -> String {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    url::Url::from_file_path(&path)
        .map(String::from)
        .unwrap_or_else(|_| format!("file://{}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::parse(Some(
            r#"
            [engines.gemini]
            command = ["sig-gemini"]
            schemes = ["gemini"]
            mime = ["text/gemini"]
            "#,
        ))
        .unwrap()
    }

    fn engine(name: &str) -> Route {
        Route::Engine(name.into())
    }

    #[test]
    fn routes_without_io() {
        let c = config();
        assert_eq!(route("sig://home", &c), Route::Ui);
        assert_eq!(route("SIG://Settings/", &c), Route::Ui);
        assert_eq!(route("sig:nope", &c), Route::Ui);
        assert_eq!(route("gemini://example.org/", &c), engine("gemini"));
        assert_eq!(route("data:text/html,hi", &c), engine("servo"));
        assert_eq!(route("https://a.b/app.wasm", &c), engine("wasm"));
        assert_eq!(route("https://a.b/APP.WASM?x=1#y", &c), engine("wasm"));
        assert_eq!(route("file:///tmp/app.wasm", &c), engine("wasm"));
        assert_eq!(route("file:///tmp/page.html", &c), engine("servo"));
        assert_eq!(route("file:///tmp/dir/", &c), engine("servo"));
        assert_eq!(route("https://example.com", &c), Route::Probe);
        assert_eq!(route("https://example.com/archive.zip", &c), Route::Probe);
        assert!(matches!(route("ftp://example.com", &c), Route::Invalid(_)));
        assert!(matches!(route("not a url", &c), Route::Invalid(_)));
    }

    #[test]
    fn routes_by_content_type() {
        let c = config();
        assert_eq!(
            route_mime(Some("text/html; charset=utf-8"), &c),
            engine("servo")
        );
        assert_eq!(route_mime(Some("application/wasm"), &c), engine("wasm"));
        assert_eq!(route_mime(Some("IMAGE/PNG"), &c), engine("servo"));
        assert_eq!(route_mime(Some("text/gemini"), &c), engine("gemini"));
        assert_eq!(route_mime(Some("application/zip"), &c), Route::Download);
        assert_eq!(route_mime(None, &c), engine("servo"));
    }

    #[test]
    fn missing_default_engine_is_reported() {
        let mut c = config();
        c.default_engine = "nope".into();
        assert!(matches!(route("file:///tmp/x", &c), Route::Invalid(_)));
    }
}
