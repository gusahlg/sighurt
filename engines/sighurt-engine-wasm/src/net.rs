//! The network policy guests run under, and the network resources a runtime shares.
//!
//! Every connection a guest makes goes through [`NetPolicy`]: `api_fetch`, `api_fetch_begin`,
//! WebSocket, Server-Sent Events, RTC signaling, `api_audio_play_url`, `api_video_load_url`, and
//! the modules and workers it loads. The policy fails closed:
//!
//! - only absolute `http`/`https` URLs (`ws`/`wss` for WebSocket), so no `file:`, `data:`,
//!   FFmpeg protocol (`pipe:`, `concat:`, `tcp://`) or anything else a guest names;
//! - no loopback, private, link-local, shared, multicast or reserved destination
//!   ([`is_public`]), unless the page itself is a local file or was served from such an
//!   address. Names are checked where they are resolved for the connection ([`resolve`], the
//!   resolver of the shared HTTP clients), every address a name resolves to, and the
//!   connection is made only to the allowed ones, so a name that resolves elsewhere the second
//!   time (DNS rebinding) is checked again. Address literals are checked before connecting;
//! - at most [`MAX_REDIRECTS`] redirects, each to a URL checked the same way;
//! - an `Origin` header naming the page (`null` for a local file);
//! - no system proxy: a proxy would hide the destination from the check.
//!
//! FFmpeg connects by itself, so a video URL's name is checked before FFmpeg opens it but not
//! again when FFmpeg resolves it, and FFmpeg follows HTTP redirects without asking
//! (`crate::video`).
//!
//! What a document's connections hold that its guest hasn't read is bounded together
//! ([`NetBytes`]): [`MAX_DOCUMENT_BYTES`] for a document and its workers, and
//! [`MAX_RUNTIME_BYTES`] for all documents of a runtime. Past that, connections stop reading
//! until the guest catches up, and RTC messages are dropped.
//!
//! Owns: the policy, the shared Tokio runtime ([`Net`]) and HTTP clients, and the accounting of
//! buffered bytes. Must not own: any subsystem's connections (each module owns its own).

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use url::{Host, Url};

/// Most redirects one request follows. Each is checked like the URL the guest gave.
pub const MAX_REDIRECTS: usize = 5;
/// Longest a connection may take to be established.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Most bytes of received data a document's connections (fetch, WebSocket, SSE, RTC) and those
/// of its workers may hold for its guest together: one document can't make the host buffer
/// more, however many connections it opens.
pub const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;
/// Most bytes of received data all documents of a runtime may hold together.
pub const MAX_RUNTIME_BYTES: usize = 512 * 1024 * 1024;
/// How often a connection waiting for room in the byte budget looks again.
pub const ROOM_POLL: Duration = Duration::from_millis(50);
/// Tokio worker threads of a runtime's network tasks.
const NET_THREADS: usize = 2;

/// The schemes of HTTP requests.
pub const HTTP: &[&str] = &["http", "https"];
/// The schemes of WebSocket connections.
pub const WEBSOCKET: &[&str] = &["ws", "wss"];

/// What a page's guest may connect to, and the `Origin` its requests carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetPolicy {
    /// The `Origin` header: the page's origin, `null` for a local file, or none for a request
    /// the browser makes for the user (loading the page itself).
    origin: Option<String>,
    /// Whether loopback, private and link-local destinations are allowed.
    allow_private: bool,
}

impl Default for NetPolicy {
    /// The strictest policy: public destinations only, as from an opaque origin.
    fn default() -> Self {
        Self {
            origin: Some("null".into()),
            allow_private: false,
        }
    }
}

impl NetPolicy {
    /// The policy of loading a page the user navigated to: any destination, no `Origin`.
    pub fn top_level() -> Self {
        Self {
            origin: None,
            allow_private: true,
        }
    }

