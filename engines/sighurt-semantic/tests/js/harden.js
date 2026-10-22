// Behavioral checks for the extraction script and the action scripts.
// Run by tests/harden.rs. Expects the crate root and a directory of action scripts.
"use strict";

const fs = require("fs");
const path = require("path");

const root = process.argv[2];
const actionDir = process.argv[3];
if (!root || !actionDir) {
  console.error("usage: node harden.js <crate> <action-dir>");
  process.exit(2);
}

const extractSrc = fs.readFileSync(path.join(root, "src/extract.js"), "utf8");
let failed = 0;

function fail(message) {
  failed++;
  console.error("FAIL " + message);
}

function substitute(bounds) {
  let src = extractSrc;
  const pairs = [
    ["__SIGHURT_MAX_NODES__", String(bounds.nodes)],
    ["__SIGHURT_MAX_DEPTH__", String(bounds.depth)],
    ["__SIGHURT_MAX_STRING_BYTES__", String(bounds.stringBytes)],
    ["__SIGHURT_MAX_TOTAL_BYTES__", String(bounds.totalBytes)],
    ["__SIGHURT_MAX_OPTIONS__", String(bounds.options)],
    ["__SIGHURT_TIME_BUDGET_MS__", String(bounds.time)],
    ["__SIGHURT_STATE_KEY__", JSON.stringify(bounds.key)],
  ];
  for (const [token, value] of pairs) {
    if (!src.includes(token)) throw new Error("missing " + token);
    src = src.replace(token, value);
  }
  return src;
}

function el(tag, children) {
  const node = {
    nodeType: 1,
    tagName: tag,
    childNodes: children || [],
    attrs: {},
    parentNode: null,
    ownerDocument: null,
    getAttribute(name) {
      return Object.prototype.hasOwnProperty.call(this.attrs, name) ? this.attrs[name] : null;
    },
    hasAttribute(name) {
      return this.getAttribute(name) != null;
    },
  };
  for (const child of node.childNodes) child.parentNode = node;
  return node;
}

function text(value) {
  let current = value;
  const node = {
    nodeType: 3,
    parentNode: null,
    childNodes: [],
    ownerDocument: null,
    get nodeValue() {
      if (node.onRead) node.onRead();
      return current;
    },
    set nodeValue(next) {
      current = next;
    },
  };
  return node;
}

function bind(doc, node) {
  node.ownerDocument = doc;
  const kids = node.childNodes || [];
  for (let i = 0; i < kids.length; i++) bind(doc, kids[i]);
}

function makeDoc(bodyChildren) {
  const body = el("BODY", bodyChildren);
  const html = el("HTML", [body]);
  const doc = {
    nodeType: 9,
    childNodes: [html],
    documentElement: html,
    body,
    activeElement: null,
    defaultView: null,
    ownerDocument: null,
    _ids: {},
    getElementById(id) {
      return this._ids[id] || null;
    },
  };
  doc.ownerDocument = doc;
  bind(doc, html);
  return doc;
}

let clock = 0;
function installClock() {
  clock = 0;
  global.performance = {
    now() {
      return clock;
    },
  };
}

function runExtract(bounds, doc) {
  installClock();
  global.document = doc;
  const src = substitute(bounds);
  return eval(src);
}

function stateOf(key) {
  const desc = Object.getOwnPropertyDescriptor(globalThis, key);
  return desc && desc.value;
}

function mapCount(map) {
  let n = 0;
  map.forEach(() => {
    n++;
  });
  return n;
}

// --- reverse map: rotating reused nodes must not grow the map past the cap ---
function checkReverseMap() {
  const key = "sighurtRevTest";
  const capNodes = 8;
  const revCap = capNodes * 2;
  const bounds = {
    nodes: capNodes,
    depth: 64,
    stringBytes: 1000,
    totalBytes: 100000,
    options: 10,
    time: 100000,
    key,
  };
  // A stable prefix stays in every walk (fresh, and earliest in the map). The tail
  // rotates, so those prefix entries sit in front of entries that have gone stale.
  const pool = [];
  for (let i = 0; i < 3 + 24 * 4; i++) pool.push(el("DIV", []));
  const doc = makeDoc([]);
  const prefix = 3;
  const tail = 4;
  let last = "";
  for (let round = 0; round < 24; round++) {
    const kids = [];
    for (let i = 0; i < prefix; i++) kids.push(pool[i]);
    for (let i = 0; i < tail; i++) kids.push(pool[prefix + round * tail + i]);
    doc.body.childNodes = kids;
    for (const kid of kids) kid.parentNode = doc.body;
    last = runExtract(bounds, doc);
  }
  const state = stateOf(key);
  if (!state || !state.rev) {
    fail("reverse map: extraction did not keep state; last=" + String(last).slice(0, 120));
    return;
  }
  const size = mapCount(state.rev);
  const reported = state.nrev;
  if (!(size <= revCap && size === reported)) {
    fail(
      "reverse map: size " + size + " reported " + reported + " cap " + revCap
    );
  }
  if (!String(last).includes('"kind":"document"')) {
    fail("reverse map: last snapshot is not a document");
  }
}

