//! WHATWG URL Standard compliant URL parsing for WASM apps.
//!
//! Wraps the `url` crate (which implements the WHATWG URL spec) and accepts the
//! `http`, `https` and `file` schemes plus `sig://` for the shell's internal pages
//! (so guests can link to them).

use std::fmt;

use url::Url;

const SUPPORTED_SCHEMES: &[&str] = &["http", "https", "file", "sig"];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AppUrl {
    inner: Url,
}

#[derive(Debug)]
pub enum UrlError {
    Parse(String),
    UnsupportedScheme(String),
    Empty,
    RelativeRequiresBase,
}

impl fmt::Display for UrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UrlError::Parse(msg) => write!(f, "URL parse error: {msg}"),
            UrlError::UnsupportedScheme(s) => write!(f, "unsupported URL scheme: {s}"),
            UrlError::Empty => write!(f, "empty URL"),
            UrlError::RelativeRequiresBase => {
                write!(f, "relative URL cannot be parsed without a base URL")
            }
        }
    }
}

impl std::error::Error for UrlError {}

impl AppUrl {
    /// Parse a user-supplied URL string.
    ///
    /// Bare hostnames like `example.com/path` are assumed HTTPS.
    /// Relative paths (starting with `/` or `.`) are rejected — use
    /// [`AppUrl::join`] to resolve them against a base URL.
    pub fn parse(input: &str) -> Result<Self, UrlError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(UrlError::Empty);
        }

        if (trimmed.starts_with('/') || trimmed.starts_with('.')) && !trimmed.starts_with("//") {
            return Err(UrlError::RelativeRequiresBase);
        }

        let normalized = if trimmed.contains("://") || trimmed.starts_with("//") {
            trimmed.to_string()
        } else {
            format!("https://{trimmed}")
        };

        let inner = Url::parse(&normalized).map_err(|e| UrlError::Parse(e.to_string()))?;

        if !SUPPORTED_SCHEMES.contains(&inner.scheme()) {
            return Err(UrlError::UnsupportedScheme(inner.scheme().to_string()));
        }

        Ok(Self { inner })
    }

    /// Resolve a possibly-relative reference against this URL as the base.
    pub fn join(&self, reference: &str) -> Result<Self, UrlError> {
        let inner = self
            .inner
            .join(reference)
            .map_err(|e| UrlError::Parse(e.to_string()))?;

        if !SUPPORTED_SCHEMES.contains(&inner.scheme()) {
            return Err(UrlError::UnsupportedScheme(inner.scheme().to_string()));
        }

        Ok(Self { inner })
    }

    pub fn scheme(&self) -> &str {
        self.inner.scheme()
    }

    pub fn as_str(&self) -> &str {
        self.inner.as_str()
    }

    /// True for http/https URLs that can be fetched over the network.
    pub fn is_fetchable(&self) -> bool {
        matches!(self.scheme(), "http" | "https")
    }

    /// True for `file://` URLs.
    pub fn is_local_file(&self) -> bool {
        self.scheme() == "file"
    }

    /// True for `sig://` internal browser pages.
    pub fn is_internal(&self) -> bool {
        self.scheme() == "sig"
    }

    /// Extract the local filesystem path from a `file://` URL.
    pub fn to_file_path(&self) -> Option<std::path::PathBuf> {
        self.inner.to_file_path().ok()
    }

    /// Scheme + host + port serialized as a string (for same-origin checks).
    pub fn origin_str(&self) -> String {
        match self.inner.origin() {
            url::Origin::Opaque(_) => self.scheme().to_string(),
            url::Origin::Tuple(scheme, host, port) => {
                format!("{scheme}://{host}:{port}")
            }
        }
    }

    /// Stable app identity used to scope permissions and storage.
    ///
    /// - `http`/`https`: scheme + host + port (path changes, e.g. via `push_state`, don't
    ///   change the origin).
    /// - `file`: the containing directory, so different local apps don't share state while
    ///   an app and its sibling assets do.
    /// - Other schemes fall back to [`AppUrl::origin_str`].
    pub fn app_origin(&self) -> String {
        if self.is_local_file() {
            let path = self.inner.path();
            let dir = match path.rfind('/') {
                Some(0) => "/",
                Some(i) => &path[..i],
                None => path,
            };
            format!("file://{dir}")
        } else {
            self.origin_str()
        }
    }
}

/// [`AppUrl::app_origin`] for a raw URL string; falls back to the input when unparseable.
pub fn app_origin_of(url: &str) -> String {
    match AppUrl::parse(url) {
        Ok(parsed) => parsed.app_origin(),
        Err(_) => url.to_string(),
    }
}

impl fmt::Display for AppUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.inner)
    }
}

/// Percent-encode a string (useful for building URL path/query components).
pub fn percent_encode(input: &str) -> String {
    percent_encoding::utf8_percent_encode(input, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// Decode a percent-encoded string.
pub fn percent_decode(input: &str) -> String {
    percent_encoding::percent_decode_str(input)
        .decode_utf8_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_https() {
        let url = AppUrl::parse(" https://example.com/app.wasm?v=1#top ").unwrap();
        assert_eq!(url.scheme(), "https");
        assert!(url.is_fetchable());
        assert_eq!(url.as_str(), "https://example.com/app.wasm?v=1#top");
    }

    #[test]
    fn bare_hostname_becomes_https() {
        let url = AppUrl::parse("example.com/app.wasm").unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.as_str(), "https://example.com/app.wasm");
    }

    #[test]
    fn resolve_relative() {
        let base = AppUrl::parse("https://example.com/apps/v1/main.wasm").unwrap();
        let resolved = base.join("../v2/new.wasm").unwrap();
        assert_eq!(resolved.as_str(), "https://example.com/apps/v2/new.wasm");
    }

    #[test]
    fn file_url() {
        let url = AppUrl::parse("file:///tmp/app.wasm").unwrap();
        assert!(url.is_local_file());
        assert!(!url.is_fetchable());
    }

    #[test]
    fn sig_internal() {
        let url = AppUrl::parse("sig://home").unwrap();
        assert!(url.is_internal());
    }

    #[test]
    fn unsupported_scheme() {
        assert!(AppUrl::parse("ftp://example.com").is_err());
    }

    #[test]
    fn percent_encoding_roundtrip() {
        let original = "hello world";
        let encoded = percent_encode(original);
        let decoded = percent_decode(&encoded);
        assert_eq!(decoded, original);
    }

    #[test]
    fn app_origin_https_ignores_path() {
        let a = AppUrl::parse("https://example.com/apps/a.wasm").unwrap();
        let b = AppUrl::parse("https://example.com/other/b.wasm").unwrap();
        assert_eq!(a.app_origin(), "https://example.com:443");
        assert_eq!(a.app_origin(), b.app_origin());
    }

    #[test]
    fn app_origin_file_is_containing_directory() {
        let a = AppUrl::parse("file:///tmp/apps/a.wasm").unwrap();
        let b = AppUrl::parse("file:///tmp/other/b.wasm").unwrap();
        assert_eq!(a.app_origin(), "file:///tmp/apps");
        assert_ne!(a.app_origin(), b.app_origin());
    }

    #[test]
    fn app_origin_of_falls_back_to_input() {
        assert_eq!(app_origin_of(""), "");
        assert_eq!(app_origin_of("./relative.wasm"), "./relative.wasm");
    }

    #[test]
    fn relative_path_rejected_without_base() {
        assert!(matches!(
            AppUrl::parse("../other.wasm"),
            Err(UrlError::RelativeRequiresBase)
        ));
    }
}
