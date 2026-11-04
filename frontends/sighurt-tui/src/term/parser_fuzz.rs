//! Random-input tests of the terminal parser: bytes the terminal sends, valid, truncated and
//! garbage. A child of `parser`, so it sees the parser's state. Seeded; see
//! `tools/fuzz/mod.rs`.
//!
//! Held for every stream: no panic; the same events however the bytes are cut into reads
//! (`feed` is a state machine and keeps no time); every buffer within its bound; and after the
//! timers have fired (and a paste ended) the parser is in the ground state, so no byte
//! stays unconsumed and no input is lost for good.

#[path = "../../../../tools/fuzz/mod.rs"]
mod fuzz;

use std::time::Duration;

use fuzz::{iterations, run, Rng};

use super::{
    Buf, Csi, Dcs, Input, Parser, State, MAX_HELD, MAX_PARAMS, MAX_PASTE, MAX_PAYLOAD, MAX_START,
    MAX_SUBS, PASTE_END,
};
use crate::term::probe::{absorb, Caps};

const PIECES: &[&[u8]] = &[
    b"\x1b",
    b"\x1b[",
    b"\x1bO",
    b"\x1b]",
    b"\x1bP",
    b"\x1b_",
    b"\x1b\\",
    b"\x1b\x1b",
    b"\x07",
    b"\x18",
    b"\x1a",
    b"\x00",
    b"\x7f",
    b"\r",
    b"\t",
    b"\x08",
    b"\x9b",
    b"\x90",
    b"\x9d",
    b"[",
    b"]",
    b"O",
    b"P",
    b"_",
    b"\\",
    b";",
    b":",
    b"?",
    b">",
    b"<",
    b"=",
    b"M",
    b"m",
    b"~",
    b"u",
    b"A",
    b"Z",
    b"I",
    b"t",
    b"c",
    b"G",
    b"r",
    b"+",
    b"0",
    b"1",
    b"2",
    b"5",
    b"9",
    b"200",
    b"201",
    b" ",
    b"a",
    b"Z",
    "é".as_bytes(),
    "日".as_bytes(),
    "😀".as_bytes(),
    b"\xc3",
    b"\xe6\x97",
    b"\xf0\x9f\x98",
    b"\xff",
    b"\xc0\x80",
    b"\xed\xa0\x80",
    b"\xf4\x90\x80\x80",
];

/// Whole, valid sequences a terminal may send.
const SEQUENCES: &[&[u8]] = &[
    b"\x1b[A",
    b"\x1b[1;5C",
    b"\x1b[3~",
    b"\x1b[15;2~",
    b"\x1bOP",
    b"\x1bOA",
    b"\x1b[H",
    b"\x1b[97;5u",
    b"\x1b[97:65;2:3;65u",
    b"\x1b[57441;1u",
    b"\x1b[1;1:3D",
    b"\x1b[127;;8u",
    b"\x1b[?31u",
    b"\x1b[?0u",
    b"\x1b[?1;2c",
    b"\x1b[?62;4;22c",
    b"\x1b[I",
    b"\x1b[O",
    b"\x1b[<0;10;5M",
    b"\x1b[<0;10;5m",
    b"\x1b[<64;1;1M",
    b"\x1b[<35;300;200M",
    b"\x1b[<128;1;1M",
    b"\x1b[200~",
    b"\x1b[201~",
    b"\x1b[4;600;800t",
    b"\x1b[6;16;8t",
    b"\x1b]10;rgb:ffff/ffff/ffff\x07",
    b"\x1b]11;rgb:0000/0000/0000\x1b\\",
    b"\x1b]52;c;aGVsbG8=\x07",
    b"\x1b]52;c;?\x1b\\",
    b"\x1b]52;p;!!!\x07",
    b"\x1b]52;;\x07",
    b"\x1b_Gi=31;OK\x1b\\",
    b"\x1b_Gi=31;ENOENT:nope\x1b\\",
    b"\x1bP1+r5443=787465726d\x1b\\",
    b"\x1bP0+r\x1b\\",
    b"\x1bP>|kitty(0.42)\x1b\\",
    b"\x1b[?u",
    b"\x1b[>1u",
    b"\x1b[=5;1u",
    b"\x1b[1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16;17;18;19;20;21;22;23;24m",
    b"\x1b[1:2:3:4:5:6:7:8:9:10:11:12:13m",
    b"\x1b[99999999999999999999;1u",
    b"hello",
    b"\x1b[200~pasted text\x1b[201~",
    b"\x1b[200~\x1b[201~",
    b"\x03",
    b"\x0c",
    b"\x1b[Z",
];

