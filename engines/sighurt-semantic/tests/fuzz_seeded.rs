//! Seeded random-input tests of the bounded parser of page-provided JSON: structured snapshots
//! mutated byte by byte, and the extremes (100 000 nested arrays, 10 MB strings, lone
//! surrogates, node and option floods). Whatever comes in: no panic, a return within the
//! watchdog's limit, and a snapshot that comes out has passed `check` with the caller's
//! document on it. `tools/fuzz/mod.rs` describes the environment variables.

#[path = "../../../tools/fuzz/mod.rs"]
mod fuzz;

use std::time::{Duration, Instant};

use fuzz::{iterations, run, Rng};
use sighurt_api::ids::DocumentGeneration;
use sighurt_api::semantic::{MAX_DEPTH, MAX_NODES, MAX_OPTIONS, MAX_STRING_BYTES};
use sighurt_semantic::{max_input_bytes, parse, ParseError};

const KINDS: &[&str] = &[
    "document",
    "container",
    "heading",
    "paragraph",
    "text",
    "link",
    "button",
    "textfield",
    "textarea",
    "checkbox",
    "radio",
    "select",
    "image",
    "list",
    "listitem",
    "table",
    "row",
    "cell",
    "pre",
    "code",
    "quote",
    "separator",
    "landmark",
    "marquee",
    "",
];

fn json_string(rng: &mut Rng, max: usize) -> String {
    let mut out = String::from("\"");
    for _ in 0..rng.below(max + 1) {
        match rng.below(14) {
            0 => out.push_str("\\n"),
            1 => out.push_str("\\\""),
            2 => out.push_str("\\\\"),
            3 => out.push_str("\\u00e9"),
            4 => out.push_str("\\ud83d\\ude00"),
            5 => out.push_str("\\ud800"),
            6 => out.push_str("\\udc00"),
            7 => out.push_str("\\u0000"),
            8 => out.push_str("\\/"),
            _ => {
                let c = rng.char();
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                    c => out.push(c),
                }
            }
        }
    }
    out.push('"');
    out
}

/// A valid snapshot document: a tree under node 1, with every kind of field.
fn snapshot_json(rng: &mut Rng) -> String {
    let count = rng.range(1, 40);
    let mut nodes = Vec::new();
    for id in 1..=count {
        // Children are later ids, each node the child of exactly one earlier one.
        let children: Vec<String> = ((id + 1)..=count)
            .filter(|&child| (child - 1) / 3 == id - 1 && child != id)
            .map(|c| c.to_string())
            .collect();
        let mut fields = vec![
            format!("\"id\":{id}"),
            format!(
                "\"kind\":\"{}\"",
                if id == 1 { "document" } else { rng.pick(KINDS) }
            ),
            format!("\"children\":[{}]", children.join(",")),
        ];
        if rng.one_in(2) {
            fields.push(format!("\"text\":{}", json_string(rng, 30)));
        }
        if rng.one_in(4) {
            fields.push(format!("\"label\":{}", json_string(rng, 20)));
        }
        if rng.one_in(4) {
            fields.push(format!("\"href\":{}", json_string(rng, 20)));
        }
        if rng.one_in(4) {
            fields.push(format!("\"value\":{}", json_string(rng, 20)));
        }
        if rng.one_in(6) {
            fields.push(format!("\"level\":{}", rng.below(9)));
        }
        if rng.one_in(6) {
            fields.push(format!(
                "\"landmark\":\"{}\"",
                rng.pick(&["main", "navigation", "search", "nope"])
            ));
        }
        if rng.one_in(8) {
            let options: Vec<String> = (0..rng.below(5)).map(|_| json_string(rng, 8)).collect();
            fields.push(format!("\"options\":[{}]", options.join(",")));
            if !options.is_empty() {
                fields.push("\"selected\":0".into());
            }
        }
        if rng.one_in(8) {
            fields.push(format!("\"checked\":{}", rng.one_in(2)));
        }
        if rng.one_in(10) {
            fields.push("\"password\":true".into());
        }
        if rng.one_in(10) {
            fields.push("\"disabled\":true".into());
        }
        if rng.one_in(10) {
            fields.push(format!(
                "\"unknown{}\":{}",
                rng.below(3),
                "{\"a\":[1,2,{\"b\":null}]}"
            ));
        }
        // Fields in any order.
        for i in (1..fields.len()).rev() {
            let j = rng.below(i + 1);
            fields.swap(i, j);
        }
        nodes.push(format!("{{{}}}", fields.join(",")));
    }
    let focus = if rng.one_in(2) {
        format!(",\"focus\":{}", rng.range(1, count))
    } else {
        String::new()
    };
    format!(
        "{{\"truncated\":{},\"root\":1{focus},\"nodes\":[{}]}}",
        rng.one_in(5),
        nodes.join(",")
    )
}

