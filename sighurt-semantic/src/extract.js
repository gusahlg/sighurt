/*
 * Sighurt DOM extraction. This file is an expression: its value is a JSON string.
 * Rust replaces each placeholder token from sighurt_api::semantic and from the
 * per-process slot name. Adapters evaluate it in one JavaScript world and evaluate
 * action scripts in that same world.
 *
 * One node per content element, parent before children. Ids come from a WeakMap and
 * are never reused. A Map of WeakRefs lives on a non-enumerable property of globalThis
 * so a later action can find the node.
 *
 * Skipped: script, style, template, noscript, head metadata, hidden, aria-hidden=true,
 * display:none, and input type=hidden. visibility:hidden skips that element but not a
 * visible descendant. checkVisibility() is used when it exists, otherwise getComputedStyle.
 * Same-origin iframes are walked. A cross-origin iframe is a container labelled with its
 * absolute URL. Open shadow roots are walked and slots pull assigned light-DOM nodes.
 * A closed shadow root is not visible to script.
 *
 * Outside pre, whitespace runs collapse to one space and a block's direct text children
 * lose their edge spaces. pre keeps its text. Text is cut on a UTF-8 boundary at
 * MAX_STRING_BYTES. An href that is longer than that, or that does not fit the remaining
 * total, is dropped whole. The walk stops, and sets truncated, when the node count, the
 * depth, the total bytes or the time budget is exhausted. Option lists stop at MAX_OPTIONS.
 *
 * Password field values are never read. The flag is set and the value is left out.
 */