    /// The policy of a page whose module was loaded from `page_url`, served from
    /// `served_from` (the address the response came from; `None` for a local file). A local
    /// page, or one served from a local or private address, may reach such addresses too.
    pub fn for_page(page_url: &str, served_from: Option<IpAddr>) -> Self {
        let Ok(url) = Url::parse(page_url) else {
            return Self::default();
        };
        match url.scheme() {
            "file" => Self {
                origin: Some("null".into()),
                allow_private: true,
            },
            "http" | "https" => Self {
                origin: Some(url.origin().ascii_serialization()),
                allow_private: served_from.is_some_and(|ip| !is_public(ip)),
            },
            _ => Self::default(),
        }
    }

    /// The `Origin` header requests carry, if any.
    pub fn origin(&self) -> Option<&str> {
        self.origin.as_deref()
    }

    pub fn allow_private(&self) -> bool {
        self.allow_private
    }

    /// Whether a connection to `ip` is allowed.
    pub fn allows(&self, ip: IpAddr) -> bool {
        self.allow_private || is_public(ip)
    }

    /// `url` parsed, if a guest may connect to it: absolute, with one of `schemes`, and with a
    /// host that is not an address the policy refuses. A host name is checked when it is
    /// resolved.
    pub fn check(&self, url: &str, schemes: &[&str]) -> Result<Url, String> {
        let parsed = Url::parse(url).map_err(|e| format!("not an absolute URL: {e}"))?;
        self.check_url(&parsed, schemes)?;
        Ok(parsed)
    }

    fn check_url(&self, url: &Url, schemes: &[&str]) -> Result<(), String> {
        if !schemes.contains(&url.scheme()) {
            return Err(format!("{}: URLs are not allowed here", url.scheme()));
        }
        let ip = match url.host() {
            None => return Err("the URL has no host".into()),
            Some(Host::Domain(_)) => return Ok(()),
            Some(Host::Ipv4(ip)) => IpAddr::V4(ip),
            Some(Host::Ipv6(ip)) => IpAddr::V6(ip),
        };
        if self.allows(ip) {
            Ok(())
        } else {
            Err(format!("{ip} is a local or private address"))
        }
    }

    /// Checks that every address `url`'s host resolves to is allowed, for a connection this
    /// process doesn't make itself (FFmpeg's). Blocks while it resolves.
    pub fn check_resolved(&self, url: &Url) -> Result<(), String> {
        use std::net::ToSocketAddrs;
        let Some(Host::Domain(host)) = url.host() else {
            return Ok(());
        };
        let port = url.port_or_known_default().unwrap_or(0);
        let addrs: Vec<SocketAddr> = (host, port)
            .to_socket_addrs()
            .map_err(|e| format!("{host}: {e}"))?
            .collect();
        match addrs.iter().find(|addr| !self.allows(addr.ip())) {
            Some(addr) => Err(format!(
                "{host} resolves to {}, a local or private address",
                addr.ip()
            )),
            None if addrs.is_empty() => Err(format!("{host} has no address")),
            None => Ok(()),
        }
    }
}

/// Whether `ip` is a public unicast address: not loopback, unspecified, private (RFC 1918,
/// unique local), link-local, shared (CGNAT), multicast, broadcast, reserved or documentation,
/// and not an IPv6 address that embeds one of those (IPv4-mapped, -compatible, NAT64, 6to4).
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => {
            let s = ip.segments();
            let embedded = Ipv4Addr::from((u32::from(s[6]) << 16) | u32::from(s[7]));
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_v4(v4);
            }
            if s[..6] == [0; 6] {
                // `::`, `::1` and the deprecated IPv4-compatible `::a.b.c.d`.
                return !ip.is_unspecified() && !ip.is_loopback() && public_v4(embedded);
            }
            if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                return public_v4(embedded);
            }
            if s[0] == 0x2002 {
                return public_v4(Ipv4Addr::from((u32::from(s[1]) << 16) | u32::from(s[2])));
            }
            let unique_local = s[0] & 0xfe00 == 0xfc00;
            let link_local = s[0] & 0xffc0 == 0xfe80;
            let site_local = s[0] & 0xffc0 == 0xfec0;
            let documentation = s[0] == 0x2001 && s[1] == 0x0db8;
            !(ip.is_multicast() || unique_local || link_local || site_local || documentation)
        }
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    let shared = a == 100 && (64..128).contains(&b);
    let protocol = a == 192 && b == 0 && c == 0;
    let documentation = (a == 192 && b == 0 && c == 2)
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113);
    let benchmarking = a == 198 && (b == 18 || b == 19);
    !(a == 0
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || a >= 240
        || shared
        || protocol
        || documentation
        || benchmarking)
}

