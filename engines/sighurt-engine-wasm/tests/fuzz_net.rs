//! Seeded random-input tests of the Wasm engine's network policy: which hosts a guest may
//! connect to. Whatever form a host is written in (dotted, short, octal, hexadecimal, a single
//! integer, full-width digits, IPv4-mapped IPv6, trailing dots, upper case, zone ids), a loopback,
//! private, link-local, shared, multicast or reserved address is refused, and a public one is
//! accepted. The oracle is a table of the special-purpose blocks written down here, not the
//! policy's own code. `tools/fuzz/mod.rs` describes the environment variables.

#[path = "../../../tools/fuzz/mod.rs"]
mod fuzz;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use fuzz::{iterations, run, Rng};
use sighurt_engine_wasm::net::{is_public, NetPolicy, HTTP, WEBSOCKET};

const LIMIT: Duration = Duration::from_secs(2);

/// IPv4 blocks a guest must never reach: (address, prefix length).
const REFUSED_V4: &[([u8; 4], u32)] = &[
    ([0, 0, 0, 0], 8),
    ([10, 0, 0, 0], 8),
    ([100, 64, 0, 0], 10),
    ([127, 0, 0, 0], 8),
    ([169, 254, 0, 0], 16),
    ([172, 16, 0, 0], 12),
    ([192, 0, 0, 0], 24),
    ([192, 0, 2, 0], 24),
    ([192, 168, 0, 0], 16),
    ([198, 18, 0, 0], 15),
    ([198, 51, 100, 0], 24),
    ([203, 0, 113, 0], 24),
    ([224, 0, 0, 0], 4),
    ([240, 0, 0, 0], 4),
];

fn refused_v4(ip: Ipv4Addr) -> bool {
    let n = u32::from(ip);
    REFUSED_V4.iter().any(|&(base, bits)| {
        let mask = if bits == 0 {
            0
        } else {
            u32::MAX << (32 - bits)
        };
        n & mask == u32::from(Ipv4Addr::from(base)) & mask
    })
}

/// IPv4 addresses worth landing on: the edges of every refused block and of the public space
/// beside them.
fn v4(rng: &mut Rng) -> Ipv4Addr {
    match rng.below(4) {
        0 => Ipv4Addr::from(rng.next_u64() as u32),
        1 => {
            let &(base, bits) = rng.pick(REFUSED_V4);
            let base = u32::from(Ipv4Addr::from(base));
            let size = 1u64 << (32 - bits);
            let edge = *rng.pick(&[0u64, 1, size - 1, size, u64::MAX]);
            let offset = if edge == u64::MAX {
                rng.next_u64() % size
            } else {
                edge
            };
            Ipv4Addr::from((u64::from(base) + offset) as u32)
        }
        2 => Ipv4Addr::new(
            *rng.pick(&[
                8, 1, 9, 11, 100, 126, 128, 169, 172, 191, 192, 198, 203, 223, 255,
            ]),
            rng.byte(),
            rng.byte(),
            rng.byte(),
        ),
        _ => Ipv4Addr::new(
            *rng.pick(&[127, 10, 0, 169, 192, 172, 100]),
            *rng.pick(&[0, 1, 15, 16, 31, 32, 64, 127, 128, 168, 169, 254, 255]),
            rng.byte(),
            rng.byte(),
        ),
    }
}

/// `ip` written the many ways a URL's host may spell one.
fn spelled(rng: &mut Rng, ip: Ipv4Addr) -> String {
    let [a, b, c, d] = ip.octets();
    let n = u32::from(ip);
    match rng.below(9) {
        0 => ip.to_string(),
        1 => format!("{a}.{b}.{}", u16::from(c) << 8 | u16::from(d)),
        2 => format!(
            "{a}.{}",
            u32::from(b) << 16 | u32::from(c) << 8 | u32::from(d)
        ),
        3 => n.to_string(),
        4 => format!("0x{n:x}"),
        5 => format!("0{n:o}"),
        6 => format!("0x{a:x}.0x{b:X}.0x{c:x}.{d}"),
        7 => format!("0{a:o}.0{b:o}.{c}.0{d:o}"),
        _ => ip
            .to_string()
            .chars()
            .map(|ch| {
                if ch.is_ascii_digit() {
                    char::from_u32(0xFF10 + ch.to_digit(10).unwrap()).unwrap()
                } else if ch == '.' {
                    '\u{3002}'
                } else {
                    ch
                }
            })
            .collect(),
    }
}

