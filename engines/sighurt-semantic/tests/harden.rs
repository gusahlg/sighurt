//! The extraction script and the action scripts, run by `node` against a fake DOM.
//!
//! Covers the shared work budgets, the reverse-map cap, a non-string input type, and
//! `:disabled` on a control whose own `disabled` property is false. Without `node` the
//! test prints a skip and passes.

use sighurt_api::semantic::{NodeId, SemanticAction};
use sighurt_semantic::action_script;

#[test]
fn helpers_stay_inside_budgets_and_disabled_controls_refuse_writes() {
    let Some(node) = node_binary() else {
        eprintln!("node not available; extraction budget checks were not run");
        return;
    };
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = std::env::temp_dir().join(format!("sighurt-semantic-harden-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let scripts = [
        (
            "set_blocked.js",
            SemanticAction::SetValue {
                node: NodeId(1),
                value: "CHANGED".to_owned(),
            },
        ),
        (
            "set_open.js",
            SemanticAction::SetValue {
                node: NodeId(3),
                value: "OPENED".to_owned(),
            },
        ),
        (
            "set_throw.js",
            SemanticAction::SetValue {
                node: NodeId(4),
                value: "THREW".to_owned(),
            },
        ),
        (
            "select_beta.js",
            SemanticAction::SelectOption {
                node: NodeId(2),
                index: 1,
            },
        ),
        (
            "select_gamma.js",
            SemanticAction::SelectOption {
                node: NodeId(2),
                index: 2,
            },
        ),
        (
            "select_alpha.js",
            SemanticAction::SelectOption {
                node: NodeId(2),
                index: 0,
            },
        ),
    ];
    for (name, action) in scripts {
        std::fs::write(dir.join(name), action_script(&action)).unwrap();
    }
    let output = std::process::Command::new(&node)
        .arg(root.join("tests/js/harden.js"))
        .arg(&root)
        .arg(&dir)
        .output()
        .unwrap_or_else(|err| panic!("running node: {err}"));
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "harden.js failed\n{stdout}{stderr}"
    );
}

fn node_binary() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("node"))
        .find(|candidate| candidate.is_file())
}