/// Resolves `host`, keeping only the addresses `policy` allows; an error when none is left.
pub async fn resolve(host: &str, port: u16, allow_private: bool) -> io::Result<Vec<SocketAddr>> {
    let policy = NetPolicy {
        origin: None,
        allow_private,
    };
    let allowed: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await?
        .filter(|addr| policy.allows(addr.ip()))
        .collect();
    if allowed.is_empty() {
        let message = format!("{host} has no public address");
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, message));
    }
    Ok(allowed)
}

/// The resolver of the shared HTTP clients: [`resolve`].
struct Guarded {
    allow_private: bool,
}

impl reqwest::dns::Resolve for Guarded {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let allow_private = self.allow_private;
        Box::pin(async move {
            let addrs = resolve(name.as_str(), 0, allow_private).await?;
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Follows at most [`MAX_REDIRECTS`] redirects, each to a URL the policy allows.
fn redirects(allow_private: bool) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        let policy = NetPolicy {
            origin: None,
            allow_private,
        };
        if attempt.previous().len() > MAX_REDIRECTS {
            return attempt.error(format!("more than {MAX_REDIRECTS} redirects"));
        }
        match policy.check_url(attempt.url(), HTTP) {
            Ok(()) => attempt.follow(),
            Err(e) => attempt.error(format!("redirect refused: {e}")),
        }
    })
}

/// The blocking HTTP client of a policy: shared by the whole process (each runs a thread of its
/// own), and made on first use.
pub fn blocking_client(policy: &NetPolicy) -> Result<&'static reqwest::blocking::Client, String> {
    static STRICT: OnceLock<Option<reqwest::blocking::Client>> = OnceLock::new();
    static LOCAL: OnceLock<Option<reqwest::blocking::Client>> = OnceLock::new();
    let allow_private = policy.allow_private;
    let slot = if allow_private { &LOCAL } else { &STRICT };
    slot.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .no_proxy()
            .dns_resolver(Arc::new(Guarded { allow_private }))
            .redirect(redirects(allow_private))
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .ok()
    })
    .as_ref()
    .ok_or_else(|| "cannot start an HTTP client".to_string())
}

/// What the network subsystems of a runtime share, made when a guest first uses the network:
/// one Tokio runtime for every connection of every document, and its HTTP clients. Tasks are
/// the subsystems' to stop; dropping this doesn't wait for those still running (a DNS lookup,
/// say), which would hold up the thread that drops it.
pub struct Net {
    runtime: Option<tokio::runtime::Runtime>,
    strict: reqwest::Client,
    local: reqwest::Client,
}

impl Net {
    pub fn new() -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(NET_THREADS)
            .thread_name("sighurt-wasm-net")
            .enable_all()
            .build()?;
        let client = |allow_private| {
            reqwest::Client::builder()
                .no_proxy()
                .dns_resolver(Arc::new(Guarded { allow_private }))
                .redirect(redirects(allow_private))
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .map_err(io::Error::other)
        };
        Ok(Self {
            runtime: Some(runtime),
            strict: client(false)?,
            local: client(true)?,
        })
    }

    pub fn runtime(&self) -> &tokio::runtime::Runtime {
        self.runtime.as_ref().expect("present until dropped")
    }

    /// The HTTP client of `policy`. Requests still need its `Origin` header.
    pub fn client(&self, policy: &NetPolicy) -> &reqwest::Client {
        if policy.allow_private {
            &self.local
        } else {
            &self.strict
        }
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// A count of buffered bytes against a limit.
#[derive(Debug)]
pub struct Counter {
    used: AtomicUsize,
    limit: usize,
}

impl Counter {
    pub fn new(limit: usize) -> Self {
        Self {
            used: AtomicUsize::new(0),
            limit,
        }
    }

    fn take(&self, n: usize) -> bool {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(n).filter(|&total| total <= self.limit)
            })
            .is_ok()
    }

    fn give(&self, n: usize) {
        self.used.fetch_sub(n, Ordering::AcqRel);
    }

    pub fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }
}