fn host_url(rng: &mut Rng, host: &str) -> String {
    let scheme = rng.pick(&["http", "https", "ws", "wss", "HTTP", "ftp", "file"]);
    let auth = rng.pick(&["", "user@", "user:pw@", "a:b@", "%41@"]);
    let port = rng.pick(&["", ":80", ":443", ":8080", ":0", ":65535"]);
    let path = rng.pick(&["", "/", "/x?y#z", "/../..", "?q"]);
    format!("{scheme}://{auth}{host}{port}{path}")
}

fn assert_policy(url: &str, expect_refused: Option<bool>, note: &str) {
    let strict = NetPolicy::default();
    let schemes: &[&str] = if url.starts_with("ws") || url.starts_with("WS") {
        WEBSOCKET
    } else {
        HTTP
    };
    let result = strict.check(url, schemes);
    match (expect_refused, &result) {
        (Some(true), Ok(_)) => panic!("{note}: {url} was allowed"),
        (Some(false), Err(e)) => {
            // A well-formed public host may be refused only for its scheme.
            let parsed = ::url::Url::parse(url);
            let scheme_ok = parsed.as_ref().is_ok_and(|u| schemes.contains(&u.scheme()));
            assert!(!scheme_ok, "{note}: {url} was refused: {e}");
        }
        _ => {}
    }
    // Whatever the spelling, an allowed URL has a domain or a public address.
    if let Ok(parsed) = result {
        match parsed.host() {
            Some(::url::Host::Domain(_)) => {}
            Some(::url::Host::Ipv4(ip)) => assert!(
                !refused_v4(ip) && is_public(IpAddr::V4(ip)),
                "{url} -> {ip}"
            ),
            Some(::url::Host::Ipv6(ip)) => assert!(is_public(IpAddr::V6(ip)), "{url} -> {ip}"),
            None => panic!("{url} has no host and was allowed"),
        }
    }
}

#[test]
fn fuzz_ipv4_any_spelling() {
    run("fuzz_net_v4", iterations(100) * 3, LIMIT, |case| {
        let ip = v4(&mut case.rng);
        let host = spelled(&mut case.rng, ip);
        let url = host_url(&mut case.rng, &host);
        case.note(|| format!("{ip} as {url}"));
        assert_eq!(is_public(IpAddr::V4(ip)), !refused_v4(ip), "{ip}");
        // The URL parser may refuse a spelling; if it reads one, it reads the same address.
        if let Ok(parsed) = ::url::Url::parse(&url) {
            if let Some(::url::Host::Ipv4(read)) = parsed.host() {
                if matches!(parsed.scheme(), "http" | "https" | "ws" | "wss") {
                    assert_eq!(read, ip, "{url} read as {read}");
                    assert_policy(&url, Some(refused_v4(ip)), "ipv4");
                    return;
                }
            }
        }
        assert_policy(&url, None, "ipv4");
    });
}