fn stream(rng: &mut Rng) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..rng.range(1, 14) {
        match rng.below(6) {
            0 => out.extend_from_slice(rng.pick(PIECES)),
            1 => {
                let n = rng.range(1, 12);
                out.extend(rng.bytes(n));
            }
            2 => out.extend_from_slice(rng.pick(PIECES)),
            _ => out.extend_from_slice(rng.pick(SEQUENCES)),
        }
    }
    match rng.below(5) {
        0 => rng.mutate_bytes(&out),
        1 => {
            let cut = rng.below(out.len() + 1);
            out.truncate(cut);
            out
        }
        _ => out,
    }
}

fn csi_size(csi: &Csi) -> usize {
    assert!(
        csi.params.len() <= MAX_PARAMS,
        "{} parameters",
        csi.params.len()
    );
    assert!(
        csi.params.iter().all(|p| p.len() <= MAX_SUBS),
        "sub-parameters"
    );
    assert!(csi.intermediates.len() <= 2);
    csi.params.iter().map(Vec::len).sum::<usize>() + csi.intermediates.len()
}

fn buf_size(buf: &Buf) -> usize {
    assert!(
        buf.data.len() <= MAX_PAYLOAD,
        "payload {} bytes",
        buf.data.len()
    );
    buf.data.len()
}

fn dcs_size(dcs: &Dcs) -> usize {
    assert!(
        dcs.header.len() <= MAX_START + 1,
        "DCS header {} bytes",
        dcs.header.len()
    );
    csi_size(&dcs.csi) + dcs.header.len() + buf_size(&dcs.data)
}

/// The bytes the parser holds back, asserting each buffer is within its bound.
fn pending(parser: &Parser) -> usize {
    assert!(
        parser.held.len() <= MAX_HELD,
        "{} keys held",
        parser.held.len()
    );
    match &parser.state {
        State::Ground | State::Esc { .. } | State::Ss3 | State::Utf8 { .. } => 0,
        State::Discard { .. } | State::DiscardEsc { .. } => 0,
        State::Csi(csi) => csi_size(csi),
        State::Osc(buf) | State::OscEsc(buf) | State::Apc(buf) | State::ApcEsc(buf) => {
            buf_size(buf)
        }
        State::Dcs(dcs) | State::DcsEsc(dcs) => dcs_size(dcs),
        State::Paste { data, matched } => {
            assert!(data.len() <= MAX_PASTE, "paste {} bytes", data.len());
            assert!(*matched < PASTE_END.len());
            data.len()
        }
    }
}

fn check_output(inputs: &[Input], bytes: usize) {
    // Every byte yields at most a press and a release, and a held-back sequence at most its
    // own bytes as keys when it turns out to have been typed.
    assert!(
        inputs.len() <= 4 * bytes + 4,
        "{} events from {bytes} bytes",
        inputs.len()
    );
    for input in inputs {
        match input {
            Input::Paste(text) => assert!(text.len() <= 3 * MAX_PASTE),
            Input::Reply(reply) => {
                assert!(format!("{reply:?}").len() <= 24 * MAX_PAYLOAD, "huge reply")
            }
            _ => {}
        }
    }
}