/// The budget of buffered network bytes of a document (with its workers) and of its runtime.
#[derive(Clone, Debug)]
pub struct NetBytes {
    document: Arc<Counter>,
    runtime: Option<Arc<Counter>>,
}

impl NetBytes {
    pub fn new(document: Arc<Counter>, runtime: Option<Arc<Counter>>) -> Self {
        Self { document, runtime }
    }

    /// A budget of its own of `limit` bytes.
    pub fn with_limit(limit: usize) -> Self {
        Self::new(Arc::new(Counter::new(limit)), None)
    }

    /// Takes `n` bytes, if both budgets have room.
    fn take(&self, n: usize) -> bool {
        if !self.document.take(n) {
            return false;
        }
        if self
            .runtime
            .as_ref()
            .is_some_and(|runtime| !runtime.take(n))
        {
            self.document.give(n);
            return false;
        }
        true
    }

    fn give(&self, n: usize) {
        self.document.give(n);
        if let Some(runtime) = &self.runtime {
            runtime.give(n);
        }
    }
}

/// The bytes one queue holds against a [`NetBytes`]; given back when it is dropped.
#[derive(Debug)]
pub struct Held {
    budget: NetBytes,
    held: usize,
}

impl Held {
    pub fn new(budget: NetBytes) -> Self {
        Self { budget, held: 0 }
    }

    /// Counts `n` more bytes, if the budget has room for them; returns whether it did.
    pub fn try_add(&mut self, n: usize) -> bool {
        let added = self.budget.take(n);
        self.held += n * usize::from(added);
        added
    }

    /// Counts `n` bytes fewer: the guest read them.
    pub fn remove(&mut self, n: usize) {
        let n = n.min(self.held);
        self.held -= n;
        self.budget.give(n);
    }

    /// The bytes held.
    pub fn get(&self) -> usize {
        self.held
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.budget.give(self.held);
    }
}

/// A document's network state: its policy, set once its module is loaded, and the budget of
/// what its connections hold. Its workers and the modules it loads share it.
#[derive(Debug)]
pub struct DocNet {
    policy: OnceLock<NetPolicy>,
    bytes: Arc<Counter>,
}

impl Default for DocNet {
    fn default() -> Self {
        Self {
            policy: OnceLock::new(),
            bytes: Arc::new(Counter::new(MAX_DOCUMENT_BYTES)),
        }
    }
}

impl DocNet {
    /// Sets the document's policy; only the first call counts.
    pub fn set_policy(&self, policy: NetPolicy) {
        let _ = self.policy.set(policy);
    }

    /// The document's policy: [`NetPolicy::default`], the strictest, until its module was
    /// loaded.
    pub fn policy(&self) -> NetPolicy {
        self.policy.get().cloned().unwrap_or_default()
    }