/// The IPv6 addresses that stand for an IPv4 one, and the blocks of their own.
fn v6(rng: &mut Rng) -> (Ipv6Addr, Option<Ipv4Addr>, bool) {
    let inner = v4(rng);
    let [a, b, c, d] = inner.octets();
    let (hi, lo) = (
        u16::from(a) << 8 | u16::from(b),
        u16::from(c) << 8 | u16::from(d),
    );
    // (address, embedded IPv4 if it stands for one, refused for its own block)
    match rng.below(14) {
        0 => (
            Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, hi, lo),
            Some(inner),
            false,
        ),
        1 => (Ipv6Addr::new(0, 0, 0, 0, 0, 0, hi, lo), Some(inner), false),
        2 => (
            Ipv6Addr::new(0x64, 0xff9b, 0, 0, 0, 0, hi, lo),
            Some(inner),
            false,
        ),
        3 => (
            Ipv6Addr::new(0x2002, hi, lo, rng.below(65536) as u16, 0, 0, 0, 1),
            Some(inner),
            false,
        ),
        4 => (
            Ipv6Addr::new(0, 0, 0, 0, 0xffff, 0, hi, lo),
            Some(inner),
            false,
        ),
        5 => (
            Ipv6Addr::new(0x64, 0xff9b, 1, rng.below(65536) as u16, 0, 0, hi, lo),
            None,
            true,
        ),
        6 => (
            Ipv6Addr::new(
                0xfc00 | rng.below(0x200) as u16,
                rng.below(65536) as u16,
                0,
                0,
                0,
                0,
                0,
                1,
            ),
            None,
            true,
        ),
        7 => (
            Ipv6Addr::new(
                0xfe80 | rng.below(0x40) as u16,
                0,
                0,
                0,
                0,
                0,
                0,
                rng.below(65536) as u16,
            ),
            None,
            true,
        ),
        8 => (
            Ipv6Addr::new(0xfec0 | rng.below(0x40) as u16, 0, 0, 0, 0, 0, 0, 1),
            None,
            true,
        ),
        9 => (
            Ipv6Addr::new(
                0xff00 | rng.below(0x100) as u16,
                rng.below(65536) as u16,
                0,
                0,
                0,
                0,
                0,
                1,
            ),
            None,
            true,
        ),
        10 => (
            Ipv6Addr::new(0x2001, 0xdb8, rng.below(65536) as u16, 0, 0, 0, 0, 1),
            None,
            true,
        ),
        11 => (
            *rng.pick(&[Ipv6Addr::UNSPECIFIED, Ipv6Addr::LOCALHOST]),
            None,
            true,
        ),
        12 => (
            Ipv6Addr::new(
                0x2606,
                0x4700,
                rng.below(65536) as u16,
                0,
                0,
                0,
                0,
                rng.below(65536) as u16,
            ),
            None,
            false,
        ),
        _ => (
            Ipv6Addr::new(
                0x2a00 | rng.below(0x100) as u16,
                rng.below(65536) as u16,
                0,
                0,
                0,
                0,
                0,
                1,
            ),
            None,
            false,
        ),
    }
}

#[test]
fn fuzz_ipv6_and_embedded_ipv4() {
    run("fuzz_net_v6", iterations(100) * 3, LIMIT, |case| {
        let (ip, embedded, own_block) = v6(&mut case.rng);
        case.note(|| format!("{ip} embeds {embedded:?}"));
        let refused = own_block || embedded.is_some_and(refused_v4);
        assert_eq!(is_public(IpAddr::V6(ip)), !refused, "{ip}");
        // As a URL host, in brackets, in any case, with leading zeros.
        let host = match case.rng.below(3) {
            0 => format!("[{ip}]"),
            1 => format!("[{}]", ip.to_string().to_uppercase()),
            _ => {
                let groups: Vec<String> =
                    ip.segments().iter().map(|s| format!("{s:04x}")).collect();
                format!("[{}]", groups.join(":"))
            }
        };
        let url = host_url(&mut case.rng, &host);
        case.note(|| format!("{ip} as {url}"));
        if let Ok(parsed) = ::url::Url::parse(&url) {
            if let (Some(::url::Host::Ipv6(read)), true) = (
                parsed.host(),
                matches!(parsed.scheme(), "http" | "https" | "ws" | "wss"),
            ) {
                assert_eq!(read, ip);
                assert_policy(&url, Some(refused), "ipv6");
                return;
            }
        }
        assert_policy(&url, None, "ipv6");
    });
}

