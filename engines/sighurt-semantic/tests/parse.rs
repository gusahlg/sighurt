//! The parser's contract: valid snapshots, and every way a page can hand us something else.

use sighurt_api::ids::DocumentGeneration;
use sighurt_api::semantic::{
    Landmark, NodeId, NodeKind, SemanticSnapshot, SnapshotError, MAX_DEPTH, MAX_NODES, MAX_OPTIONS,
    MAX_STRING_BYTES, MAX_TOTAL_BYTES,
};
use sighurt_semantic::{max_input_bytes, parse, ParseError};

fn document() -> DocumentGeneration {
    DocumentGeneration::next()
}

fn parse_ok(json: &str) -> SemanticSnapshot {
    parse(json, document()).unwrap_or_else(|err| panic!("parse failed: {err}"))
}

#[test]
fn a_document_round_trips() {
    let generation = document();
    // A `document` field in the JSON is page data and is ignored. The checkbox and the select
    // hang off the form so the value is one tree.
    let json = r#"{
        "document": {"serial": 999},
        "truncated": true,
        "root": 1,
        "focus": 4,
        "nodes": [
            {"id": 1, "kind": "document", "children": [2, 3]},
            {"id": 2, "kind": "heading", "level": 1, "children": [5]},
            {"id": 3, "kind": "landmark", "landmark": "form", "label": "Lookup", "children": [4, 6, 7]},
            {"id": 4, "kind": "textfield", "label": "Name", "value": "Ada"},
            {"id": 5, "kind": "text", "text": "Hello"},
            {"id": 6, "kind": "checkbox", "checked": false},
            {"id": 7, "kind": "select", "options": ["Cat", "Dog"], "selected": 0, "disabled": true}
        ]
    }"#;
    let snapshot = parse(json, generation).unwrap();
    assert_eq!(snapshot.document, generation);
    assert!(snapshot.truncated);
    assert_eq!(snapshot.root, NodeId(1));
    assert_eq!(snapshot.focus, Some(NodeId(4)));
    assert_eq!(snapshot.nodes.len(), 7);
    assert_eq!(snapshot.nodes[1].kind, NodeKind::Heading(1));
    assert_eq!(snapshot.nodes[2].kind, NodeKind::Landmark(Landmark::Form));
    assert_eq!(snapshot.nodes[2].label.as_deref(), Some("Lookup"));
    assert_eq!(snapshot.nodes[3].value.as_deref(), Some("Ada"));
    assert_eq!(snapshot.nodes[4].text.as_deref(), Some("Hello"));
    assert_eq!(snapshot.nodes[5].checked, Some(false));
    assert_eq!(snapshot.nodes[6].options, ["Cat", "Dog"]);
    assert_eq!(snapshot.nodes[6].selected, Some(0));
    assert!(snapshot.nodes[6].disabled);
    assert!(!snapshot.nodes[3].password);
    assert!(snapshot.check().is_ok());
}

#[test]
fn unknown_kinds_become_containers() {
    let snapshot = parse_ok(
        r#"{"root":1,"nodes":[
            {"id":1,"kind":"document","children":[2,3,4,5]},
            {"id":2,"kind":"marquee"},
            {"id":3,"kind":"heading","level":7},
            {"id":4,"kind":"heading"},
            {"id":5,"kind":"landmark","landmark":"nope"}
        ]}"#,
    );
    assert!(snapshot.nodes[1..]
        .iter()
        .all(|node| node.kind == NodeKind::Container));
    let named = parse_ok(
        r#"{"root":1,"nodes":[
            {"id":1,"kind":"document","children":[2]},
            {"id":2,"kind":"landmark","landmark":"main","label":"Content"}
        ]}"#,
    );
    assert_eq!(named.nodes[1].kind, NodeKind::Landmark(Landmark::Main));
}

#[test]
fn the_callers_document_is_stamped() {
    let first = document();
    let second = document();
    let json = r#"{"document":1,"root":1,"nodes":[{"id":1,"kind":"document"}]}"#;
    assert_eq!(parse(json, first).unwrap().document, first);
    assert_eq!(parse(json, second).unwrap().document, second);
    assert_ne!(first, second);
}