function charBounds(key) {
  return {
    nodes: 400,
    depth: 6,
    stringBytes: 500,
    totalBytes: 40,
    options: 20,
    time: 100000,
    key,
  };
}

// charsLeft starts at totalBytes * 8 = 320. Each text node below is 16 chars.
function checkLabelledBy() {
  let reads = 0;
  const labelled = el("DIV", []);
  const pieces = [];
  for (let i = 0; i < 400; i++) {
    const value = i === 0 ? "STARTMARK_PADXXXX" : "WORD" + String(i).padStart(12, "0");
    const bit = text(value);
    bit._v = value;
    bit.onRead = () => {
      reads++;
    };
    pieces.push(bit);
  }
  labelled.childNodes = pieces;
  Object.defineProperty(labelled, "textContent", {
    configurable: true,
    get() {
      let all = "";
      for (const kid of pieces) {
        reads++;
        all += kid._v;
      }
      return all;
    },
  });
  const section = el("SECTION", [text("section body")]);
  section.attrs["aria-labelledby"] = "tgt";
  const doc = makeDoc([section]);
  doc._ids.tgt = labelled;
  labelled.ownerDocument = doc;
  const json = String(runExtract(charBounds("sighurtLabelTest"), doc));
  if (!json.includes("STARTMARK")) fail("budgets: labelled-by text missing");
  if (reads > 80) fail("budgets: aria-labelledby read " + reads + " text nodes");
}

function checkOptionText() {
  let reads = 0;
  let getter = 0;
  const optionKids = [];
  for (let i = 0; i < 400; i++) {
    const value = i === 0 ? "OPTSTART_PADXXXXX" : "OPT" + String(i).padStart(13, "0");
    const bit = text(value);
    bit._v = value;
    bit.onRead = () => {
      reads++;
    };
    optionKids.push(bit);
  }
  const option = el("OPTION", optionKids);
  Object.defineProperty(option, "text", {
    configurable: true,
    get() {
      getter++;
      let all = "";
      for (const kid of optionKids) {
        reads++;
        all += kid._v;
      }
      return all;
    },
  });
  const select = el("SELECT", [option]);
  select.options = [option];
  select.selectedIndex = 0;
  select.attrs["aria-label"] = "Pet";
  const doc = makeDoc([select]);
  const json = String(runExtract(charBounds("sighurtOptionTest"), doc));
  if (getter !== 0 || reads > 80) {
    fail("budgets: option text getter " + getter + " descendant reads " + reads);
  }
  if (!json.includes("OPTSTART")) fail("budgets: option text missing");
}

function checkDeepLabel() {
  let deepTouches = 0;
  let leaf = el("SPAN", [text("deepleaf")]);
  for (let i = 0; i < 250; i++) {
    const parent = el("SPAN", [leaf]);
    parent._kids = parent.childNodes;
    Object.defineProperty(parent, "childNodes", {
      configurable: true,
      get() {
        deepTouches++;
        return parent._kids;
      },
    });
    leaf = parent;
  }
  const input = el("INPUT", []);
  input.attrs["aria-label"] = "Deep";
  input.type = "text";
  input.value = "v";
  const label = el("LABEL", [input, leaf]);
  input.labels = [label];
  input.id = "deep";
  label.htmlFor = "";
  const doc = makeDoc([label]);
  deepTouches = 0;
  runExtract(
    {
      nodes: 800,
      depth: 6,
      stringBytes: 200,
      totalBytes: 50000,
      options: 8,
      time: 100000,
      key: "sighurtDeepTest",
    },
    doc
  );
  if (deepTouches > 40) {
    fail("budgets: label walk touched " + deepTouches + " levels (depth cap is 6)");
  }
}

