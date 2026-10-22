//! Scripts that perform one [`SemanticAction`](sighurt_api::semantic::SemanticAction).
//!
//! Owns: turning an action into a JavaScript expression. Must not own: deciding that the action
//! is allowed, or reading a password field.

use sighurt_api::semantic::{NodeId, SemanticAction};

use crate::world::{js_string_literal, state_key_literal};

const TEMPLATE: &str = r#"(function () {
  var STATE_KEY = __SIGHURT_STATE_KEY__;
  var NODE_ID = __SIGHURT_NODE_ID__;
  var stringify = JSON.stringify;
  var create = Object.create;
  var getOwn = Object.getOwnPropertyDescriptor;
  var mapGet = Map.prototype.get;
  var elMatches = null;
  try {
    var matchDesc = (typeof Element === "function") && getOwn(Element.prototype, "matches");
    if (matchDesc && typeof matchDesc.value === "function") elMatches = matchDesc.value;
  } catch (matchErr) { elMatches = null; }
  function result(ok, why) {
    var out = create(null);
    out.ok = ok;
    if (!ok) out.why = why;
    return stringify(out);
  }
  function fail(why) { return result(false, why); }
  function effectivelyDisabled(el) {
    if (!el) return true;
    try {
      var fn = elMatches;
      if (typeof fn !== "function") fn = el.matches;
      if (typeof fn === "function") return fn.call(el, ":disabled") === true;
    } catch (e) { return true; }
    try { return el.disabled === true; }
    catch (e2) { return true; }
  }
  function blocked() {
    if (effectivelyDisabled(node)) return "disabled";
    try {
      if (typeof node.getAttribute === "function" && node.getAttribute("aria-disabled") === "true") return "disabled";
    } catch (e2) { return "disabled"; }
    return "";
  }
  function optionBlocked(option) {
    if (effectivelyDisabled(option)) return true;
    var parent = null;
    try { parent = option.parentNode; } catch (e) { return true; }
    if (!parent) return false;
    var tag = "";
    try { tag = parent.tagName ? String(parent.tagName) : ""; }
    catch (e2) { return true; }
    if (tag !== "OPTGROUP") return false;
    return effectivelyDisabled(parent);
  }
  function readOnly() {
    try { if (node.readOnly === true) return true; }
    catch (e) { return true; }
    try {
      if (typeof node.getAttribute === "function" && (node.getAttribute("readonly") !== null || node.getAttribute("aria-readonly") === "true")) return true;
    } catch (e2) { return true; }
    return false;
  }
  var slot = null;
  try { slot = getOwn(globalThis, STATE_KEY); }
  catch (e) { slot = null; }
  var state = slot && slot.value;
  if (!state || !state.rev || typeof mapGet !== "function") return fail("missing");
  var ref = null;
  try { ref = mapGet.call(state.rev, NODE_ID); } catch (e1) { ref = null; }
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
/// missing or detached node, a node that does not take this action, or a disabled or read-only
/// control evaluates to `ok: false`. Every string from `action` is embedded as a JSON string
/// literal. Records are null-prototype objects, serialized with the `JSON.stringify` captured
/// at the start of the script.
///
/// [`SemanticAction::SetValue`](sighurt_api::semantic::SemanticAction::SetValue) may set a
/// password field. Extraction is what never reads one. Submit uses `requestSubmit` only.
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
        r#"var whyBlocked = blocked();
    if (whyBlocked) return fail(whyBlocked);
    if (readOnly()) return fail("readonly");
    var tag = node && node.tagName ? String(node.tagName) : "";
    var typ = "";
    try {{ typ = node && node.type ? String(node.type).toLowerCase() : ""; }}
    catch (typeErr) {{ return fail("kind"); }}
    var field = tag === "TEXTAREA" || (tag === "INPUT" && typ !== "button" && typ !== "submit" && typ !== "reset" && typ !== "checkbox" && typ !== "radio" && typ !== "file" && typ !== "image" && typ !== "hidden");
    if (!field) return fail("kind");
    var ctor = tag === "TEXTAREA" ? globalThis.HTMLTextAreaElement : globalThis.HTMLInputElement;
    var desc = ctor && getOwn(ctor.prototype, "value");
    if (!desc || typeof desc.set !== "function") return fail("kind");
    desc.set.call(node, {literal});
    if (typeof node.dispatchEvent === "function" && typeof Event === "function") {{
      node.dispatchEvent(new Event("input", {{ bubbles: true }}));
      node.dispatchEvent(new Event("change", {{ bubbles: true }}));
    }}
    return result(true);"#
    );
    assemble(node, &body)
}

fn select_option(node: NodeId, index: u32) -> String {
    let body = format!(
        r#"var whyBlocked = blocked();
    if (whyBlocked) return fail(whyBlocked);
    if (readOnly()) return fail("readonly");
    if (!node || node.tagName !== "SELECT" || !node.options) return fail("kind");
    var index = {index};
    if (!(index >= 0) || index >= node.options.length) return fail("range");
    var option = null;
    try {{ option = node.options[index]; }} catch (optErr) {{ return fail("range"); }}
    if (!option || optionBlocked(option)) return fail("disabled");
    node.selectedIndex = index;
    if (typeof node.dispatchEvent === "function" && typeof Event === "function") {{
      node.dispatchEvent(new Event("change", {{ bubbles: true }}));
    }}
    return result(true);"#
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

const ACTIVATE: &str = r#"var whyBlocked = blocked();
    if (whyBlocked) return fail(whyBlocked);
    if (readOnly()) return fail("readonly");
    if (typeof node.click !== "function") return fail("kind");
    node.click();
    return result(true);"#;

const FOCUS: &str = r#"var whyBlocked = blocked();
    if (whyBlocked) return fail(whyBlocked);
    if (typeof node.focus !== "function") return fail("kind");
    node.focus();
    return result(true);"#;

/// Submit only through `requestSubmit`, so constraint validation runs. A disabled control is
/// refused. There is no `submit()` fallback: that API skips validation.
const SUBMIT: &str = r#"var whyBlocked = blocked();
    if (whyBlocked) return fail(whyBlocked);
    var form = node.tagName === "FORM" ? node : (node.form && node.form.tagName === "FORM" ? node.form : null);
    if (!form || typeof form.requestSubmit !== "function") return fail("kind");
    form.requestSubmit();
    return result(true);"#;

const SCROLL: &str = r#"if (typeof node.scrollIntoView !== "function") return fail("kind");
    node.scrollIntoView({ block: "center" });
    return result(true);"#;