    /// The document's byte budget, within `runtime`'s.
    pub fn bytes(&self, runtime: Option<Arc<Counter>>) -> NetBytes {
        NetBytes::new(self.bytes.clone(), runtime)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn only_public_unicast_addresses_are_public() {
        for public in [
            "93.184.215.14",
            "1.1.1.1",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(is_public(ip(public)), "{public}");
        }
        for private in [
            "127.0.0.1",
            "127.1.2.3",
            "0.0.0.0",
            "10.0.0.1",
            "172.16.5.4",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "240.0.0.1",
            "::",
            "::1",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:192.168.0.1",
            "::127.0.0.1",
            "64:ff9b::a00:1",
            "2002:c0a8:101::1",
        ] {
            assert!(!is_public(ip(private)), "{private}");
        }
    }

    #[test]
    fn guests_reach_only_http_urls_of_public_hosts() {
        let policy = NetPolicy::for_page("https://app.test/app.wasm", Some(ip("93.184.215.14")));
        assert_eq!(policy.origin(), Some("https://app.test"));
        assert!(policy.check("https://example.com/x", HTTP).is_ok());
        for refused in [
            "file:///etc/passwd",
            "pipe:0",
            "concat:http://a.test/x|http://b.test/y",
            "tcp://example.com:80",
            "crypto+http://example.com/x",
            "/proc/self/fd/0",
            "data:text/plain,x",
            "http://127.0.0.1:8080/",
            "http://[::1]/",
            "http://0x7f.1/",
            "http://10.1.2.3/",
            "http://169.254.169.254/latest/meta-data",
            "ws://example.com/",
        ] {
            assert!(policy.check(refused, HTTP).is_err(), "{refused}");
        }
        assert!(policy.check("wss://example.com/", WEBSOCKET).is_ok());
    }

    #[test]
    fn local_pages_may_reach_local_hosts() {
        let file = NetPolicy::for_page("file:///home/u/app.wasm", None);
        assert_eq!(file.origin(), Some("null"));
        assert!(file.check("http://127.0.0.1:8080/", HTTP).is_ok());
        let lan = NetPolicy::for_page("http://192.168.1.5/app.wasm", Some(ip("192.168.1.5")));
        assert!(lan.check("http://192.168.1.6/", HTTP).is_ok());
        // A public name whose module came from a public address may not.
        let public = NetPolicy::for_page("http://a.test/app.wasm", Some(ip("1.1.1.1")));
        assert!(public.check("http://192.168.1.6/", HTTP).is_err());
        assert!(NetPolicy::default()
            .check("http://127.0.0.1/", HTTP)
            .is_err());
    }

    #[test]
    fn names_are_checked_when_they_resolve() {
        let net = Net::new().unwrap();
        let refused = net.runtime().block_on(resolve("localhost", 80, false));
        assert_eq!(refused.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        let allowed = net
            .runtime()
            .block_on(resolve("localhost", 80, true))
            .unwrap();
        assert!(allowed.iter().all(|addr| addr.ip().is_loopback()));
        // The shared client refuses a name that resolves to loopback, and redirects to one.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = blocking_client(&NetPolicy::default()).unwrap();
        let err = client
            .get(format!("http://localhost:{port}/"))
            .send()
            .unwrap_err();
        assert!(err.is_connect(), "{err:?}");
    }

    #[test]
    fn redirects_are_bounded() {
        use std::io::{Read, Write};
        // Redirects to itself, forever.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = std::thread::spawn(move || {
            let mut served = 0;
            for socket in listener.incoming().take(MAX_REDIRECTS + 1) {
                let Ok(mut socket) = socket else { break };
                let mut request = [0; 4096];
                let _ = socket.read(&mut request);
                let head = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port}/{served}\r\n\
                     Content-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = socket.write_all(head.as_bytes());
                served += 1;
            }
            served
        });
        let client = blocking_client(&NetPolicy::top_level()).unwrap();
        let err = client
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .unwrap_err();
        assert!(err.is_redirect(), "{err:?}");
        assert_eq!(served.join().unwrap(), MAX_REDIRECTS + 1);
    }

    #[test]
    fn budgets_hold_what_they_allow_and_give_it_back() {
        let runtime = Arc::new(Counter::new(10));
        let a = NetBytes::new(Arc::new(Counter::new(8)), Some(runtime.clone()));
        let b = NetBytes::new(Arc::new(Counter::new(8)), Some(runtime.clone()));
        let mut one = Held::new(a.clone());
        assert!(one.try_add(8));
        assert!(!one.try_add(1), "the document's budget is full");
        let mut two = Held::new(b);
        assert!(two.try_add(2));
        assert!(!two.try_add(1), "the runtime's budget is full");
        one.remove(5);
        assert!(two.try_add(5));
        drop(two);
        assert_eq!(runtime.used(), 3);
        drop(one);
        assert_eq!(runtime.used(), 0);
        assert_eq!(a.document.used(), 0);
    }
}