fn check_result(
    result: Result<sighurt_api::semantic::SemanticSnapshot, ParseError>,
    document: DocumentGeneration,
) {
    if let Ok(snapshot) = result {
        assert_eq!(snapshot.document, document);
        snapshot.check().expect("a parsed snapshot passes check");
        assert!(snapshot.nodes.len() <= MAX_NODES);
        assert!(snapshot
            .nodes
            .iter()
            .all(|n| !(n.password && n.value.is_some())));
        assert!(snapshot
            .nodes
            .iter()
            .all(|n| n.options.len() <= MAX_OPTIONS));
    }
}

#[test]
fn fuzz_snapshots_valid_and_mutated() {
    let mut accepted = 0usize;
    let mut mutated_accepted = 0usize;
    let cases = iterations(100) * 2;
    run("fuzz_semantic", cases, Duration::from_secs(2), |case| {
        let document = DocumentGeneration::next();
        let json = snapshot_json(&mut case.rng);
        case.note(|| json.clone());
        // An unmutated snapshot is accepted, except that a password field with a value is not.
        let result = parse(&json, document);
        if let Err(error) = &result {
            assert!(
                json.contains("\"password\":true"),
                "valid snapshot refused: {error}"
            );
        } else {
            accepted += 1;
        }
        check_result(result, document);
        let mutated = if case.rng.one_in(8) {
            String::from_utf8_lossy(&case.rng.mutate_bytes(json.as_bytes())).into_owned()
        } else {
            case.rng.mutate(&json)
        };
        case.note(|| mutated.clone());
        let result = parse(&mutated, document);
        mutated_accepted += result.is_ok() as usize;
        check_result(result, document);
        // Every prefix of a snapshot is either the whole or an error.
        let mut cut = case.rng.below(json.len() + 1);
        while !json.is_char_boundary(cut) {
            cut -= 1;
        }
        if cut < json.len() {
            case.note(|| json[..cut].to_owned());
            assert!(
                parse(&json[..cut], document).is_err(),
                "a torn snapshot was accepted"
            );
        }
    });
    eprintln!(
        "fuzz_semantic: {accepted} valid accepted, {mutated_accepted} mutated accepted of {cases}"
    );
    assert!(
        cases < 200 || accepted * 2 >= cases,
        "the generator makes valid snapshots"
    );
}

#[test]
fn fuzz_random_bytes_as_text() {
    run(
        "fuzz_semantic_bytes",
        iterations(100),
        Duration::from_secs(2),
        |case| {
            let n = case.rng.below(3000);
            let bytes = case.rng.bytes(n);
            let text = String::from_utf8_lossy(&bytes).into_owned();
            case.note(|| text.clone());
            check_result(
                parse(&text, DocumentGeneration::next()),
                DocumentGeneration::next(),
            );
        },
    );
}

/// Runs `f`, which must return within `limit`; prints how long it took.
fn timed<T>(what: &str, limit: Duration, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    let took = start.elapsed();
    eprintln!("{what}: {took:?}");
    assert!(took < limit, "{what} took {took:?}");
    out
}

fn snapshot_with(nodes: &str) -> String {
    format!("{{\"root\":1,\"nodes\":[{nodes}]}}")
}