/// Any text as a URL, and hosts made of dots, zone ids, trailing dots and overlong labels: the
/// policy answers without a panic or a stall, and only ever allows what it should.
#[test]
fn fuzz_hosts_of_every_shape() {
    run("fuzz_net_hosts", iterations(100) * 2, LIMIT, |case| {
        let pieces = [
            "localhost",
            "LOCALHOST.",
            "127.0.0.1.",
            "127.0.0.1",
            "[::1]",
            "[fe80::1%25eth0]",
            "[fe80::1%eth0]",
            "[::ffff:127.0.0.1]",
            "[::ffff:7f00:1]",
            "a.b",
            ".",
            "..",
            "-",
            "_",
            "%31%32%37.0.0.1",
            "ex%61mple.com",
            "xn--",
            "例え.jp",
            "0",
            "0.0.0.0",
            "00",
            "1e3",
            "0x",
            "0x7f",
            "256.0.0.1",
            "1.2.3.4.5",
            "1..2",
            "[",
            "]",
            "@",
            ":",
            "\\",
            "%00",
            " ",
            "\t",
            "\n",
        ];
        let host = match case.rng.below(4) {
            0 => case.rng.string_from(&pieces, 4),
            1 => format!(
                "{}.{}",
                "a".repeat(case.rng.range(1, 300)),
                case.rng.pick(&["com", "", "1", "0x1"])
            ),
            2 => "a.".repeat(case.rng.range(1, 150)),
            _ => case.rng.string(12),
        };
        let mut url = host_url(&mut case.rng, &host);
        if case.rng.one_in(6) {
            url = case.rng.mutate(&url);
        }
        case.note(|| url.clone());
        assert_policy(&url, None, "hosts");
        let _ = NetPolicy::for_page(&url, None);
        let _ = NetPolicy::for_page(&url, Some(IpAddr::V4(v4(&mut case.rng))));
    });
}

#[test]
fn fuzz_page_policies() {
    run("fuzz_net_page", iterations(100), LIMIT, |case| {
        let page = *case.rng.pick(&[
            "https://example.com/a.wasm",
            "http://10.0.0.5/x",
            "http://localhost:8080/",
            "file:///tmp/a.wasm",
            "data:text/html,x",
            "javascript:1",
            "about:blank",
            "",
            "not a url",
            "ftp://example.com/",
        ]);
        let served = case.rng.one_in(2).then(|| IpAddr::V4(v4(&mut case.rng)));
        case.note(|| format!("{page} served from {served:?}"));
        let policy = NetPolicy::for_page(page, served);
        match page.split(':').next().unwrap() {
            "file" => assert!(policy.allow_private() && policy.origin() == Some("null")),
            "https" | "http" => assert_eq!(
                policy.allow_private(),
                served.is_some_and(|ip| !is_public(ip))
            ),
            _ => assert!(!policy.allow_private(), "{page}"),
        }
        // A public guest page never reaches a private address.
        if !policy.allow_private() {
            let ip = v4(&mut case.rng);
            assert_eq!(policy.allows(IpAddr::V4(ip)), !refused_v4(ip));
        }
    });
}

/// Found by `fuzz_ipv6_and_embedded_ipv4`: the local-use translation prefix `64:ff9b:1::/48`
/// (RFC 8215) was public, and so was the IPv4-translated `::ffff:0:a.b.c.d` (RFC 2765) for any
/// IPv4 address, loopback and private ones included.
#[test]
fn translation_prefixes_do_not_open_a_way_to_private_addresses() {
    for refused in [
        "64:ff9b:1::1",
        "64:ff9b:1:e6e4::834d",
        "64:ff9b:1:ffff:ffff:ffff:ffff:ffff",
        "::ffff:0:127.0.0.1",
        "::ffff:0:10.1.2.3",
        "::ffff:0:7f00:1",
        "::ffff:0:169.254.169.254",
    ] {
        let ip: IpAddr = refused.parse().unwrap();
        assert!(!is_public(ip), "{refused}");
        let url = format!("http://[{refused}]/");
        assert!(NetPolicy::default().check(&url, HTTP).is_err(), "{url}");
    }
    for public in ["::ffff:0:8.8.8.8", "64:ff9b::808:808", "64:ff9a:1::1"] {
        assert!(is_public(public.parse().unwrap()), "{public}");
    }
}