#[test]
fn truncation_is_carried() {
    let snapshot = parse_ok(r#"{"truncated":true,"root":1,"nodes":[{"id":1,"kind":"document"}]}"#);
    assert!(snapshot.truncated);
    let absent = parse_ok(r#"{"root":1,"nodes":[{"id":1,"kind":"document"}]}"#);
    assert!(!absent.truncated);
}

#[test]
fn oversized_input_is_refused_before_parsing() {
    let limit = max_input_bytes();
    assert_eq!(
        limit,
        MAX_TOTAL_BYTES * 2 + MAX_NODES * sighurt_semantic::NODE_JSON_OVERHEAD
    );
    let big = " ".repeat(limit + 1);
    match parse(&big, document()) {
        Err(ParseError::TooLarge { len, limit: got }) => {
            assert_eq!(len, limit + 1);
            assert_eq!(got, limit);
        }
        other => panic!("expected TooLarge, got {other:?}"),
    }
}

#[test]
fn malformed_json_is_an_error() {
    let generation = document();
    for json in ["", "{", "[", "}", "true,", "{\"root\":1,}", "\u{0000}"] {
        assert!(
            matches!(parse(json, generation), Err(ParseError::Syntax(_))),
            "{json:?}"
        );
    }
}

#[test]
fn wrong_types_are_errors() {
    let generation = document();
    let cases = [
        "null",
        "[]",
        "\"text\"",
        "42",
        r#"{"root":1,"nodes":{}}"#,
        r#"{"root":"1","nodes":[]}"#,
        r#"{"root":1.5,"nodes":[]}"#,
        r#"{"root":1,"nodes":[null]}"#,
        r#"{"root":1,"nodes":[{"id":"x","kind":"document"}]}"#,
        r#"{"root":1,"nodes":[{"id":1,"kind":1}]}"#,
        r#"{"root":1,"nodes":[{"id":1,"kind":"document","children":{}}]}"#,
        r#"{"root":1,"nodes":[{"id":1,"kind":"document","children":["no"]}]}"#,
        r#"{"root":1,"nodes":[{"id":1,"kind":"text","text":1}]}"#,
        r#"{"root":1,"nodes":[{"id":1,"kind":"checkbox","checked":"yes"}]}"#,
        r#"{"root":1,"nodes":[{"id":1,"kind":"document","disabled":"no"}]}"#,
        r#"{"root":1,"nodes":[{"id":1,"kind":"document","password":1}]}"#,
        r#"{"root":1,"nodes":[{"id":1,"kind":"heading","level":"big"}]}"#,
        r#"{"root":1,"nodes":[{"id":1,"kind":"heading","level":1.5}]}"#,
        r#"{"root":1,"nodes":[{"id":1,"kind":"landmark","landmark":1}]}"#,
        r#"{"root":1,"nodes":[{"id":1,"kind":"select","options":[1]}]}"#,
        r#"{"root":1,"focus":{},"nodes":[{"id":1,"kind":"document"}]}"#,
        r#"{"root":1,"truncated":"yes","nodes":[{"id":1,"kind":"document"}]}"#,
        r#"{"nodes":[{"id":1,"kind":"document"}]}"#,
        r#"{"root":1}"#,
    ];
    for json in cases {
        assert!(parse(json, generation).is_err(), "{json}");
    }
}

#[test]
fn trees_that_are_not_trees_fail() {
    let generation = document();
    let cases = [
        (
            r#"{"root":1,"nodes":[
                {"id":1,"kind":"document","children":[2]},
                {"id":2,"kind":"container","children":[1]}
            ]}"#,
            SnapshotError::NotATree(NodeId(1)),
        ),
        (
            r#"{"root":1,"nodes":[
                {"id":1,"kind":"document","children":[2,3]},
                {"id":2,"kind":"paragraph","children":[4]},
                {"id":3,"kind":"paragraph","children":[4]},
                {"id":4,"kind":"text","text":"x"}
            ]}"#,
            SnapshotError::NotATree(NodeId(4)),
        ),
        (
            r#"{"root":1,"nodes":[
                {"id":1,"kind":"document"},
                {"id":2,"kind":"text","text":"stray"}
            ]}"#,
            SnapshotError::NotATree(NodeId(2)),
        ),
        (
            r#"{"root":1,"nodes":[{"id":1,"kind":"document","children":[9]}]}"#,
            SnapshotError::UnknownNode(NodeId(9)),
        ),
        (
            r#"{"root":1,"focus":3,"nodes":[{"id":1,"kind":"document"}]}"#,
            SnapshotError::UnknownNode(NodeId(3)),
        ),
        (
            r#"{"root":1,"nodes":[
                {"id":1,"kind":"document"},
                {"id":1,"kind":"text"}
            ]}"#,
            SnapshotError::DuplicateId(NodeId(1)),
        ),
        (r#"{"root":1,"nodes":[]}"#, SnapshotError::NodeCount(0)),
    ];
    for (json, expected) in cases {
        match parse(json, generation) {
            Err(ParseError::Snapshot(error)) => assert_eq!(error, expected, "{json}"),
            other => panic!("expected {expected:?}, got {other:?}"),
        }
    }
}

#[test]
fn a_password_value_is_an_error() {
    let generation = document();
    let leaked = r#"{"root":1,"nodes":[
        {"id":1,"kind":"document","children":[2]},
        {"id":2,"kind":"textfield","password":true,"value":"secret"}
    ]}"#;
    assert_eq!(
        parse(leaked, generation).unwrap_err(),
        ParseError::Snapshot(SnapshotError::PasswordValue(NodeId(2)))
    );
    let empty = r#"{"root":1,"nodes":[
        {"id":1,"kind":"document","children":[2]},
        {"id":2,"kind":"textfield","password":true,"value":""}
    ]}"#;
    assert!(matches!(
        parse(empty, generation),
        Err(ParseError::Snapshot(SnapshotError::PasswordValue(_)))
    ));
    let clean = parse_ok(
        r#"{"root":1,"nodes":[
            {"id":1,"kind":"document","children":[2]},
            {"id":2,"kind":"textfield","password":true,"label":"Secret"}
        ]}"#,
    );
    assert!(clean.nodes[1].password);
    assert!(clean.nodes[1].value.is_none());
}