/// Fires the timers until the parser is back in the ground state, ending a paste first.
fn settle(parser: &mut Parser) -> Vec<Input> {
    let mut out = Vec::new();
    if matches!(parser.state, State::Paste { .. }) {
        out.extend(parser.feed(PASTE_END));
    }
    for _ in 0..4 {
        out.extend(parser.expire());
    }
    assert!(
        matches!(parser.state, State::Ground),
        "the timers leave the parser in {:?}",
        std::mem::discriminant(&parser.state)
    );
    assert!(parser.poll_timeout().is_none(), "a timer stays armed");
    out
}

/// `bytes` fed in reads cut at `cuts`.
fn feed_cut(bytes: &[u8], cuts: &[usize], kitty_events: bool) -> (Vec<Input>, Parser) {
    let mut parser = Parser::new();
    if kitty_events {
        parser.feed(b"\x1b[?31u");
    }
    let mut out = Vec::new();
    let mut at = 0;
    for &cut in cuts.iter().chain(std::iter::once(&bytes.len())) {
        let cut = cut.clamp(at, bytes.len());
        out.extend(parser.feed(&bytes[at..cut]));
        pending(&parser);
        at = cut;
    }
    (out, parser)
}

#[test]
fn fuzz_parser_streams() {
    run(
        "fuzz_term_parser",
        iterations(100) * 3,
        Duration::from_secs(2),
        |case| {
            let bytes = stream(&mut case.rng);
            let kitty_events = case.rng.one_in(3);
            case.note(|| format!("kitty_events={kitty_events} {bytes:?}"));
            // All at once.
            let (whole, mut parser) = feed_cut(&bytes, &[], kitty_events);
            check_output(&whole, bytes.len());
            // The same bytes, cut into reads at random points, give the same events.
            let mut cuts: Vec<usize> = (0..case.rng.range(1, 6))
                .map(|_| case.rng.below(bytes.len() + 1))
                .collect();
            cuts.sort_unstable();
            let (split, mut split_parser) = feed_cut(&bytes, &cuts, kitty_events);
            assert_eq!(whole, split, "cut at {cuts:?}");
            // Byte by byte.
            let singles: Vec<usize> = (1..bytes.len()).collect();
            let (single, _) = feed_cut(&bytes, &singles, kitty_events);
            assert_eq!(whole, single, "byte by byte");
            // Whatever is held back is released when the timers fire, and the parser is idle.
            let rest = settle(&mut parser);
            let rest_split = settle(&mut split_parser);
            assert_eq!(rest, rest_split);
            check_output(&rest, bytes.len());
            // And it takes input again: a plain key is a key.
            let after = parser.feed(b"x");
            assert!(
                after
                    .iter()
                    .any(|i| matches!(i, Input::Key(k) if k.text.as_deref() == Some("x"))),
                "{after:?}"
            );
        },
    );
}

#[test]
fn fuzz_parser_with_timers_between_reads() {
    run(
        "fuzz_term_parser_timers",
        iterations(100),
        Duration::from_secs(2),
        |case| {
            let mut parser = Parser::new();
            let mut fed = 0;
            for _ in 0..case.rng.range(1, 10) {
                let bytes = stream(&mut case.rng);
                case.note(|| format!("{bytes:?}"));
                fed += bytes.len() + 1;
                check_output(&parser.feed(&bytes), bytes.len());
                pending(&parser);
                if case.rng.one_in(2) {
                    check_output(&parser.expire(), fed);
                    pending(&parser);
                }
                let _ = parser.poll_timeout();
            }
            settle(&mut parser);
        },
    );
}