(function () {
  "use strict";
  var MAX_NODES = __SIGHURT_MAX_NODES__;
  var MAX_DEPTH = __SIGHURT_MAX_DEPTH__;
  var MAX_STRING_BYTES = __SIGHURT_MAX_STRING_BYTES__;
  var MAX_TOTAL_BYTES = __SIGHURT_MAX_TOTAL_BYTES__;
  var MAX_OPTIONS = __SIGHURT_MAX_OPTIONS__;
  var TIME_BUDGET_MS = __SIGHURT_TIME_BUDGET_MS__;
  var STATE_KEY = __SIGHURT_STATE_KEY__;

  var SKIP = {
    SCRIPT: 1, STYLE: 1, TEMPLATE: 1, NOSCRIPT: 1, LINK: 1, META: 1, BASE: 1,
    TITLE: 1, HEAD: 1, SOURCE: 1
  };
  var INLINE = {
    SPAN: 1, B: 1, STRONG: 1, I: 1, EM: 1, U: 1, S: 1, STRIKE: 1, SMALL: 1, BIG: 1,
    MARK: 1, SUB: 1, SUP: 1, FONT: 1, ABBR: 1, CITE: 1, TIME: 1, BDI: 1, BDO: 1,
    WBR: 1, DATA: 1, DFN: 1, KBD: 1, SAMP: 1, VAR: 1, RUBY: 1, RT: 1, RP: 1, INS: 1,
    DEL: 1, Q: 1, NOBR: 1, LABEL: 1, PICTURE: 1
  };
  var FLAT = { HTML: 1, BODY: 1, THEAD: 1, TBODY: 1, TFOOT: 1, COLGROUP: 1, COL: 1 };

  function now() {
    if (typeof performance !== "undefined" && performance && typeof performance.now === "function") {
      return performance.now();
    }
    return Date.now();
  }

  function loadState(key) {
    var existing = globalThis[key];
    if (existing && existing.ids && existing.rev && typeof existing.next === "number") return existing;
    var created = { ids: new WeakMap(), rev: new Map(), next: 1 };
    try {
      Object.defineProperty(globalThis, key, {
        value: created, enumerable: false, writable: false, configurable: false
      });
    } catch (e) { /* the page won the race on this unguessable name */ }
    return globalThis[key] || created;
  }

  function prune(state) {
    var dead = [];
    state.rev.forEach(function (ref, id) {
      var node = null;
      try { node = ref.deref(); } catch (e) { node = null; }
      if (!node) dead.push(id);
    });
    for (var i = 0; i < dead.length; i++) state.rev.delete(dead[i]);
  }

  function fallback(state) {
    var id = 1;
    try {
      if (state && state.ids && typeof document !== "undefined") {
        var existing = state.ids.get(document);
        if (existing) id = existing;
      }
    } catch (e) { id = 1; }
    return JSON.stringify({
      truncated: true, root: id, focus: null,
      nodes: [{ id: id, kind: "document" }]
    });
  }

  function tagOf(el) {
    var tag = el && el.tagName;
    return tag ? String(tag).toUpperCase() : "";
  }

  function attr(el, name) {
    try { return el.getAttribute(name); } catch (e) { return null; }
  }

  function inputType(el) {
    try { return String(el.type || "text").toLowerCase(); } catch (e) { return "text"; }
  }

  function hrefOf(el) {
    var href = attr(el, "href");
    if (href) return href;
    return attr(el, "xlink:href");
  }

  function isWs(code) {
    return code === 32 || code === 9 || code === 10 || code === 13 || code === 12 || code === 11
      || code === 160 || code === 0x1680 || (code >= 0x2000 && code <= 0x200A)
      || code === 0x2028 || code === 0x2029 || code === 0x202F || code === 0x205F
      || code === 0x3000 || code === 0xFEFF;
  }

  function utf8len(text) {
    var n = 0;
    for (var i = 0; i < text.length; i++) {
      var code = text.charCodeAt(i);
      if (code < 0x80) n += 1;
      else if (code < 0x800) n += 2;
      else if (code >= 0xD800 && code <= 0xDBFF) { n += 4; i++; }
      else n += 3;
    }
    return n;
  }

  function collapseWs(text) {
    var out = [];
    var prev = false;
    for (var i = 0; i < text.length; i++) {
      var code = text.charCodeAt(i);
      if (isWs(code)) {
        if (!prev) { out.push(" "); prev = true; }
      } else {
        prev = false;
        out.push(text.charAt(i));
        if (code >= 0xD800 && code <= 0xDBFF) {
          i++;
          if (i < text.length) out.push(text.charAt(i));
        }
      }
    }
    return out.join("");
  }

  var state = null;
  var nodes = [];
  var byId = Object.create(null);
  var used = 0;
  var truncated = false;
  var started = 0;
  var seenDocs = typeof WeakSet === "function" ? new WeakSet() : null;
  var emitted = typeof WeakSet === "function" ? new WeakSet() : null;

  function overBudget() {
    if (nodes.length >= MAX_NODES) { truncated = true; return true; }
    if (used >= MAX_TOTAL_BYTES) { truncated = true; return true; }
    if (now() - started > TIME_BUDGET_MS) { truncated = true; return true; }
    return false;
  }

  function unbudget(text) {
    if (!text) return;
    used -= utf8len(text);
    if (used < 0) used = 0;
  }

  // Copy `raw`, collapsing whitespace when asked, replacing lone surrogates, and stopping
  // at the per-string cap and at whatever remains of the total. Sets truncated when the
  // source does not fit. Adds the kept bytes to `used`.
  function takeDomText(raw, collapse) {
    if (!raw) return "";
    var room = MAX_TOTAL_BYTES - used;
    if (room < 0) room = 0;
    var limit = MAX_STRING_BYTES < room ? MAX_STRING_BYTES : room;
    var out = [];
    var bytes = 0;
    var prevSpace = false;
    var i = 0;
    var cut = false;
    while (i < raw.length) {
      var code = raw.charCodeAt(i);
      var piece;
      var add;
      var step = 1;
      if (code >= 0xD800 && code <= 0xDBFF) {
        var next = i + 1 < raw.length ? raw.charCodeAt(i + 1) : 0;
        if (next >= 0xDC00 && next <= 0xDFFF) {
          piece = raw.charAt(i) + raw.charAt(i + 1);
          add = 4;
          step = 2;
        } else {
          piece = "\uFFFD";
          add = 3;
        }
      } else if (code >= 0xDC00 && code <= 0xDFFF) {
        piece = "\uFFFD";
        add = 3;
      } else if (code < 0x80) {
        piece = raw.charAt(i);
        add = 1;
      } else if (code < 0x800) {
        piece = raw.charAt(i);
        add = 2;
      } else {
        piece = raw.charAt(i);
        add = 3;
      }
      if (collapse && step === 1 && isWs(code)) {
        if (prevSpace) { i += step; continue; }
        piece = " ";
        add = 1;
        prevSpace = true;
      } else {
        prevSpace = false;
      }
      if (bytes + add > limit) { cut = true; break; }
      out.push(piece);
      bytes += add;
      i += step;
    }
    if (i < raw.length) cut = true;
    if (cut) truncated = true;
    used += bytes;
    return out.join("");
  }

  function fitLabel(text) {
    if (!text) return null;
    var collapsed = collapseWs(String(text)).replace(/^ /, "").replace(/ $/, "");
    if (!collapsed) return null;
    var kept = takeDomText(collapsed, false);
    return kept || null;
  }

  function fitHref(absolute) {
    if (!absolute) return null;
    var bytes = utf8len(absolute);
    if (bytes > MAX_STRING_BYTES || used + bytes > MAX_TOTAL_BYTES) {
      truncated = true;
      return null;
    }
    used += bytes;
    return absolute;
  }

  function absoluteUrl(raw, doc) {
    if (!raw) return null;
    try {
      var base = doc && doc.baseURI ? doc.baseURI : document.baseURI;
      return new URL(raw, base).href;
    } catch (e) { return null; }
  }

  function idOf(node) {
    var existing = state.ids.get(node);
    if (existing !== undefined) return existing;
    var id = state.next++;
    state.ids.set(node, id);
    try {
      state.rev.set(id, new WeakRef(node));
    } catch (e) {
      state.rev.set(id, { deref: function () { return node; } });
    }
    return id;
  }

  function pushNode(obj) {
    if (nodes.length >= MAX_NODES) { truncated = true; return false; }
    nodes.push(obj);
    byId[obj.id] = obj;
    return true;
  }

  function removeNode(id) {
    var obj = byId[id];
    if (!obj) return;
    unbudget(obj.text);
    delete byId[id];
    for (var i = nodes.length - 1; i >= 0; i--) {
      if (nodes[i].id === id) { nodes.splice(i, 1); break; }
    }
  }

  function hiddenKind(el) {
    if (el.hasAttribute && (el.hasAttribute("hidden") || el.getAttribute("aria-hidden") === "true")) {
      return "subtree";
    }
    var view = null;
    try { view = el.ownerDocument && el.ownerDocument.defaultView; } catch (e) { view = null; }
    if (typeof el.checkVisibility === "function") {
      var shown = true;
      try {
        shown = el.checkVisibility({
          checkOpacity: false,
          checkVisibilityCSS: true,
          contentVisibilityAuto: false,
          opacityProperty: false,
          visibilityProperty: true
        });
      } catch (e2) { shown = true; }
      if (!shown) {
        var vis = "visible";
        var display = "";
        try {
          if (view && view.getComputedStyle) {
            var style = view.getComputedStyle(el);
            if (style) { vis = style.visibility; display = style.display; }
          }
        } catch (e3) { /* fall through to hiding the subtree */ }
        if (vis === "hidden" && display !== "none") return "self";
        return "subtree";
      }
      return "show";
    }
    if (view && view.getComputedStyle) {
      try {
        var style2 = view.getComputedStyle(el);
        if (style2 && style2.display === "none") return "subtree";
        if (style2 && style2.visibility === "hidden") return "self";
      } catch (e4) { /* treat as shown */ }
    }
    return "show";
  }

  function childNodesOf(el) {
    if (el && el.nodeType === 1 && el.shadowRoot) return el.shadowRoot.childNodes;
    return (el && el.childNodes) || [];
  }

  function roleOf(el) {
    var role = attr(el, "role");
    if (!role) return "";
    var parts = String(role).toLowerCase().split(/\s+/);
    return parts[0] || "";
  }

  function namedRegion(el) {
    var aria = attr(el, "aria-label");
    if (aria) return aria;
    var ids = attr(el, "aria-labelledby");
    if (!ids) return "";
    var doc = el.ownerDocument || document;
    var parts = String(ids).split(/\s+/);
    var out = [];
    var cap = parts.length < 8 ? parts.length : 8;
    for (var i = 0; i < cap; i++) {
      if (!parts[i]) continue;
      var target = null;
      try { target = doc.getElementById(parts[i]); } catch (e) { target = null; }
      if (!target) continue;
      var text = "";
      try { text = target.textContent || ""; } catch (e2) { text = ""; }
      if (text) out.push(text);
      if (out.join("").length > MAX_STRING_BYTES) break;
    }
    return out.join(" ");
  }

  function roleKind(role, el) {
    if (role === "presentation" || role === "none") return { kind: "flatten" };
    if (role === "banner") return { kind: "landmark", landmark: "banner" };
    if (role === "navigation") return { kind: "landmark", landmark: "navigation" };
    if (role === "main") return { kind: "landmark", landmark: "main" };
    if (role === "complementary") return { kind: "landmark", landmark: "complementary" };
    if (role === "contentinfo") return { kind: "landmark", landmark: "contentinfo" };
    if (role === "search") return { kind: "landmark", landmark: "search" };
    if (role === "form") return { kind: "landmark", landmark: "form" };
    if (role === "region") return { kind: "landmark", landmark: "region" };
    if (role === "heading") {
      var level = parseInt(attr(el, "aria-level"), 10);
      if (!(level >= 1 && level <= 6)) level = 2;
      return { kind: "heading", level: level };
    }
    if (role === "link") return { kind: "link" };
    if (role === "button") return { kind: "button" };
    if (role === "textbox") return { kind: "textfield" };
    if (role === "checkbox") return { kind: "checkbox" };
    if (role === "radio") return { kind: "radio" };
    if (role === "list") return { kind: "list" };
    if (role === "listitem") return { kind: "listitem" };
    if (role === "img" || role === "image") return { kind: "image" };
    if (role === "separator") return { kind: "separator" };
    if (role === "paragraph") return { kind: "paragraph" };
    if (role === "table" || role === "grid") return { kind: "table" };
    if (role === "row") return { kind: "row" };
    if (role === "cell" || role === "gridcell" || role === "columnheader" || role === "rowheader") {
      return { kind: "cell" };
    }
    return null;
  }

  function classifyInput(el) {
    var type = inputType(el);
    if (type === "hidden") return { kind: "skip" };
    if (type === "checkbox") return { kind: "checkbox" };
    if (type === "radio") return { kind: "radio" };
    if (type === "button" || type === "submit" || type === "reset" || type === "image") {
      return { kind: "button" };
    }
    if (type === "file") return { kind: "textfield", file: true };
    if (type === "password") return { kind: "textfield", password: true };
    return { kind: "textfield" };
  }

  function classify(el, sectioning) {
    var tag = tagOf(el);
    if (tag === "INPUT") return classifyInput(el);
    if (tag === "TEXTAREA") return { kind: "textarea" };
    if (tag === "SELECT") return { kind: "select" };
    if (tag === "IMG") return { kind: "image" };
    if (tag === "IFRAME" || tag === "FRAME") return { kind: "iframe" };
    if (tag === "BUTTON") return { kind: "button" };
    var byRole = roleKind(roleOf(el), el);
    if (byRole) return byRole;
    if (tag === "A" || tag === "AREA") {
      if (hrefOf(el)) return { kind: "link" };
      return { kind: "flatten" };
    }
    if (tag === "H1" || tag === "H2" || tag === "H3" || tag === "H4" || tag === "H5" || tag === "H6") {
      var level = tag.charCodeAt(1) - 48;
      var ariaLevel = parseInt(attr(el, "aria-level"), 10);
      if (ariaLevel >= 1 && ariaLevel <= 6) level = ariaLevel;
      return { kind: "heading", level: level };
    }
    if (tag === "P" || tag === "FIGCAPTION" || tag === "CAPTION" || tag === "SUMMARY") {
      return { kind: "paragraph" };
    }
    if (tag === "UL" || tag === "OL" || tag === "MENU" || tag === "DL") return { kind: "list" };
    if (tag === "LI" || tag === "DT" || tag === "DD") return { kind: "listitem" };
    if (tag === "TABLE") return { kind: "table" };
    if (tag === "TR") return { kind: "row" };
    if (tag === "TD" || tag === "TH") return { kind: "cell" };
    if (tag === "PRE") return { kind: "pre" };
    if (tag === "CODE") return { kind: "code" };
    if (tag === "BLOCKQUOTE") return { kind: "quote" };
    if (tag === "HR") return { kind: "separator" };
    if (tag === "HEADER") {
      return sectioning ? { kind: "container" } : { kind: "landmark", landmark: "banner" };
    }
    if (tag === "FOOTER") {
      return sectioning ? { kind: "container" } : { kind: "landmark", landmark: "contentinfo" };
    }
    if (tag === "NAV") return { kind: "landmark", landmark: "navigation" };
    if (tag === "MAIN") return { kind: "landmark", landmark: "main" };
    if (tag === "ASIDE") return { kind: "landmark", landmark: "complementary" };
    if (tag === "FORM") return { kind: "landmark", landmark: "form" };
    if (tag === "SEARCH") return { kind: "landmark", landmark: "search" };
    if (tag === "SECTION" || tag === "ARTICLE") {
      var name = namedRegion(el);
      if (name) return { kind: "landmark", landmark: "region", label: name };
      return { kind: "container" };
    }
    if (INLINE[tag] || FLAT[tag]) return { kind: "flatten" };
    return { kind: "container" };
  }

  function establishes(tag, info) {
    if (tag === "ARTICLE" || tag === "ASIDE" || tag === "MAIN" || tag === "NAV" || tag === "SECTION") {
      return true;
    }
    var mark = info && info.landmark;
    return mark === "main" || mark === "navigation" || mark === "complementary" || mark === "region";
  }

  function isBlock(kind) {
    return kind === "heading" || kind === "paragraph" || kind === "list" || kind === "listitem"
      || kind === "table" || kind === "row" || kind === "cell" || kind === "quote"
      || kind === "landmark" || kind === "container" || kind === "pre";
  }

  function isLeaf(kind) {
    return kind === "textfield" || kind === "textarea" || kind === "select"
      || kind === "checkbox" || kind === "radio" || kind === "image" || kind === "separator";
  }

  function labelText(label, control) {
    var parts = [];
    var count = 0;
    function rec(node) {
      if (!node || node === control || count > MAX_STRING_BYTES) return;
      if (node.nodeType === 3) {
        parts.push(node.nodeValue || "");
        count += (node.nodeValue || "").length;
        return;
      }
      if (node.nodeType !== 1 || SKIP[tagOf(node)]) return;
      var kids = node.childNodes || [];
      for (var i = 0; i < kids.length; i++) rec(kids[i]);
    }
    rec(label);
    return parts.join("");
  }

  // Label element (`for`, otherwise the wrapping label), then aria-label, placeholder, name.
  function fieldLabel(el) {
    var list = null;
    try { list = el.labels; } catch (e) { list = null; }
    if (list && list.length) {
      var i;
      var lab;
      if (el.id) {
        for (i = 0; i < list.length; i++) {
          lab = list[i];
          if (lab && lab.htmlFor === el.id) return labelText(lab, el);
        }
      }
      for (i = 0; i < list.length; i++) {
        lab = list[i];
        if (lab && !lab.htmlFor) return labelText(lab, el);
      }
    }
    var aria = attr(el, "aria-label");
    if (aria) return aria;
    var placeholder = attr(el, "placeholder");
    if (placeholder) return placeholder;
    return attr(el, "name") || "";
  }

  function isDisabled(el) {
    try { if (el.disabled) return true; } catch (e) { /* not a form control */ }
    return attr(el, "aria-disabled") === "true";
  }

  function inputCaption(el) {
    if (inputType(el) === "image") return attr(el, "alt") || "";
    try { return el.value == null ? "" : String(el.value); } catch (e) { return attr(el, "value") || ""; }
  }

  function fillControl(el, obj, info) {
    var kind = info.kind;
    if (kind === "button" || kind === "textfield" || kind === "textarea" || kind === "select"
      || kind === "checkbox" || kind === "radio") {
      obj.disabled = isDisabled(el);
    }
    if (kind === "checkbox" || kind === "radio") {
      if (tagOf(el) === "INPUT") {
        try { obj.checked = !!el.checked; } catch (e) { obj.checked = false; }
      } else {
        var ariaChecked = attr(el, "aria-checked");
        obj.checked = ariaChecked === "true" || ariaChecked === "mixed";
      }
    }
    if (kind === "textfield" || kind === "textarea" || kind === "checkbox" || kind === "radio" || kind === "select") {
      var lab = fitLabel(fieldLabel(el));
      if (lab) obj.label = lab;
    }
    if (kind === "textfield" || kind === "textarea") {
      if (info.password) {
        // Never read el.value for a password field.
        obj.password = true;
      } else if (!info.file) {
        var raw = "";
        try { raw = el.value == null ? "" : String(el.value); } catch (e2) { raw = ""; }
        var kept = takeDomText(raw, false);
        if (kept) obj.value = kept;
      }
    }
    if (kind === "select") fillSelect(el, obj);
    if (kind === "image") {
      var alt = attr(el, "aria-label");
      if (alt == null || alt === "") alt = attr(el, "alt");
      var altText = fitLabel(alt);
      if (altText) obj.text = altText;
    }
    if (kind === "link" || kind === "button") {
      var rawHref = hrefOf(el);
      if (rawHref) {
        var fitted = fitHref(absoluteUrl(rawHref, el.ownerDocument || document));
        if (fitted) obj.href = fitted;
      }
    }
    if (kind === "landmark") {
      var name = fitLabel(info.label || namedRegion(el));
      if (name) obj.label = name;
    }
  }

  function fillSelect(el, obj) {
    var options = [];
    try { options = el.options || []; } catch (e) { options = []; }
    var out = [];
    var limit = options.length;
    if (limit > MAX_OPTIONS) { limit = MAX_OPTIONS; truncated = true; }
    for (var i = 0; i < limit; i++) {
      if (used >= MAX_TOTAL_BYTES) { truncated = true; break; }
      var text = "";
      try { text = options[i].text || options[i].label || ""; } catch (e2) { text = ""; }
      text = collapseWs(String(text)).replace(/^ /, "").replace(/ $/, "");
      out.push(text ? takeDomText(text, false) : "");
    }
    obj.options = out;
    var selected = -1;
    try { selected = el.selectedIndex; } catch (e3) { selected = -1; }
    if (typeof selected === "number" && selected >= 0 && selected < out.length) obj.selected = selected;
    else if (typeof selected === "number" && selected >= out.length) truncated = true;
  }

  function foldButton(obj) {
    var parts = [];
    var kept = [];
    var children = obj.children || [];
    for (var i = 0; i < children.length; i++) {
      var child = byId[children[i]];
      if (child && child.kind === "text") {
        if (child.text) parts.push(child.text);
        removeNode(child.id);
      } else if (child) {
        kept.push(children[i]);
      }
    }
    obj.children = kept;
    var text = parts.join("").replace(/^ +/, "").replace(/ +$/, "");
    if (!text) return;
    var keptText = takeDomText(text, false);
    if (keptText) obj.text = keptText;
  }

  function stripEdge(ids, leading) {
    if (!ids.length) return ids;
    var index = leading ? 0 : ids.length - 1;
    var obj = byId[ids[index]];
    if (!obj || obj.kind !== "text" || typeof obj.text !== "string") return ids;
    var text = obj.text;
    var next = leading ? text.replace(/^ +/, "") : text.replace(/ +$/, "");
    if (next === text) return ids;
    if (!next) {
      ids.splice(index, 1);
      removeNode(obj.id);
      return ids;
    }
    unbudget(text);
    obj.text = next;
    used += utf8len(next);
    return ids;
  }

  function trimEdges(ids) {
    ids = stripEdge(ids, true);
    return stripEdge(ids, false);
  }

  function walkList(list, depth, inPre, sectioning, skipText) {
    var ids = [];
    var count = list ? list.length : 0;
    for (var i = 0; i < count; i++) {
      if (overBudget()) break;
      var part = walk(list[i], depth, inPre, sectioning, skipText);
      for (var j = 0; j < part.length; j++) ids.push(part[j]);
    }
    return ids;
  }

  function walkText(node, inPre) {
    if (emitted && emitted.has(node)) return [];
    var raw = node.nodeValue;
    if (!raw) return [];
    var text = takeDomText(raw, !inPre);
    if (!text) return [];
    var obj = { id: idOf(node), kind: "text", text: text };
    if (emitted) emitted.add(node);
    if (!pushNode(obj)) { unbudget(text); return []; }
    return [obj.id];
  }

  function emitNewline(el) {
    if (emitted && emitted.has(el)) return [];
    var text = takeDomText("\n", false);
    if (!text) return [];
    var obj = { id: idOf(el), kind: "text", text: text };
    if (emitted) emitted.add(el);
    if (!pushNode(obj)) { unbudget(text); return []; }
    return [obj.id];
  }

  function walkDocument(doc, parentDepth, skipText) {
    if (!doc) return [];
    if (seenDocs) {
      if (seenDocs.has(doc)) { truncated = true; return []; }
      seenDocs.add(doc);
    }
    var html = null;
    try { html = doc.documentElement; } catch (e) { html = null; }
    if (!html) return [];
    return walk(html, parentDepth + 1, false, false, skipText);
  }

  function walkElement(el, depth, inPre, sectioning, skipText) {
    if (emitted && emitted.has(el)) return [];
    var tag = tagOf(el);
    if (SKIP[tag]) return [];
    if (tag === "INPUT" && inputType(el) === "hidden") return [];
    var hide = hiddenKind(el);
    if (hide === "subtree") return [];
    if (hide === "self") return walkList(childNodesOf(el), depth, inPre, sectioning, skipText);
    if (tag === "SLOT" && typeof el.assignedNodes === "function") {
      var assigned = [];
      try { assigned = el.assignedNodes({ flatten: true }); } catch (e) { assigned = []; }
      if (assigned && assigned.length) return walkList(assigned, depth, inPre, sectioning, skipText);
      return walkList(el.childNodes || [], depth, inPre, sectioning, skipText);
    }
    if (tag === "BR") {
      if (skipText) return [];
      return emitNewline(el);
    }
    var info = classify(el, sectioning);
    if (info.kind === "skip") return [];
    if (info.kind === "flatten") return walkList(childNodesOf(el), depth, inPre, sectioning, skipText);

    if (info.kind === "iframe") {
      var frame = { id: idOf(el), kind: "container", children: [] };
      var frameLabel = null;
      var src = attr(el, "src");
      if (!src) { try { src = el.src || ""; } catch (e2) { src = ""; } }
      if (src) frameLabel = fitHref(absoluteUrl(src, el.ownerDocument || document));
      if (frameLabel) frame.label = frameLabel;
      if (emitted) emitted.add(el);
      if (!pushNode(frame)) return [];
      var inner = null;
      try { inner = el.contentDocument; } catch (e3) { inner = null; }
      if (inner) {
        frame.children = trimEdges(walkDocument(inner, depth, skipText));
      }
      return [frame.id];
    }

    var obj = { id: idOf(el), kind: info.kind, children: [] };
    if (info.level) obj.level = info.level;
    if (info.landmark) obj.landmark = info.landmark;
    fillControl(el, obj, info);
    if (emitted) emitted.add(el);
    if (!pushNode(obj)) return [];

    var childDepth = depth + 1;
    var childPre = inPre || info.kind === "pre";
    var childSection = sectioning || establishes(tag, info);
    if (!isLeaf(info.kind)) {
      var skipChildText = skipText || (info.kind === "button" && !!attr(el, "aria-label"));
      obj.children = walkList(childNodesOf(el), childDepth, childPre, childSection, skipChildText);
      if (!childPre && isBlock(info.kind)) obj.children = trimEdges(obj.children);
      if (info.kind === "button") {
        if (attr(el, "aria-label")) {
          var ariaCap = fitLabel(attr(el, "aria-label"));
          if (ariaCap) obj.text = ariaCap;
        } else if (tag === "INPUT") {
          var inputCap = fitLabel(inputCaption(el));
          if (inputCap) obj.text = inputCap;
        } else {
          foldButton(obj);
        }
      }
    }
    return [obj.id];
  }

  function walk(node, depth, inPre, sectioning, skipText) {
    if (!node || overBudget()) return [];
    if (depth > MAX_DEPTH) { truncated = true; return []; }
    var type = node.nodeType;
    if (type === 3 || type === 4) {
      if (skipText) return [];
      return walkText(node, inPre);
    }
    if (type === 11) return walkList(node.childNodes, depth, inPre, sectioning, skipText);
    if (type !== 1) return [];
    return walkElement(node, depth, inPre, sectioning, skipText);
  }

  function focusId(doc) {
    var el = null;
    try { el = doc.activeElement; } catch (e) { el = null; }
    var guard = 0;
    while (el && guard < 32) {
      guard++;
      var next = null;
      try {
        if (el.shadowRoot && el.shadowRoot.activeElement) next = el.shadowRoot.activeElement;
        else if ((el.tagName === "IFRAME" || el.tagName === "FRAME") && el.contentDocument) {
          var inner = el.contentDocument.activeElement;
          if (inner && inner !== el.contentDocument.body && inner !== el.contentDocument.documentElement) {
            next = inner;
          }
        }
      } catch (e2) { next = null; }
      if (!next || next === el) break;
      el = next;
    }
    if (!el || el === doc.body || el === doc.documentElement || el === doc) return null;
    var id = state.ids.get(el);
    if (id === undefined || !byId[id]) return null;
    return id;
  }

  function publish(obj) {
    var out = { id: obj.id, kind: obj.kind };
    if (obj.level) out.level = obj.level;
    if (obj.landmark) out.landmark = obj.landmark;
    if (obj.children && obj.children.length) out.children = obj.children;
    if (obj.text) out.text = obj.text;
    if (obj.href) out.href = obj.href;
    if (obj.value) out.value = obj.value;
    if (obj.label) out.label = obj.label;
    if (obj.checked === true || obj.checked === false) out.checked = obj.checked;
    if (obj.options) out.options = obj.options;
    if (obj.selected === 0 || obj.selected) out.selected = obj.selected;
    if (obj.disabled) out.disabled = true;
    if (obj.password) out.password = true;
    return out;
  }

  try {
    state = loadState(STATE_KEY);
    prune(state);
    started = now();
    var root = { id: idOf(document), kind: "document", children: [] };
    if (emitted) emitted.add(document);
    pushNode(root);
    root.children = trimEdges(walkDocument(document, 0, false));
    var focus = focusId(document);
    var published = [];
    for (var i = 0; i < nodes.length; i++) published.push(publish(nodes[i]));
    return JSON.stringify({ truncated: truncated, root: root.id, focus: focus, nodes: published });
  } catch (e) {
    return fallback(state);
  }
})()
