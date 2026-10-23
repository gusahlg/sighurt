//! The scripts embed the semantic bounds and page text as data, and they parse as JavaScript
//! when `node` is on `PATH`.

use sighurt_api::semantic::{
    NodeId, SemanticAction, MAX_DEPTH, MAX_NODES, MAX_OPTIONS, MAX_STRING_BYTES, MAX_TOTAL_BYTES,
};
use sighurt_semantic::{action_script, extract_script, EXTRACT, TIME_BUDGET_MS};

fn state_key_assignment(script: &str) -> &str {
    let marker = "var STATE_KEY = ";
    let start = script.find(marker).expect("script names the state slot") + marker.len();
    let rest = &script[start..];
    let end = rest.find(';').expect("state slot assignment ends");
    &rest[..end]
}

#[test]
fn bounds_are_the_api_constants() {
    let src = EXTRACT.as_str();
    assert!(src.contains(&format!("var MAX_NODES = {MAX_NODES};")));
    assert!(src.contains(&format!("var MAX_DEPTH = {MAX_DEPTH};")));
    assert!(src.contains(&format!("var MAX_STRING_BYTES = {MAX_STRING_BYTES};")));
    assert!(src.contains(&format!("var MAX_TOTAL_BYTES = {MAX_TOTAL_BYTES};")));
    assert!(src.contains(&format!("var MAX_OPTIONS = {MAX_OPTIONS};")));
    assert!(src.contains(&format!("var TIME_BUDGET_MS = {TIME_BUDGET_MS};")));
    assert!(!src.contains("__SIGHURT_"));
    let slower = extract_script(500);
    assert!(slower.contains("var TIME_BUDGET_MS = 500;"));
    assert_eq!(
        slower.replace("var TIME_BUDGET_MS = 500;", &format!("var TIME_BUDGET_MS = {TIME_BUDGET_MS};")),
        *src,
        "only the budget differs"
    );
    assert!(src.contains("JSON.stringify"));
    assert!(src.contains("Never read el.value for a password field."));
    assert!(src.contains("obj.password = true"));
    assert!(src.contains("Object.create"));
    assert!(src.contains("new WeakRef("));
    assert!(src.contains("var stack"));
    assert!(!src.contains("deref: function"));
    assert!(src.trim_start().starts_with("/*") || src.trim_start().starts_with('('));
}

#[test]
fn extract_and_actions_share_the_state_slot() {
    let extract = state_key_assignment(EXTRACT.as_str());
    let action = action_script(&SemanticAction::Focus(NodeId(1)));
    assert_eq!(extract, state_key_assignment(&action));
    assert!(extract.starts_with('"') && extract.ends_with('"'));
    assert!(extract.len() > 16);
}

#[test]
fn actions_name_their_node_and_do_that_action() {
    let id = NodeId(42);
    let activate = action_script(&SemanticAction::Activate(id));
    assert!(activate.contains("var NODE_ID = 42;"));
    assert!(activate.contains("node.click()"));
    let focus = action_script(&SemanticAction::Focus(id));
    assert!(focus.contains("node.focus()"));
    let submit = action_script(&SemanticAction::Submit(id));
    assert!(submit.contains("requestSubmit"));
    assert!(
        !submit.contains("form.submit()") && !submit.contains(".submit()"),
        "submit must not fall back to HTMLFormElement.submit"
    );
    assert!(activate.contains("\"disabled\""));
    assert!(activate.contains("fail(\"readonly\")"));
    assert!(!focus.contains("fail(\"readonly\")"));
    assert!(submit.contains("\"disabled\""));
    let scroll = action_script(&SemanticAction::ScrollIntoView(id));
    assert!(scroll.contains("scrollIntoView({ block: \"center\" })"));
    let select = action_script(&SemanticAction::SelectOption { node: id, index: 3 });
    assert!(select.contains("var index = 3;"));
    assert!(select.contains("selectedIndex"));
    assert!(select.contains("\"change\""));
    assert!(activate.contains(":disabled"));
    assert!(select.contains(":disabled"));
    assert!(select.contains("OPTGROUP"));
}

