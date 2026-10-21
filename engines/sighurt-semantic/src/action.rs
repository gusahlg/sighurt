//! Scripts that perform one [`SemanticAction`](sighurt_api::semantic::SemanticAction).
//!
//! Owns: turning an action into a JavaScript expression. Must not own: deciding that the action
//! is allowed, or reading a password field.

use sighurt_api::semantic::{NodeId, SemanticAction};

use crate::world::{js_string_literal, state_key_literal};

const TEMPLATE: &str = r#"(function () {
  var STATE_KEY = __SIGHURT_STATE_KEY__;
  var NODE_ID = __SIGHURT_NODE_ID__;
  function fail(why) { return JSON.stringify({ ok: false, why: why }); }
  var slot = null;
  try { slot = Object.getOwnPropertyDescriptor(globalThis, STATE_KEY); }
  catch (e) { slot = null; }
  var state = slot && slot.value;
  if (!state || !state.rev || typeof state.rev.get !== "function") return fail("missing");
  var ref = state.rev.get(NODE_ID);
  if (!ref || typeof ref.deref !== "function") return fail("missing");
  var node = null;
  try { node = ref.deref(); } catch (e2) { node = null; }
  if (!node) return fail("detached");
  if (node.nodeType === 1 && node.isConnected === false) return fail("detached");
  if (node.nodeType === 3) {
    var parent = node.parentNode;
    if (!parent || (parent.nodeType === 1 && parent.isConnected === false)) return fail("detached");
  }
  if (node.nodeType === 9 && !node.defaultView) return fail("detached");
  try {
    __SIGHURT_BODY__
  } catch (e3) {
    return fail("threw");
  }
})()"#;

/// JavaScript that performs `action` and evaluates to `{"ok":true}` or `{"ok":false,"why":...}`.
///
/// The node is the one [`EXTRACT`](crate::EXTRACT) numbered, looked up in the same world. A
/// missing or detached node, or a node that does not take this action, evaluates to `ok: false`.
/// Every string from `action` is embedded as a JSON string literal.
///
/// [`SemanticAction::SetValue`](sighurt_api::semantic::SemanticAction::SetValue) may set a
/// password field. Extraction is what never reads one.
pub fn action_script(action: &SemanticAction) -> String {
    let body = match action {
        SemanticAction::Activate(_) => ACTIVATE,
        SemanticAction::Focus(_) => FOCUS,
        SemanticAction::SetValue { value, .. } => return set_value(action.node(), value),
        SemanticAction::SelectOption { index, .. } => return select_option(action.node(), *index),
        SemanticAction::Submit(_) => SUBMIT,
        SemanticAction::ScrollIntoView(_) => SCROLL,
    };
    assemble(action.node(), body)
}

fn set_value(node: NodeId, value: &str) -> String {
    // The literal is already a JavaScript expression. It is inserted into the body before the
    // body is inserted into the template, so a value that happens to contain a template token
    // is not substituted again.
    let literal = js_string_literal(value);
    let body = format!(
        r#"var tag = node && node.tagName ? String(node.tagName) : "";
    var typ = node && node.type ? String(node.type).toLowerCase() : "";
    var field = tag === "TEXTAREA" || (tag === "INPUT" && typ !== "button" && typ !== "submit" && typ !== "reset" && typ !== "checkbox" && typ !== "radio" && typ !== "file" && typ !== "image" && typ !== "hidden");
    if (!field) return fail("kind");
    var ctor = tag === "TEXTAREA" ? globalThis.HTMLTextAreaElement : globalThis.HTMLInputElement;
    var desc = ctor && Object.getOwnPropertyDescriptor(ctor.prototype, "value");
    if (!desc || typeof desc.set !== "function") return fail("kind");
    desc.set.call(node, {literal});
    if (typeof node.dispatchEvent === "function" && typeof Event === "function") {{
      node.dispatchEvent(new Event("input", {{ bubbles: true }}));
      node.dispatchEvent(new Event("change", {{ bubbles: true }}));
    }}
    return JSON.stringify({{ ok: true }});"#
    );
    assemble(node, &body)
}

fn select_option(node: NodeId, index: u32) -> String {
    let body = format!(
        r#"if (!node || node.tagName !== "SELECT" || !node.options) return fail("kind");
    var index = {index};
    if (!(index >= 0) || index >= node.options.length) return fail("range");
    node.selectedIndex = index;
    if (typeof node.dispatchEvent === "function" && typeof Event === "function") {{
      node.dispatchEvent(new Event("change", {{ bubbles: true }}));
    }}
    return JSON.stringify({{ ok: true }});"#
    );
    assemble(node, &body)
}

fn assemble(node: NodeId, body: &str) -> String {
    // Structural tokens are replaced before the body, which may contain arbitrary page text.
    let mut script = TEMPLATE.to_string();
    script = script.replacen("__SIGHURT_STATE_KEY__", &state_key_literal(), 1);
    script = script.replacen("__SIGHURT_NODE_ID__", &node.0.to_string(), 1);
    script = script.replacen("__SIGHURT_BODY__", body, 1);
    script
}

const ACTIVATE: &str = r#"if (typeof node.click !== "function") return fail("kind");
    node.click();
    return JSON.stringify({ ok: true });"#;

const FOCUS: &str = r#"if (typeof node.focus !== "function") return fail("kind");
    node.focus();
    return JSON.stringify({ ok: true });"#;

const SUBMIT: &str = r#"var form = node.tagName === "FORM" ? node : (node.form && node.form.tagName === "FORM" ? node.form : null);
    if (!form) return fail("kind");
    if (typeof form.requestSubmit === "function") {
      try { form.requestSubmit(); }
      catch (err) { form.submit(); }
    } else {
      form.submit();
    }
    return JSON.stringify({ ok: true });"#;

const SCROLL: &str = r#"if (typeof node.scrollIntoView !== "function") return fail("kind");
    node.scrollIntoView({ block: "center" });
    return JSON.stringify({ ok: true });"#;