function checkWideLabel() {
  // Wide label under a short time budget. The child list must not be stacked eagerly.
  const wideKey = "sighurtWideTest";
  const wideBounds = {
    nodes: 1000,
    depth: 32,
    stringBytes: 200,
    totalBytes: 5000,
    options: 8,
    time: 30,
    key: wideKey,
  };
  let indexReads = 0;
  let arm = false;
  const wideInput = el("INPUT", []);
  wideInput.type = "text";
  wideInput.value = "w";
  wideInput.attrs["aria-label"] = "Wide";
  const wide = el("DIV", []);
  const wideKids = new Proxy([], {
    get(target, prop, recv) {
      if (prop === "length") return 900;
      if (typeof prop === "string" && /^\d+$/.test(prop)) {
        if (arm) {
          indexReads++;
          clock += 20;
        }
        if (!Object.prototype.hasOwnProperty.call(target, prop)) {
          target[prop] = text("w" + prop);
          target[prop].parentNode = wide;
        }
        return target[prop];
      }
      const value = Reflect.get(target, prop, recv);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  wide.childNodes = wideKids;
  const wideLabel = el("LABEL", [wideInput, wide]);
  wideInput.labels = [wideLabel];
  wideInput.id = "wide";
  wideLabel.htmlFor = "";
  const wideDoc = makeDoc([wideLabel]);
  bind(wideDoc, wideLabel);
  arm = true;
  runExtract(wideBounds, wideDoc);
  if (indexReads > 40) {
    fail("budgets: wide label indexed " + indexReads + " children before the time budget");
  }
}

function checkSlots() {
  // Flattened assignedNodes must not materialize a nested slot chain.
  const slotKey = "sighurtSlotTest";
  const slotBounds = {
    nodes: 200,
    depth: 6,
    stringBytes: 200,
    totalBytes: 5000,
    options: 8,
    time: 100000,
    key: slotKey,
  };
  let flattenCalls = 0;
  let directCalls = 0;
  let flattenedNodes = 0;
  const payload = text("slotted-text");
  let inner = null;
  let top = null;
  for (let i = 0; i < 200; i++) {
    const slot = el("SLOT", []);
    slot.inner = inner;
    slot.assignedNodes = function (opt) {
      if (opt && opt.flatten) {
        flattenCalls++;
        let cursor = slot;
        while (cursor) {
          flattenedNodes++;
          cursor = cursor.inner;
        }
        return [payload];
      }
      directCalls++;
      if (slot === top) return [payload, slot.inner];
      if (slot.inner) return [slot.inner];
      return [payload];
    };
    inner = slot;
    top = slot;
  }
  const slotDoc = makeDoc([top]);
  bind(slotDoc, top);
  const slotJson = runExtract(slotBounds, slotDoc);
  if (flattenCalls !== 0 || flattenedNodes > 0) {
    fail(
      "budgets: assignedNodes flatten calls " +
        flattenCalls +
        " materialized " +
        flattenedNodes
    );
  }
  if (directCalls > 20) {
    fail("budgets: slot walk made " + directCalls + " assignedNodes calls (depth cap is 6)");
  }
  if (!String(slotJson).includes("slotted-text") && directCalls < 1) {
    fail("budgets: slot walk did not run");
  }
}

// --- a non-string input type is a password; its value is omitted ---
function checkInputType() {
  const key = "sighurtTypeTest";
  const bounds = {
    nodes: 40,
    depth: 16,
    stringBytes: 200,
    totalBytes: 5000,
    options: 8,
    time: 100000,
    key,
  };
  function field(name, typeGet, value) {
    const input = el("INPUT", []);
    input.attrs["aria-label"] = name;
    input.labels = null;
    Object.defineProperty(input, "type", { configurable: true, get: typeGet });
    Object.defineProperty(input, "value", { configurable: true, get() { return value; } });
    return input;
  }
  const untyped = field("Untyped", () => undefined, "SIGHURT_UNDEFINED_TYPE_SENTINEL");
  const nulled = field("Nulled", () => null, "SIGHURT_NULL_TYPE_SENTINEL");
  const plain = field("Plain", () => "text", "kept-text");
  const secret = field("Secret", () => "password", "SIGHURT_PASSWORD_SENTINEL");
  const doc = makeDoc([untyped, nulled, plain, secret]);
  const json = String(runExtract(bounds, doc));
  let parsed;
  try {
    parsed = JSON.parse(json);
  } catch (e) {
    fail("input type: snapshot is not JSON: " + json.slice(0, 200));
    return;
  }
  const nodes = parsed.nodes || [];
  function byLabel(label) {
    return nodes.find((node) => node.label === label);
  }
  for (const label of ["Untyped", "Nulled"]) {
    const node = byLabel(label);
    if (!node || node.password !== true || node.value != null) {
      fail("input type: " + label + " should be a password without a value, got " + JSON.stringify(node));
    }
  }
  if (json.includes("SIGHURT_UNDEFINED_TYPE_SENTINEL") || json.includes("SIGHURT_NULL_TYPE_SENTINEL")) {
    fail("input type: non-string type leaked a value");
  }
  if (json.includes("SIGHURT_PASSWORD_SENTINEL")) {
    fail("input type: password value leaked");
  }
  const kept = byLabel("Plain");
  if (!kept || kept.password === true || kept.value !== "kept-text") {
    fail("input type: string type=text lost its value: " + JSON.stringify(kept));
  }
}

function installActionRealm() {
  function Element() {}
  Element.prototype.matches = function (sel) {
    if (this.matchesThrows) throw new Error("matches");
    return sel === ":disabled" && this.effectivelyDisabled === true;
  };
  global.Element = Element;
  function HTMLInputElement() {}
  Object.defineProperty(HTMLInputElement.prototype, "value", {
    configurable: true,
    set(v) {
      this.assigned = v;
    },
    get() {
      return this.assigned;
    },
  });
  global.HTMLInputElement = HTMLInputElement;
  function HTMLTextAreaElement() {}
  global.HTMLTextAreaElement = HTMLTextAreaElement;
  global.Event = function Event() {};
}

function control(tag) {
  return {
    nodeType: 1,
    tagName: tag,
    isConnected: true,
    effectivelyDisabled: false,
    matchesThrows: false,
    disabled: false,
    readOnly: false,
    parentNode: null,
    type: "text",
  };
}

function checkActions() {
  installActionRealm();
  const names = ["set_blocked.js", "set_open.js", "set_throw.js", "select_beta.js", "select_gamma.js", "select_alpha.js"];
  const scripts = {};
  for (const name of names) {
    const file = path.join(actionDir, name);
    if (!fs.existsSync(file)) {
      fail("actions: missing " + name);
      return;
    }
    scripts[name] = fs.readFileSync(file, "utf8");
  }
  const keyMatch = scripts["set_blocked.js"].match(/var STATE_KEY = ("(?:\\.|[^"\\])*");/);
  if (!keyMatch) {
    fail("actions: state key missing");
    return;
  }
  const key = JSON.parse(keyMatch[1]);
  const blocked = control("INPUT");
  blocked.effectivelyDisabled = true;
  blocked.disabled = false;
  const open = control("INPUT");
  const throwing = control("INPUT");
  throwing.matchesThrows = true;
  throwing.disabled = false;
  const alpha = { tagName: "OPTION", disabled: false, effectivelyDisabled: false, parentNode: null };
  const beta = { tagName: "OPTION", disabled: true, effectivelyDisabled: true, parentNode: null };
  const group = { tagName: "OPTGROUP", disabled: true, effectivelyDisabled: true };
  const gamma = { tagName: "OPTION", disabled: false, effectivelyDisabled: false, parentNode: group };
  const select = control("SELECT");
  select.options = [alpha, beta, gamma];
  select.selectedIndex = 0;
  select.type = "";
  alpha.parentNode = select;
  beta.parentNode = select;
  const rev = new Map([
    [1, new WeakRef(blocked)],
    [2, new WeakRef(select)],
    [3, new WeakRef(open)],
    [4, new WeakRef(throwing)],
  ]);
  globalThis[key] = { rev };

  function run(name) {
    const value = eval(scripts[name]);
    let parsed;
    try {
      parsed = JSON.parse(value);
    } catch (e) {
      return { ok: false, why: "bad-json", raw: value };
    }
    return parsed;
  }

  const refused = run("set_blocked.js");
  if (refused.ok !== false || refused.why !== "disabled" || blocked.assigned != null) {
    fail("actions: fieldset input was writable: " + JSON.stringify(refused) + " assigned=" + blocked.assigned);
  }
  const threw = run("set_throw.js");
  if (threw.ok !== false || threw.why !== "disabled" || throwing.assigned != null) {
    fail("actions: throwing matches was writable: " + JSON.stringify(threw));
  }
  const opened = run("set_open.js");
  if (opened.ok !== true || open.assigned !== "OPENED") {
    fail("actions: enabled input was not set: " + JSON.stringify(opened) + " assigned=" + open.assigned);
  }
  select.selectedIndex = 0;
  const betaResult = run("select_beta.js");
  if (betaResult.ok !== false || betaResult.why !== "disabled" || select.selectedIndex !== 0) {
    fail("actions: disabled option selected: " + JSON.stringify(betaResult) + " index=" + select.selectedIndex);
  }
  select.selectedIndex = 0;
  const gammaResult = run("select_gamma.js");
  if (gammaResult.ok !== false || gammaResult.why !== "disabled" || select.selectedIndex !== 0) {
    fail("actions: optgroup option selected: " + JSON.stringify(gammaResult) + " index=" + select.selectedIndex);
  }
  select.selectedIndex = 1;
  const alphaResult = run("select_alpha.js");
  if (alphaResult.ok !== true || select.selectedIndex !== 0) {
    fail("actions: enabled option not selected: " + JSON.stringify(alphaResult) + " index=" + select.selectedIndex);
  }
}

checkReverseMap();
checkLabelledBy();
checkOptionText();
checkDeepLabel();
checkWideLabel();
checkSlots();
checkInputType();
checkActions();

if (failed) {
  console.error(failed + " failure(s)");
  process.exit(1);
}
console.log("harden ok");