#[test]
fn extremes_are_refused_quickly() {
    let document = DocumentGeneration::next();
    let limit = Duration::from_secs(2);
    for depth in [100, 10_000, 100_000, 1_000_000] {
        let arrays = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        assert!(timed("nested arrays", limit, || parse(&arrays, document)).is_err());
        let objects = format!("{}1{}", "{\"a\":".repeat(depth), "}".repeat(depth));
        assert!(timed("nested objects", limit, || parse(&objects, document)).is_err());
        // Nested inside an unknown field of a node, and unterminated.
        let inside = snapshot_with(&format!(
            "{{\"id\":1,\"kind\":\"document\",\"x\":{}",
            "[".repeat(depth)
        ));
        assert!(timed("nested unterminated", limit, || parse(&inside, document)).is_err());
        let inside = snapshot_with(&format!(
            "{{\"id\":1,\"kind\":\"document\",\"x\":{}{}}}",
            "[".repeat(depth),
            "]".repeat(depth)
        ));
        assert!(timed("nested in a node", limit, || parse(&inside, document)).is_err());
    }
    // A string over the bound, 10 MB (over the input bound too), and just under the input bound.
    for len in [
        MAX_STRING_BYTES + 1,
        10 * 1024 * 1024,
        max_input_bytes() - 200,
    ] {
        let text = "a".repeat(len);
        let json = snapshot_with(&format!(
            "{{\"id\":1,\"kind\":\"text\",\"text\":\"{text}\"}}"
        ));
        assert!(
            timed("long string", limit, || parse(&json, document)).is_err(),
            "{len}"
        );
        let escaped = "\\u00e9".repeat(len / 2 + 1);
        let json = snapshot_with(&format!(
            "{{\"id\":1,\"kind\":\"text\",\"text\":\"{escaped}\"}}"
        ));
        assert!(
            timed("long escaped string", limit, || parse(&json, document)).is_err(),
            "{len}"
        );
        let json = snapshot_with(&format!(
            "{{\"id\":1,\"kind\":\"document\",\"x\":\"{text}\"}}"
        ));
        let _ = timed("long unknown string", limit, || parse(&json, document));
    }
    // Over the input bound is refused unread.
    let big = " ".repeat(max_input_bytes() + 1);
    assert!(matches!(
        parse(&big, document),
        Err(ParseError::TooLarge { .. })
    ));
    // Lone and paired surrogates in every position.
    for text in [
        "\\ud800",
        "\\udc00",
        "\\ud800\\ud800",
        "\\udc00\\ud800",
        "\\ud800x",
        "\\ud83d\\ude00",
        "\\ud800\\u0041",
    ] {
        let json = snapshot_with(&format!(
            "{{\"id\":1,\"kind\":\"text\",\"text\":\"{text}\"}}"
        ));
        let _ = parse(&json, document);
        let json = snapshot_with(&format!("{{\"id\":1,\"kind\":\"text\",\"te{text}xt\":1}}"));
        let _ = parse(&json, document);
    }
    // Floods: nodes, children, options, ids, a chain deeper than the tree bound.
    let many: String = (1..=MAX_NODES + 1)
        .map(|i| format!("{{\"id\":{i},\"kind\":\"text\"}}"))
        .collect::<Vec<_>>()
        .join(",");
    assert!(timed("too many nodes", limit, || parse(
        &snapshot_with(&many),
        document
    ))
    .is_err());
    let flat: String = std::iter::once(format!(
        "{{\"id\":1,\"kind\":\"document\",\"children\":[{}]}}",
        (2..=MAX_NODES)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",")
    ))
    .chain((2..=MAX_NODES).map(|i| format!("{{\"id\":{i},\"kind\":\"text\",\"text\":\"x\"}}")))
    .collect::<Vec<_>>()
    .join(",");
    let snapshot = timed("full flat tree", Duration::from_secs(5), || {
        parse(&snapshot_with(&flat), document)
    })
    .unwrap();
    assert_eq!(snapshot.nodes.len(), MAX_NODES);
    let ids = (0..2_000_000)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let json = snapshot_with(&format!(
        "{{\"id\":1,\"kind\":\"document\",\"children\":[{ids}]}}"
    ));
    assert!(timed("child id flood", limit, || parse(&json, document)).is_err());
    let options = vec!["\"o\""; 1_000_000].join(",");
    let json = snapshot_with(&format!(
        "{{\"id\":1,\"kind\":\"select\",\"options\":[{options}]}}"
    ));
    let _ = timed("option flood", limit, || parse(&json, document));
    let chain: String = (1..=MAX_DEPTH + 40)
        .map(|i| {
            format!(
                "{{\"id\":{i},\"kind\":\"container\",\"children\":[{}]}}",
                i + 1
            )
        })
        .chain(std::iter::once(format!(
            "{{\"id\":{},\"kind\":\"text\"}}",
            MAX_DEPTH + 41
        )))
        .collect::<Vec<_>>()
        .join(",");
    assert!(timed("deep chain", limit, || parse(
        &snapshot_with(&chain),
        document
    ))
    .is_err());
    let keys: String = (0..300_000)
        .map(|i| format!("\"k{i}\":{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let json = snapshot_with(&format!("{{\"id\":1,\"kind\":\"document\",{keys}}}"));
    let _ = timed("many unknown keys", limit, || parse(&json, document));
    let numbers = format!(
        "{{\"id\":1,\"kind\":\"document\",\"n\":{}}}",
        "9".repeat(1_000_000)
    );
    let _ = timed("long number", limit, || {
        parse(&snapshot_with(&numbers), document)
    });
    let numbers = format!(
        "{{\"id\":1,\"kind\":\"document\",\"n\":1.{}e{}}}",
        "0".repeat(1_000_000),
        "9".repeat(100_000)
    );
    let _ = timed("long float", limit, || {
        parse(&snapshot_with(&numbers), document)
    });
}

/// Found by `fuzz_snapshots_valid_and_mutated`: a surrogate escape with no partner (a page
/// string cut inside an emoji, which `JSON.stringify` writes as `\ud83d`) made the whole
/// snapshot a syntax error. It reads as U+FFFD, and the escape after a lone high surrogate is
/// read on its own.
#[test]
fn a_lone_surrogate_is_a_replacement_character() {
    let text = |escaped: &str| {
        let json = snapshot_with(&format!(
            "{{\"id\":1,\"kind\":\"text\",\"text\":\"{escaped}\"}}"
        ));
        parse(&json, DocumentGeneration::next())
            .map(|s| s.nodes[0].text.clone().unwrap_or_default())
    };
    assert_eq!(text("a\\ud83d").unwrap(), "a\u{fffd}");
    assert_eq!(text("\\udc00b").unwrap(), "\u{fffd}b");
    assert_eq!(text("\\ud800\\ud800").unwrap(), "\u{fffd}\u{fffd}");
    assert_eq!(text("\\ud800\\u0041").unwrap(), "\u{fffd}A");
    assert_eq!(text("\\ud800x").unwrap(), "\u{fffd}x");
    assert_eq!(text("\\ud83d\\ude00").unwrap(), "\u{1f600}");
    assert!(text("\\ud800\\uzzzz").is_err());
    assert!(text("\\ud800\\u12").is_err());
}