#[test]
fn string_option_and_depth_bounds_fail_closed() {
    let generation = document();
    let long = "x".repeat(MAX_STRING_BYTES + 1);
    let json = format!(
        r#"{{"root":1,"nodes":[{{"id":1,"kind":"document","children":[2]}},{{"id":2,"kind":"text","text":{}}}]}}"#,
        serde_json::to_string(&long).unwrap()
    );
    assert_eq!(
        parse(&json, generation).unwrap_err(),
        ParseError::Snapshot(SnapshotError::TooMuchText)
    );
    let exact = "é".repeat(MAX_STRING_BYTES / 2);
    assert_eq!(exact.len(), MAX_STRING_BYTES);
    let json = format!(
        r#"{{"root":1,"nodes":[{{"id":1,"kind":"document","children":[2]}},{{"id":2,"kind":"text","text":{}}}]}}"#,
        serde_json::to_string(&exact).unwrap()
    );
    assert!(parse(&json, generation).is_ok());

    let options = vec!["a"; MAX_OPTIONS + 1];
    let json = format!(
        r#"{{"root":1,"nodes":[{{"id":1,"kind":"document","children":[2]}},{{"id":2,"kind":"select","options":{}}}]}}"#,
        serde_json::to_string(&options).unwrap()
    );
    assert_eq!(
        parse(&json, generation).unwrap_err(),
        ParseError::Snapshot(SnapshotError::Options(NodeId(2)))
    );
    let bad_index = r#"{"root":1,"nodes":[
        {"id":1,"kind":"document","children":[2]},
        {"id":2,"kind":"select","options":["a"],"selected":1}
    ]}"#;
    assert_eq!(
        parse(bad_index, generation).unwrap_err(),
        ParseError::Snapshot(SnapshotError::Options(NodeId(2)))
    );

    assert!(parse(&chain(MAX_DEPTH), generation).is_ok());
    assert_eq!(
        parse(&chain(MAX_DEPTH + 1), generation).unwrap_err(),
        ParseError::Snapshot(SnapshotError::TooDeep)
    );
}

#[test]
fn node_count_stops_at_the_cap() {
    let generation = document();
    match parse(&flat(MAX_NODES + 1), generation) {
        Err(ParseError::Snapshot(SnapshotError::NodeCount(count))) => {
            assert_eq!(count, MAX_NODES + 1);
        }
        other => panic!("expected NodeCount, got {other:?}"),
    }
    assert!(parse(&flat(MAX_NODES), generation).is_ok());
}

#[test]
fn total_text_has_a_hard_edge() {
    let generation = document();
    let piece = "x".repeat(MAX_STRING_BYTES);
    let fitting = MAX_TOTAL_BYTES / MAX_STRING_BYTES;
    assert!(parse(&texts(fitting, &piece), generation).is_ok());
    assert_eq!(
        parse(&texts(fitting + 1, &piece), generation).unwrap_err(),
        ParseError::Snapshot(SnapshotError::TooMuchText)
    );
}

/// `edges` links below the root. `edges == MAX_DEPTH` is the deepest tree that passes.
fn chain(edges: usize) -> String {
    let mut nodes = Vec::with_capacity(edges + 1);
    for id in 1..=edges + 1 {
        if id <= edges {
            nodes.push(format!(
                r#"{{"id":{id},"kind":"container","children":[{}]}}"#,
                id + 1
            ));
        } else {
            nodes.push(format!(r#"{{"id":{id},"kind":"text","text":"end"}}"#));
        }
    }
    format!(r#"{{"root":1,"nodes":[{}]}}"#, nodes.join(","))
}

/// A document whose children are `count - 1` text nodes, `count` nodes in all.
fn flat(count: usize) -> String {
    assert!(count >= 1);
    let mut children = Vec::with_capacity(count - 1);
    let mut nodes = Vec::with_capacity(count);
    for id in 2..=count {
        children.push(id.to_string());
        nodes.push(format!(r#"{{"id":{id},"kind":"text","text":"n"}}"#));
    }
    format!(
        r#"{{"root":1,"nodes":[{{"id":1,"kind":"document","children":[{}]}},{}]}}"#,
        children.join(","),
        nodes.join(",")
    )
}

fn texts(count: usize, piece: &str) -> String {
    let encoded = serde_json::to_string(piece).unwrap();
    let mut children = Vec::with_capacity(count);
    let mut nodes = Vec::with_capacity(count);
    for id in 2..=count + 1 {
        children.push(id.to_string());
        nodes.push(format!(r#"{{"id":{id},"kind":"text","text":{encoded}}}"#));
    }
    format!(
        r#"{{"root":1,"nodes":[{{"id":1,"kind":"document","children":[{}]}},{}]}}"#,
        children.join(","),
        nodes.join(",")
    )
}