/// Replies are folded into the capabilities by the probe; nothing in them is unbounded.
#[test]
fn fuzz_probe_absorbs_any_reply() {
    run(
        "fuzz_term_probe",
        iterations(100),
        Duration::from_secs(2),
        |case| {
            let mut parser = Parser::new();
            let mut caps = Caps::default();
            for _ in 0..case.rng.range(1, 6) {
                let bytes = stream(&mut case.rng);
                case.note(|| format!("{bytes:?}"));
                for input in parser.feed(&bytes).iter().chain(&settle(&mut parser)) {
                    absorb(&mut caps, input);
                }
            }
            for text in [&caps.osc10, &caps.osc11].into_iter().flatten() {
                assert!(text.len() <= 3 * MAX_PAYLOAD);
            }
            if let Some(xt) = &caps.xtgettcap {
                assert!(xt.entries.len() <= MAX_PAYLOAD);
            }
        },
    );
}

/// A megabyte of garbage, and megabytes of the shapes that make a parser hold on: no panic, a
/// bounded buffer, and no time to speak of.
#[test]
fn a_megabyte_of_garbage_is_consumed_in_bounded_memory() {
    let mut rng = Rng::new(0x6172_6261_6765);
    let megabyte = rng.bytes(1 << 20);
    let mut shapes: Vec<(&str, Vec<u8>)> = vec![("random", megabyte)];
    let long = |head: &[u8], filler: u8, tail: &[u8]| -> Vec<u8> {
        let mut v = head.to_vec();
        v.resize(head.len() + (2 << 20), filler);
        v.extend_from_slice(tail);
        v
    };
    shapes.push(("osc 52 never ended", long(b"\x1b]52;c;", b'A', b"")));
    shapes.push(("osc 52 ended", long(b"\x1b]52;c;", b'A', b"\x07x")));
    shapes.push(("osc 11 flood", long(b"\x1b]11;", b'f', b"\x1b\\")));
    shapes.push(("apc flood", long(b"\x1b_Gi=1;", b'z', b"\x1b\\")));
    shapes.push(("dcs flood", long(b"\x1bP1+r", b'7', b"\x1b\\")));
    shapes.push(("csi digits", long(b"\x1b[", b'9', b"u")));
    shapes.push(("csi semicolons", long(b"\x1b[", b';', b"m")));
    shapes.push(("csi colons", long(b"\x1b[1", b':', b"m")));
    shapes.push(("csi intermediates", long(b"\x1b[", b' ', b"q")));
    shapes.push(("paste never ended", long(b"\x1b[200~", b'p', b"")));
    shapes.push(("paste ended", long(b"\x1b[200~", b'p', b"\x1b[201~k")));
    shapes.push(("paste with false ends", {
        let mut v = b"\x1b[200~".to_vec();
        for _ in 0..(1 << 19) {
            v.extend_from_slice(b"\x1b[201");
        }
        v
    }));
    shapes.push(("escapes", long(b"", 0x1b, b"")));
    shapes.push(("alt escapes", long(b"", 0x1b, b"[")));
    shapes.push(("utf8 lead bytes", long(b"", 0xf0, b"")));
    shapes.push((
        "kitty keys",
        long(b"\x1b[?31u", 0, b"")
            .into_iter()
            .chain(
                (0..200_000).flat_map(|i: u32| {
                    format!("\x1b[{};{}:1u", 33 + i % 90, 1 + i % 8).into_bytes()
                }),
            )
            .collect(),
    ));
    for (name, bytes) in shapes {
        let start = std::time::Instant::now();
        let mut parser = Parser::new();
        let mut events = 0;
        let mut high = 0;
        for chunk in bytes.chunks(4096) {
            let out = parser.feed(chunk);
            check_output(&out, chunk.len());
            events += out.len();
            high = high.max(pending(&parser));
        }
        events += settle(&mut parser).len();
        let took = start.elapsed();
        eprintln!(
            "{name}: {} bytes, {events} events, at most {high} bytes held, {took:?}",
            bytes.len()
        );
        assert!(took < Duration::from_secs(10), "{name} took {took:?}");
        assert!(high <= MAX_PASTE);
    }
}