#[test]
fn set_value_embeds_a_json_string_and_may_target_any_text_field() {
    let value = "\"};alert(1)//\u{2028}\u{2029}\n\u{0000}😀";
    let script = action_script(&SemanticAction::SetValue {
        node: NodeId(7),
        value: value.to_owned(),
    });
    assert!(script.contains("var NODE_ID = 7;"));
    assert!(script.contains("desc.set.call"));
    assert!(script.contains("\"input\""));
    assert!(script.contains("\"change\""));
    // Password is not a reason to refuse a write. The script never mentions the word.
    assert!(!script.contains("password"));
    assert!(!script.contains('\u{2028}'));
    assert!(!script.contains('\u{2029}'));
    let mut literal = serde_json::to_string(value).unwrap();
    literal = literal
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    assert!(script.contains(&literal), "missing {literal} in {script}");
    // The breakout is inside the literal, with the quote escaped.
    assert!(script.contains(r#"\"};alert(1)//"#));
}

#[test]
fn a_template_token_inside_a_value_is_not_rewritten() {
    let value = "keep __SIGHURT_STATE_KEY__ and __SIGHURT_NODE_ID__";
    let script = action_script(&SemanticAction::SetValue {
        node: NodeId(1),
        value: value.to_owned(),
    });
    assert!(script.contains("__SIGHURT_STATE_KEY__"));
    assert!(script.contains("__SIGHURT_NODE_ID__"));
    assert!(script.contains("var NODE_ID = 1;"));
    assert!(script.contains("var STATE_KEY = \"s"));
}

#[test]
fn page_fixtures_are_present() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pages");
    for name in [
        "headings.html",
        "links.html",
        "forms.html",
        "hidden.html",
        "iframe.html",
        "shadow.html",
        "landmarks.html",
        "huge-list.html",
        "harden.html",
        "inherit.html",
    ] {
        let path = root.join(name);
        let text = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{path:?}: {err}"));
        assert!(
            text.contains("<html") || text.contains("<!DOCTYPE"),
            "{name}"
        );
        assert!(text.len() > 64, "{name}");
    }
    let huge = std::fs::read_to_string(root.join("huge-list.html")).unwrap();
    assert!(
        huge.matches("<li>").count() > 10_000,
        "the list must exceed the node cap"
    );
}

#[test]
fn scripts_are_valid_javascript_when_node_is_installed() {
    let Some(node) = node_binary() else {
        eprintln!("node not available; JavaScript syntax was not checked");
        return;
    };
    let dir = std::env::temp_dir().join(format!("sighurt-semantic-js-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let scripts = [
        ("extract.js", EXTRACT.as_str().to_owned()),
        (
            "activate.js",
            action_script(&SemanticAction::Activate(NodeId(1))),
        ),
        ("focus.js", action_script(&SemanticAction::Focus(NodeId(2)))),
        (
            "value.js",
            action_script(&SemanticAction::SetValue {
                node: NodeId(3),
                value: "a\"b\\c</script>\n".to_owned(),
            }),
        ),
        (
            "select.js",
            action_script(&SemanticAction::SelectOption {
                node: NodeId(4),
                index: 0,
            }),
        ),
        (
            "submit.js",
            action_script(&SemanticAction::Submit(NodeId(5))),
        ),
        (
            "scroll.js",
            action_script(&SemanticAction::ScrollIntoView(NodeId(6))),
        ),
    ];
    let mut failed = false;
    for (name, source) in scripts {
        let path = dir.join(name);
        std::fs::write(&path, source).unwrap();
        let output = std::process::Command::new(&node)
            .arg("--check")
            .arg(&path)
            .output()
            .unwrap_or_else(|err| panic!("running node --check: {err}"));
        if !output.status.success() {
            failed = true;
            eprintln!(
                "node --check {name} failed:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let _ = std::fs::remove_file(&path);
    }
    let _ = std::fs::remove_dir(&dir);
    assert!(!failed, "node --check rejected a script");
}

fn node_binary() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("node"))
        .find(|candidate| candidate.is_file())
}
