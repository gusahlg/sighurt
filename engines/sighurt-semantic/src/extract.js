/*
 * Sighurt DOM extraction. This file is an expression: its value is a JSON string.
 * Rust replaces each placeholder token from sighurt_api::semantic and from the
 * per-process slot name. Adapters evaluate it in one JavaScript world and evaluate
 * action scripts in that same world.
 *
 * One node per content element, parent before children. Ids come from a WeakMap and
 * are never recycled. A Map of WeakRefs, capped at twice MAX_NODES, lives on a
 * non-enumerable property of globalThis so a later action can find the node. There
 * is no strong-reference fallback: if WeakRef is missing, actions miss.
 *
 * The walk is an explicit stack. It counts DOM depth, including elements that are
 * flattened, and it shares one budget for visited nodes, children of one element,
 * and characters read from the page. Label, text and option helpers use that
 * budget. Past a budget the snapshot is truncated; it is still JSON.
 *
 * Records are null-prototype objects. The JSON is assembled by hand and strings
 * are escaped with the JSON.stringify captured at the start, so a page that
 * pollutes Object.prototype or Array.prototype cannot put toJSON into the output.
 *
 * Skipped: script, style, template, noscript, head metadata, hidden, aria-hidden=true,
 * display:none, and input type=hidden. visibility:hidden skips that element and its own
 * text, but not a visible descendant. checkVisibility() is used when it exists, otherwise
 * getComputedStyle.
 * Same-origin iframes are walked. A cross-origin iframe is a container labelled with its
 * absolute URL. Open shadow roots are walked and slots pull assigned light-DOM nodes.
 * A closed shadow root is not visible to script.
 *
 * Outside pre, whitespace runs collapse to one space and a block's direct text children
 * lose their edge spaces. pre keeps its text. Text is cut on a UTF-8 boundary at
 * MAX_STRING_BYTES. An href that is longer than that, or that does not fit the remaining
 * total, is dropped whole. The walk stops, and sets truncated, when a budget is exhausted.
 * Option lists stop at MAX_OPTIONS.
 *
 * Password field values are never read. A type that cannot be read is a password.
 * The flag is set and the value is left out.
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
  var MAX_VISITS = MAX_NODES * 8;
  var MAX_CHILDREN = MAX_NODES;
  var REV_CAP = MAX_NODES * 2;

  var create = Object.create;
  var getOwn = Object.getOwnPropertyDescriptor;
  var defineProp = Object.defineProperty;
  var stringify = JSON.stringify;
  var wmGet = WeakMap.prototype.get;
  var wmSet = WeakMap.prototype.set;
  var wmDelete = WeakMap.prototype.delete;
  var mapGet = Map.prototype.get;
  var mapSet = Map.prototype.set;
  var mapDelete = Map.prototype.delete;
  var mapForEach = Map.prototype.forEach;
  var arrJoin = Array.prototype.join;
  var strSlice = String.prototype.slice;
  var floor = Math.floor;

  var SKIP = create(null);
  SKIP.SCRIPT = 1; SKIP.STYLE = 1; SKIP.TEMPLATE = 1; SKIP.NOSCRIPT = 1; SKIP.LINK = 1;
  SKIP.META = 1; SKIP.BASE = 1; SKIP.TITLE = 1; SKIP.HEAD = 1; SKIP.SOURCE = 1;
  var INLINE = create(null);
  INLINE.SPAN = 1; INLINE.B = 1; INLINE.STRONG = 1; INLINE.I = 1; INLINE.EM = 1;
  INLINE.U = 1; INLINE.S = 1; INLINE.STRIKE = 1; INLINE.SMALL = 1; INLINE.BIG = 1;
  INLINE.MARK = 1; INLINE.SUB = 1; INLINE.SUP = 1; INLINE.FONT = 1; INLINE.ABBR = 1;
  INLINE.CITE = 1; INLINE.TIME = 1; INLINE.BDI = 1; INLINE.BDO = 1; INLINE.WBR = 1;
  INLINE.DATA = 1; INLINE.DFN = 1; INLINE.KBD = 1; INLINE.SAMP = 1; INLINE.VAR = 1;
  INLINE.RUBY = 1; INLINE.RT = 1; INLINE.RP = 1; INLINE.INS = 1; INLINE.DEL = 1;
  INLINE.Q = 1; INLINE.NOBR = 1; INLINE.LABEL = 1; INLINE.PICTURE = 1;
  var FLAT = create(null);
  FLAT.HTML = 1; FLAT.BODY = 1; FLAT.THEAD = 1; FLAT.TBODY = 1; FLAT.TFOOT = 1;
  FLAT.COLGROUP = 1; FLAT.COL = 1;

  var perf = (typeof performance !== "undefined") ? performance : null;
  var perfNow = perf && perf.now;
  var dateNow = Date.now;
  function now() {
    if (perf && typeof perfNow === "function") {
      try { return perfNow.call(perf); } catch (e) { /* fall through */ }
    }
    try { return dateNow.call(Date); } catch (e2) { return 0; }
  }

  function blank() { return create(null); }

  function join(parts) {
    try { return arrJoin.call(parts, ""); } catch (e) { return ""; }
  }

  function slice(text, start, end) {
    try { return strSlice.call(text, start, end); } catch (e) { return ""; }
  }

  function asString(value) {
    if (typeof value === "string") return value;
    if (value == null) return "";
    try { return String(value); } catch (e) { return ""; }
  }

  function lowerAscii(text) {
    var out = "";
    for (var i = 0; i < text.length; i++) {
      var code = text.charCodeAt(i);
      if (code >= 65 && code <= 90) out += String.fromCharCode(code + 32);
      else out += text.charAt(i);
    }
    return out;
  }

  function jsNum(n) {
    if (typeof n !== "number" || n !== n || n === Infinity || n === -Infinity || n < 0) return "0";
    if (n > 9007199254740991) n = 9007199254740991;
    n = floor(n);
    if (n === 0) return "0";
    var digits = "0123456789";
    var s = "";
    var guard = 0;
    while (n >= 1 && guard < 20) {
      var d = floor(n % 10);
      s = digits.charAt(d) + s;
      n = floor(n / 10);
      guard++;
    }
    return s || "0";
  }

  function jsString(text) {
    return stringify(asString(text));
  }

  function loadState(key) {
    var slot = null;
    try { slot = getOwn(globalThis, key); } catch (e) { slot = null; }
    var existing = slot && slot.value;
    if (existing && existing.ids && existing.rev && typeof existing.next === "number") {
      if (!existing.order) {
        existing.order = [];
        existing.orderAt = 0;
        existing.nrev = 0;
        try {
          mapForEach.call(existing.rev, function (ref, id) {
            var weak = false;
            try { weak = typeof WeakRef === "function" && ref instanceof WeakRef; }
            catch (err) { weak = false; }
            if (!weak) {
              try { mapDelete.call(existing.rev, id); } catch (drop) { /* leave it */ }
              return;
            }
            existing.order[existing.order.length] = id;
            existing.nrev++;
          });
        } catch (walk) { /* the map is not iterable; actions miss */ }
      }
      if (typeof existing.orderAt !== "number") existing.orderAt = 0;
      if (typeof existing.nrev !== "number") existing.nrev = 0;
      return existing;
    }
    var created = blank();
    created.ids = new WeakMap();
    created.rev = new Map();
    created.next = 1;
    created.order = [];
    created.orderAt = 0;
    created.nrev = 0;
    try {
      defineProp(globalThis, key, {
        value: created, enumerable: false, writable: false, configurable: false
      });
    } catch (e2) { /* the page won the race on this unguessable name */ }
    var again = null;
    try { again = getOwn(globalThis, key); } catch (e3) { again = null; }
    return (again && again.value) || created;
  }

  function derefOf(ref) {
    if (!ref || typeof ref.deref !== "function") return null;
    try { return ref.deref(); } catch (e) { return null; }
  }

  function forgetRef(id, ref) {
    try { mapDelete.call(state.rev, id); } catch (e) { /* already gone */ }
    if (state.nrev > 0) state.nrev--;
    var node = derefOf(ref);
    if (node) {
      try { wmDelete.call(state.ids, node); } catch (e2) { /* id number stays retired */ }
    }
  }

  function compactOrder() {
    if (state.orderAt > 1024 && state.orderAt * 2 > state.order.length) {
      var next = [];
      var n = 0;
      for (var i = state.orderAt; i < state.order.length; i++) {
        if (state.order[i] != null) next[n++] = state.order[i];
      }
      state.order = next;
      state.orderAt = 0;
    }
  }

  // Previous generation first (not in state.fresh), then the oldest id of this walk.
  function dropOldest() {
    var order = state.order;
    var i = state.orderAt || 0;
    var oldestFresh = -1;
    while (i < order.length) {
      var id = order[i];
      if (id == null) { i++; continue; }
      var ref = null;
      try { ref = mapGet.call(state.rev, id); } catch (e) { ref = null; }
      if (!ref) { order[i] = null; i++; continue; }
      if (!state.fresh || state.fresh[id] !== 1) {
        order[i] = null;
        state.orderAt = i + 1;
        forgetRef(id, ref);
        compactOrder();
        return true;
      }
      if (oldestFresh < 0) oldestFresh = i;
      i++;
    }
    state.orderAt = i;
    if (oldestFresh < 0) return false;
    var oldId = order[oldestFresh];
    var oldRef = null;
    try { oldRef = mapGet.call(state.rev, oldId); } catch (e2) { oldRef = null; }
    order[oldestFresh] = null;
    state.orderAt = oldestFresh + 1;
    forgetRef(oldId, oldRef);
    compactOrder();
    return true;
  }

  function remember(id, node) {
    if (typeof WeakRef !== "function") return;
    if (!state.fresh) state.fresh = blank();
    state.fresh[id] = 1;
    var existing = null;
    try { existing = mapGet.call(state.rev, id); } catch (e) { existing = null; }
    if (existing) return;
    while (state.nrev >= REV_CAP) {
      if (!dropOldest()) break;
    }
    if (state.nrev >= REV_CAP) return;
    var ref = null;
    try { ref = new WeakRef(node); } catch (e2) { return; }
    try { mapSet.call(state.rev, id, ref); } catch (e3) { return; }
    state.order[state.order.length] = id;
    state.nrev++;
  }

  function prune(st) {
    var order = st.order || [];
    var at = st.orderAt || 0;
    var kept = [];
    var n = 0;
    for (var i = at; i < order.length; i++) {
      var id = order[i];
      if (id == null) continue;
      var ref = null;
      try { ref = mapGet.call(st.rev, id); } catch (e) { ref = null; }
      if (!ref || !derefOf(ref)) {
        try { mapDelete.call(st.rev, id); } catch (e2) { /* gone */ }
        continue;
      }
      kept[n++] = id;
    }
    st.order = kept;
    st.orderAt = 0;
    st.nrev = n;
  }

  function fallback(st) {
    var id = 1;
    try {
      if (st && st.ids && typeof document !== "undefined") {
        var existing = wmGet.call(st.ids, document);
        if (existing) id = existing;
      }
    } catch (e) { id = 1; }
    return "{\"truncated\":true,\"root\":" + jsNum(id) +
      ",\"focus\":null,\"nodes\":[{\"id\":" + jsNum(id) + ",\"kind\":\"document\"}]}";
  }

  function tagOf(el) {
    var tag = el && el.tagName;
    if (!tag) return "";
    tag = asString(tag);
    if (tag.length > 64) { truncated = true; return ""; }
    var out = "";
    for (var i = 0; i < tag.length; i++) {
      var code = tag.charCodeAt(i);
      if (code >= 97 && code <= 122) out += String.fromCharCode(code - 32);
      else out += tag.charAt(i);
    }
    return out;
  }

  function attrRaw(el, name) {
    try { return el.getAttribute(name); } catch (e) { return null; }
  }

  function attr(el, name) {
    var raw = attrRaw(el, name);
    if (raw == null) return null;
    return readStr(asString(raw));
  }

  // The first read wins. A getter that throws, or that is not a string, is a password
  // so a later read cannot come back as text and leak the value.
  function inputType(el) {
    if (!typeCache) return "password";
    var cached;
    try { cached = wmGet.call(typeCache, el); } catch (e) { cached = undefined; }
    if (cached !== undefined) return cached;
    var result = "password";
    try {
      var raw = el.type;
      if (typeof raw !== "string") result = (raw == null || raw === "") ? "text" : "password";
      else if (raw === "") result = "text";
      else if (raw.length > 64) result = "password";
      else result = lowerAscii(raw);
    } catch (e2) { result = "password"; }
    try { wmSet.call(typeCache, el, result); }
    catch (e3) { typeCache = null; return "password"; }
    return result;
  }

  function hasHref(el) {
    var href = attrRaw(el, "href");
    if (href) return true;
    var xlink = attrRaw(el, "xlink:href");
    return !!xlink;
  }

  function hrefOf(el) {
    var href = attrRaw(el, "href");
    if (href) return spendWhole(asString(href));
    var xlink = attrRaw(el, "xlink:href");
    if (!xlink) return null;
    return spendWhole(asString(xlink));
  }

  function isWs(code) {
    return code === 32 || code === 9 || code === 10 || code === 13 || code === 12 || code === 11
      || code === 160 || code === 0x1680 || (code >= 0x2000 && code <= 0x200A)
      || code === 0x2028 || code === 0x2029 || code === 0x202F || code === 0x205F
      || code === 0x3000 || code === 0xFEFF;
  }

  function utf8len(text) {
    var n = 0;
    var limit = text.length;
    if (limit > MAX_STRING_BYTES + 1) limit = MAX_STRING_BYTES + 1;
    for (var i = 0; i < limit; i++) {
      var code = text.charCodeAt(i);
      if (code < 0x80) n += 1;
      else if (code < 0x800) n += 2;
      else if (code >= 0xD800 && code <= 0xDBFF) { n += 4; i++; }
      else n += 3;
      if (n > MAX_STRING_BYTES) return n;
    }
    if (text.length > limit) return MAX_STRING_BYTES + 1;
    return n;
  }

  function collapseWs(text) {
    var out = [];
    var prev = false;
    for (var i = 0; i < text.length; i++) {
      var code = text.charCodeAt(i);
      if (isWs(code)) {
        if (!prev) { out[out.length] = " "; prev = true; }
      } else {
        prev = false;
        out[out.length] = text.charAt(i);
        if (code >= 0xD800 && code <= 0xDBFF) {
          i++;
          if (i < text.length) out[out.length] = text.charAt(i);
        }
      }
    }
    return join(out);
  }

  function trimStartSpaces(text) {
    var n = 0;
    while (n < text.length && text.charCodeAt(n) === 32) n++;
    return n ? slice(text, n, text.length) : text;
  }

  function trimEndSpaces(text) {
    var n = text.length;
    while (n > 0 && text.charCodeAt(n - 1) === 32) n--;
    return n === text.length ? text : slice(text, 0, n);
  }

  var state = null;
  var typeCache = null;
  var nodes = [];
  var byId = blank();
  var live = 0;
  var used = 0;
  var truncated = false;
  var started = 0;
  var visits = 0;
  var charsLeft = MAX_TOTAL_BYTES * 8;
  var seenDocs = typeof WeakSet === "function" ? new WeakSet() : null;
  var emitted = typeof WeakSet === "function" ? new WeakSet() : null;
  var stack = [];

  function overBudget() {
    if (live >= MAX_NODES || used >= MAX_TOTAL_BYTES || visits >= MAX_VISITS || charsLeft <= 0) {
      truncated = true;
      return true;
    }
    if (now() - started > TIME_BUDGET_MS) { truncated = true; return true; }
    return false;
  }

  function spendVisit() {
    if (visits >= MAX_VISITS) { truncated = true; return false; }
    visits++;
    return true;
  }

  function unbudget(text) {
    if (!text) return;
    used -= utf8len(text);
    if (used < 0) used = 0;
  }

  // Bound a page string before any helper scans it. A string past the character
  // budget is sliced down to the per-string cap; the rest is not copied.
  function readStr(raw) {
    if (raw == null || raw === "") return "";
    var text = typeof raw === "string" ? raw : asString(raw);
    var n = text.length;
    if (n > charsLeft) {
      truncated = true;
      var keep = charsLeft;
      if (keep > MAX_STRING_BYTES) keep = MAX_STRING_BYTES;
      charsLeft = 0;
      if (keep <= 0) return "";
      return slice(text, 0, keep);
    }
    charsLeft -= n;
    if (n > MAX_STRING_BYTES) {
      truncated = true;
      return slice(text, 0, MAX_STRING_BYTES);
    }
    return text;
  }

  // An href is dropped whole. It is never cut.
  function spendWhole(text) {
    if (!text) return null;
    var n = text.length;
    if (n > charsLeft || n > MAX_STRING_BYTES) {
      truncated = true;
      if (n > charsLeft) charsLeft = 0;
      else charsLeft -= n;
      return null;
    }
    charsLeft -= n;
    return text;
  }

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
      out[out.length] = piece;
      bytes += add;
      i += step;
    }
    if (i < raw.length) cut = true;
    if (cut) truncated = true;
    used += bytes;
    return join(out);
  }

  function fitLabel(text) {
    if (!text) return null;
    var collapsed = trimEndSpaces(trimStartSpaces(collapseWs(readStr(text))));
    if (!collapsed) return null;
    var kept = takeDomText(collapsed, false);
    return kept || null;
  }

  function fitHref(absolute) {
    if (!absolute) return null;
    if (absolute.length > MAX_STRING_BYTES) { truncated = true; return null; }
    var bytes = utf8len(absolute);
    if (bytes > MAX_STRING_BYTES || used + bytes > MAX_TOTAL_BYTES) {
      truncated = true;
      return null;
    }
    used += bytes;
    return absolute;
  }

  function absoluteUrl(raw, doc) {
    if (!raw || typeof URL !== "function") return null;
    try {
      var base = doc && doc.baseURI ? doc.baseURI : document.baseURI;
      return new URL(raw, base).href;
    } catch (e) { return null; }
  }

  function idOf(node) {
    var existing;
    try { existing = wmGet.call(state.ids, node); } catch (e) { existing = undefined; }
    if (existing !== undefined) {
      remember(existing, node);
      return existing;
    }
    var id = state.next++;
    if (typeof id !== "number" || id !== id) id = 1;
    try { wmSet.call(state.ids, node, id); } catch (e2) { /* the id still names this walk */ }
    remember(id, node);
    return id;
  }

  function pushNode(obj) {
    if (live >= MAX_NODES) { truncated = true; return false; }
    nodes[nodes.length] = obj;
    byId[obj.id] = obj;
    live++;
    return true;
  }

  function discard(obj) {
    if (!obj) return;
    unbudget(obj.text);
    unbudget(obj.value);
    unbudget(obj.label);
    unbudget(obj.href);
    if (obj.options) {
      for (var i = 0; i < obj.options.length; i++) unbudget(obj.options[i]);
    }
  }

  function removeNode(id) {
    var obj = byId[id];
    if (!obj || obj.dropped) return;
    unbudget(obj.text);
    obj.dropped = true;
    delete byId[id];
    live--;
    if (live < 0) live = 0;
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
        var opts = blank();
        opts.checkOpacity = false;
        opts.checkVisibilityCSS = true;
        opts.contentVisibilityAuto = false;
        opts.opacityProperty = false;
        opts.visibilityProperty = true;
        shown = el.checkVisibility(opts);
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
        if (display === "contents") return "show";
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

  function firstToken(text) {
    var s = lowerAscii(readStr(text));
    var start = 0;
    while (start < s.length && isWs(s.charCodeAt(start))) start++;
    var stop = start;
    while (stop < s.length && !isWs(s.charCodeAt(stop))) stop++;
    if (start === stop) return "";
    return slice(s, start, stop);
  }

  function roleOf(el) {
    var role = attrRaw(el, "role");
    if (!role) return "";
    return firstToken(asString(role));
  }

  function namedRegion(el) {
    var aria = attr(el, "aria-label");
    if (aria) return aria;
    var ids = attr(el, "aria-labelledby");
    if (!ids) return "";
    var doc = el.ownerDocument || document;
    var out = [];
    var n = 0;
    var i = 0;
    var chars = 0;
    while (i < ids.length && n < 8) {
      while (i < ids.length && isWs(ids.charCodeAt(i))) i++;
      var start = i;
      while (i < ids.length && !isWs(ids.charCodeAt(i))) i++;
      if (start === i) break;
      n++;
      var target = null;
      try { target = doc.getElementById(slice(ids, start, i)); } catch (e) { target = null; }
      if (!target) continue;
      var text = "";
      try { text = target.textContent || ""; } catch (e2) { text = ""; }
      text = readStr(text);
      if (!text) continue;
      out[out.length] = text;
      chars += text.length;
      if (chars > MAX_STRING_BYTES) break;
    }
    return joinParts(out, " ");
  }

  function joinParts(parts, sep) {
    var s = "";
    for (var i = 0; i < parts.length; i++) {
      if (i) s += sep;
      s += parts[i];
    }
    return s;
  }

  function infoKind(kind) {
    var info = blank();
    info.kind = kind;
    return info;
  }

  function roleKind(role, el) {
    if (role === "presentation" || role === "none") return infoKind("flatten");
    if (role === "banner") return withLandmark("banner");
    if (role === "navigation") return withLandmark("navigation");
    if (role === "main") return withLandmark("main");
    if (role === "complementary") return withLandmark("complementary");
    if (role === "contentinfo") return withLandmark("contentinfo");
    if (role === "search") return withLandmark("search");
    if (role === "form") return withLandmark("form");
    if (role === "region") return withLandmark("region");
    if (role === "heading") {
      var info = infoKind("heading");
      var level = parseInt(attr(el, "aria-level"), 10);
      if (!(level >= 1 && level <= 6)) level = 2;
      info.level = level;
      return info;
    }
    if (role === "link") return infoKind("link");
    if (role === "button") return infoKind("button");
    if (role === "textbox") return infoKind("textfield");
    if (role === "checkbox") return infoKind("checkbox");
    if (role === "radio") return infoKind("radio");
    if (role === "list") return infoKind("list");
    if (role === "listitem") return infoKind("listitem");
    if (role === "img" || role === "image") return infoKind("image");
    if (role === "separator") return infoKind("separator");
    if (role === "paragraph") return infoKind("paragraph");
    if (role === "table" || role === "grid") return infoKind("table");
    if (role === "row") return infoKind("row");
    if (role === "cell" || role === "gridcell" || role === "columnheader" || role === "rowheader") {
      return infoKind("cell");
    }
    return null;
  }

  function withLandmark(name) {
    var info = infoKind("landmark");
    info.landmark = name;
    return info;
  }

  function classifyInput(el) {
    var type = inputType(el);
    if (type === "hidden") return infoKind("skip");
    if (type === "checkbox") return infoKind("checkbox");
    if (type === "radio") return infoKind("radio");
    if (type === "button" || type === "submit" || type === "reset" || type === "image") {
      return infoKind("button");
    }
    if (type === "file") {
      var file = infoKind("textfield");
      file.file = true;
      return file;
    }
    if (type === "password") {
      var pw = infoKind("textfield");
      pw.password = true;
      return pw;
    }
    return infoKind("textfield");
  }

  function classify(el, sectioning) {
    var tag = tagOf(el);
    if (tag === "INPUT") return classifyInput(el);
    if (tag === "TEXTAREA") return infoKind("textarea");
    if (tag === "SELECT") return infoKind("select");
    if (tag === "IMG") return infoKind("image");
    if (tag === "IFRAME" || tag === "FRAME") return infoKind("iframe");
    if (tag === "BUTTON") return infoKind("button");
    var byRole = roleKind(roleOf(el), el);
    if (byRole) return byRole;
    if (tag === "A" || tag === "AREA") {
      if (hasHref(el)) return infoKind("link");
      return infoKind("flatten");
    }
    if (tag === "H1" || tag === "H2" || tag === "H3" || tag === "H4" || tag === "H5" || tag === "H6") {
      var info = infoKind("heading");
      var level = tag.charCodeAt(1) - 48;
      var ariaLevel = parseInt(attr(el, "aria-level"), 10);
      if (ariaLevel >= 1 && ariaLevel <= 6) level = ariaLevel;
      info.level = level;
      return info;
    }
    if (tag === "P" || tag === "FIGCAPTION" || tag === "CAPTION" || tag === "SUMMARY") {
      return infoKind("paragraph");
    }
    if (tag === "UL" || tag === "OL" || tag === "MENU" || tag === "DL") return infoKind("list");
    if (tag === "LI" || tag === "DT" || tag === "DD") return infoKind("listitem");
    if (tag === "TABLE") return infoKind("table");
    if (tag === "TR") return infoKind("row");
    if (tag === "TD" || tag === "TH") return infoKind("cell");
    if (tag === "PRE") return infoKind("pre");
    if (tag === "CODE") return infoKind("code");
    if (tag === "BLOCKQUOTE") return infoKind("quote");
    if (tag === "HR") return infoKind("separator");
    if (tag === "HEADER") return sectioning ? infoKind("container") : withLandmark("banner");
    if (tag === "FOOTER") return sectioning ? infoKind("container") : withLandmark("contentinfo");
    if (tag === "NAV") return withLandmark("navigation");
    if (tag === "MAIN") return withLandmark("main");
    if (tag === "ASIDE") return withLandmark("complementary");
    if (tag === "FORM") return withLandmark("form");
    if (tag === "SEARCH") return withLandmark("search");
    if (tag === "SECTION" || tag === "ARTICLE") {
      var name = namedRegion(el);
      if (name) {
        var region = withLandmark("region");
        region.label = name;
        return region;
      }
      return infoKind("container");
    }
    if (INLINE[tag] || FLAT[tag]) return infoKind("flatten");
    return infoKind("container");
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
    var pending = [];
    pending[0] = label;
    var chars = 0;
    while (pending.length) {
      var node = pending[pending.length - 1];
      pending.length = pending.length - 1;
      if (!node || node === control || chars > MAX_STRING_BYTES) continue;
      if (!spendVisit()) break;
      var type = node.nodeType;
      if (type === 3) {
        var bit = readStr(node.nodeValue || "");
        if (bit) {
          parts[parts.length] = bit;
          chars += bit.length;
        }
        continue;
      }
      if (type !== 1 || SKIP[tagOf(node)]) continue;
      var kids = node.childNodes;
      if (!kids) continue;
      var n = kids.length;
      if (n > MAX_CHILDREN) { n = MAX_CHILDREN; truncated = true; }
      for (var i = n - 1; i >= 0; i--) pending[pending.length] = kids[i];
    }
    return join(parts);
  }

  function fieldLabel(el) {
    var list = null;
    try { list = el.labels; } catch (e) { list = null; }
    if (list && list.length) {
      var i;
      var lab;
      if (el.id) {
        for (i = 0; i < list.length && i < MAX_CHILDREN; i++) {
          lab = list[i];
          if (lab && lab.htmlFor === el.id) return labelText(lab, el);
        }
      }
      for (i = 0; i < list.length && i < MAX_CHILDREN; i++) {
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
    try { return el.value == null ? "" : readStr(asString(el.value)); }
    catch (e) { return attr(el, "value") || ""; }
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
        try { raw = el.value == null ? "" : readStr(asString(el.value)); } catch (e2) { raw = ""; }
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
    var limit = 0;
    try { limit = options.length; } catch (e2) { limit = 0; }
    if (limit > MAX_OPTIONS) { limit = MAX_OPTIONS; truncated = true; }
    for (var i = 0; i < limit; i++) {
      if (used >= MAX_TOTAL_BYTES || charsLeft <= 0) { truncated = true; break; }
      var text = "";
      try { text = options[i].text || options[i].label || ""; } catch (e3) { text = ""; }
      text = trimEndSpaces(trimStartSpaces(collapseWs(readStr(asString(text)))));
      out[out.length] = text ? takeDomText(text, false) : "";
    }
    obj.options = out;
    var selected = -1;
    try { selected = el.selectedIndex; } catch (e4) { selected = -1; }
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
        if (child.text) parts[parts.length] = child.text;
        removeNode(child.id);
      } else if (child && !child.dropped) {
        kept[kept.length] = children[i];
      }
    }
    obj.children = kept;
    var text = trimEndSpaces(trimStartSpaces(join(parts)));
    if (!text) return;
    var keptText = takeDomText(text, false);
    if (keptText) obj.text = keptText;
  }

  function removeAt(ids, index) {
    for (var i = index; i < ids.length - 1; i++) ids[i] = ids[i + 1];
    ids.length = ids.length - 1;
  }

  function stripEdge(ids, leading) {
    if (!ids.length) return ids;
    var index = leading ? 0 : ids.length - 1;
    var obj = byId[ids[index]];
    if (!obj || obj.kind !== "text" || typeof obj.text !== "string") return ids;
    var text = obj.text;
    var next = leading ? trimStartSpaces(text) : trimEndSpaces(text);
    if (next === text) return ids;
    if (!next) {
      removeAt(ids, index);
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

  function frame() {
    var fr = blank();
    fr.ids = [];
    fr.stage = 0;
    fr.nextStage = 0;
    fr.childIds = null;
    return fr;
  }

  function listLen(list) {
    var n = 0;
    try { n = list ? list.length : 0; } catch (e) { return 0; }
    if (typeof n !== "number" || n !== n || n < 0) return 0;
    if (n > MAX_CHILDREN) { truncated = true; return MAX_CHILDREN; }
    return floor(n);
  }

  function pushFrame(fr) {
    stack[stack.length] = fr;
  }

  function pushList(list, depth, inPre, sectioning, skipText, elementsOnly) {
    var fr = frame();
    fr.op = "list";
    fr.list = list;
    fr.index = 0;
    fr.count = listLen(list);
    fr.depth = depth;
    fr.inPre = inPre;
    fr.sectioning = sectioning;
    fr.skipText = skipText;
    fr.elementsOnly = elementsOnly;
    pushFrame(fr);
  }

  function pushWalk(node, depth, inPre, sectioning, skipText) {
    var fr = frame();
    fr.op = "walk";
    fr.node = node;
    fr.depth = depth;
    fr.inPre = inPre;
    fr.sectioning = sectioning;
    fr.skipText = skipText;
    pushFrame(fr);
  }

  function appendIds(into, extra) {
    for (var i = 0; i < extra.length; i++) into[into.length] = extra[i];
  }

  function stepList(fr) {
    if (fr.stage === 1) {
      appendIds(fr.ids, fr.childIds || []);
      fr.stage = 0;
    }
    while (fr.index < fr.count) {
      if (overBudget()) return true;
      var node = fr.list[fr.index];
      fr.index++;
      if (!node) continue;
      var type = node.nodeType;
      if (fr.elementsOnly && type !== 1) continue;
      if (type === 3 || type === 4) {
        if (fr.skipText) continue;
        if (fr.depth > MAX_DEPTH) { truncated = true; return true; }
        if (!spendVisit()) return true;
        appendIds(fr.ids, walkText(node, fr.inPre));
        continue;
      }
      if (type !== 1 && type !== 11) continue;
      fr.nextStage = 1;
      pushWalk(node, fr.depth, fr.inPre, fr.sectioning, fr.skipText);
      return false;
    }
    return true;
  }

  function stepDoc(fr) {
    if (fr.stage === 1) {
      fr.ids = fr.childIds || [];
      return true;
    }
    var doc = fr.doc;
    if (!doc || overBudget()) { fr.ids = []; return true; }
    if (seenDocs) {
      if (seenDocs.has(doc)) { truncated = true; fr.ids = []; return true; }
      seenDocs.add(doc);
    }
    var html = null;
    try { html = doc.documentElement; } catch (e) { html = null; }
    if (!html) { fr.ids = []; return true; }
    fr.nextStage = 1;
    pushWalk(html, fr.depth + 1, false, false, fr.skipText);
    return false;
  }

  function beginFrame(fr, el) {
    var frameNode = blank();
    frameNode.id = idOf(el);
    frameNode.kind = "container";
    frameNode.children = [];
    var src = attrRaw(el, "src");
    if (!src) { try { src = el.src || ""; } catch (e) { src = ""; } }
    if (src) {
      var whole = spendWhole(asString(src));
      if (whole) {
        var label = fitHref(absoluteUrl(whole, el.ownerDocument || document));
        if (label) frameNode.label = label;
      }
    }
    if (emitted) emitted.add(el);
    if (!pushNode(frameNode)) {
      discard(frameNode);
      fr.ids = [];
      return true;
    }
    var inner = null;
    try { inner = el.contentDocument; } catch (e2) { inner = null; }
    if (!inner) { fr.ids = [frameNode.id]; return true; }
    fr.obj = frameNode;
    fr.nextStage = 2;
    var docFr = frame();
    docFr.op = "doc";
    docFr.doc = inner;
    docFr.depth = fr.depth;
    docFr.skipText = fr.skipText;
    pushFrame(docFr);
    return false;
  }

  function beginEmitted(fr, el, tag, info) {
    var obj = blank();
    obj.id = idOf(el);
    obj.kind = info.kind;
    obj.children = [];
    if (info.level) obj.level = info.level;
    if (info.landmark) obj.landmark = info.landmark;
    fillControl(el, obj, info);
    if (emitted) emitted.add(el);
    if (!pushNode(obj)) {
      discard(obj);
      fr.ids = [];
      return true;
    }
    if (isLeaf(info.kind)) { fr.ids = [obj.id]; return true; }
    var ariaLabel = null;
    if (info.kind === "button") ariaLabel = attr(el, "aria-label");
    fr.obj = obj;
    fr.tag = tag;
    fr.ariaLabel = ariaLabel;
    fr.childPre = fr.inPre || info.kind === "pre";
    var childSection = fr.sectioning || establishes(tag, info);
    var skipChildText = fr.skipText || (info.kind === "button" && !!ariaLabel);
    fr.nextStage = 3;
    pushList(childNodesOf(el), fr.depth + 1, fr.childPre, childSection, skipChildText, false);
    return false;
  }

  function beginElement(fr) {
    var el = fr.node;
    if (emitted && emitted.has(el)) { fr.ids = []; return true; }
    var tag = tagOf(el);
    if (SKIP[tag]) { fr.ids = []; return true; }
    if (tag === "INPUT" && inputType(el) === "hidden") { fr.ids = []; return true; }
    if (tag === "SLOT" && typeof el.assignedNodes === "function"
        && !(el.hasAttribute && (el.hasAttribute("hidden") || el.getAttribute("aria-hidden") === "true"))) {
      var assigned = [];
      try {
        var opt = blank();
        opt.flatten = true;
        assigned = el.assignedNodes(opt);
      } catch (e) { assigned = []; }
      var list = (assigned && assigned.length) ? assigned : (el.childNodes || []);
      fr.nextStage = 1;
      pushList(list, fr.depth + 1, fr.inPre, fr.sectioning, fr.skipText, false);
      return false;
    }
    var hide = hiddenKind(el);
    if (hide === "subtree") { fr.ids = []; return true; }
    if (hide === "self") {
      fr.nextStage = 1;
      pushList(childNodesOf(el), fr.depth + 1, fr.inPre, fr.sectioning, fr.skipText, true);
      return false;
    }
    if (tag === "BR") {
      fr.ids = fr.skipText ? [] : emitNewline(el);
      return true;
    }
    var info = classify(el, fr.sectioning);
    if (info.kind === "skip") { fr.ids = []; return true; }
    if (info.kind === "flatten") {
      fr.nextStage = 1;
      pushList(childNodesOf(el), fr.depth + 1, fr.inPre, fr.sectioning, fr.skipText, false);
      return false;
    }
    if (info.kind === "iframe") return beginFrame(fr, el);
    return beginEmitted(fr, el, tag, info);
  }

  function beginWalk(fr) {
    if (!fr.node || overBudget()) { fr.ids = []; return true; }
    if (fr.depth > MAX_DEPTH) { truncated = true; fr.ids = []; return true; }
    if (!spendVisit()) { fr.ids = []; return true; }
    var node = fr.node;
    var type = node.nodeType;
    if (type === 3 || type === 4) {
      fr.ids = fr.skipText ? [] : walkText(node, fr.inPre);
      return true;
    }
    if (type === 11) {
      fr.nextStage = 1;
      pushList(node.childNodes, fr.depth + 1, fr.inPre, fr.sectioning, fr.skipText, false);
      return false;
    }
    if (type !== 1) { fr.ids = []; return true; }
    return beginElement(fr);
  }

  function finishElement(fr) {
    var obj = fr.obj;
    var el = fr.node;
    var ids = fr.childIds || [];
    if (!fr.childPre && isBlock(obj.kind)) ids = trimEdges(ids);
    obj.children = ids;
    if (obj.kind === "button") {
      if (fr.ariaLabel) {
        var ariaCap = fitLabel(fr.ariaLabel);
        if (ariaCap) obj.text = ariaCap;
      } else if (fr.tag === "INPUT") {
        var inputCap = fitLabel(inputCaption(el));
        if (inputCap) obj.text = inputCap;
      } else {
        foldButton(obj);
      }
    }
    fr.ids = [obj.id];
    return true;
  }

  function stepWalk(fr) {
    if (fr.stage === 0) return beginWalk(fr);
    if (fr.stage === 1) {
      fr.ids = fr.childIds || [];
      return true;
    }
    if (fr.stage === 2) {
      fr.obj.children = trimEdges(fr.childIds || []);
      fr.ids = [fr.obj.id];
      return true;
    }
    if (fr.stage === 3) return finishElement(fr);
    fr.ids = [];
    return true;
  }

  function step(fr) {
    if (fr.op === "list") return stepList(fr);
    if (fr.op === "doc") return stepDoc(fr);
    return stepWalk(fr);
  }

  function walkText(node, inPre) {
    if (emitted && emitted.has(node)) return [];
    var raw = node.nodeValue;
    if (!raw) return [];
    raw = readStr(raw);
    if (!raw) return [];
    var text = takeDomText(raw, !inPre);
    if (!text) return [];
    var obj = blank();
    obj.id = idOf(node);
    obj.kind = "text";
    obj.text = text;
    if (emitted) emitted.add(node);
    if (!pushNode(obj)) { unbudget(text); return []; }
    return [obj.id];
  }

  function emitNewline(el) {
    if (emitted && emitted.has(el)) return [];
    var text = takeDomText("\n", false);
    if (!text) return [];
    var obj = blank();
    obj.id = idOf(el);
    obj.kind = "text";
    obj.text = text;
    if (emitted) emitted.add(el);
    if (!pushNode(obj)) { unbudget(text); return []; }
    return [obj.id];
  }

  function frameResult(fr) {
    if (fr.op === "list" || fr.op === "doc") return fr.ids || [];
    if (fr.obj) {
      if (!fr.obj.children) fr.obj.children = fr.childIds || [];
      return [fr.obj.id];
    }
    return fr.ids || [];
  }

  function run() {
    var steps = 0;
    var stepCap = MAX_VISITS * 4;
    while (stack.length) {
      if (steps > stepCap) { truncated = true; break; }
      steps++;
      var fr = stack[stack.length - 1];
      var before = stack.length;
      var done = step(fr);
      if (!done && stack.length === before) {
        truncated = true;
        done = true;
      }
      if (!done) continue;
      var ids = fr.ids || [];
      stack.length = stack.length - 1;
      if (!stack.length) return ids;
      var parent = stack[stack.length - 1];
      parent.childIds = ids;
      parent.stage = parent.nextStage;
    }
    if (!stack.length) return [];
    truncated = true;
    var ids2 = [];
    while (stack.length) {
      var top = stack[stack.length - 1];
      ids2 = frameResult(top);
      stack.length = stack.length - 1;
      if (!stack.length) break;
      var parent2 = stack[stack.length - 1];
      if (parent2.op === "list") {
        appendIds(parent2.ids, ids2);
        ids2 = parent2.ids;
      } else if (parent2.obj) {
        parent2.obj.children = ids2;
        parent2.ids = [parent2.obj.id];
        ids2 = parent2.ids;
      } else {
        parent2.childIds = ids2;
        parent2.ids = ids2;
      }
    }
    return ids2;
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
    var id;
    try { id = wmGet.call(state.ids, el); } catch (e3) { id = undefined; }
    if (id === undefined || !byId[id] || byId[id].dropped) return null;
    return id;
  }

  function liveChildren(ids) {
    if (!ids || !ids.length) return null;
    for (var i = 0; i < ids.length; i++) {
      var child = byId[ids[i]];
      if (!child || child.dropped) {
        var out = [];
        for (var j = 0; j < ids.length; j++) {
          var kept = byId[ids[j]];
          if (kept && !kept.dropped) out[out.length] = ids[j];
        }
        return out.length ? out : null;
      }
    }
    return ids;
  }

  function emitNode(obj) {
    var parts = [];
    parts[parts.length] = "{\"id\":";
    parts[parts.length] = jsNum(obj.id);
    parts[parts.length] = ",\"kind\":";
    parts[parts.length] = jsString(obj.kind);
    if (obj.level) {
      parts[parts.length] = ",\"level\":";
      parts[parts.length] = jsNum(obj.level);
    }
    if (obj.landmark) {
      parts[parts.length] = ",\"landmark\":";
      parts[parts.length] = jsString(obj.landmark);
    }
    var kids = liveChildren(obj.children);
    if (kids) {
      parts[parts.length] = ",\"children\":[";
      for (var i = 0; i < kids.length; i++) {
        if (i) parts[parts.length] = ",";
        parts[parts.length] = jsNum(kids[i]);
      }
      parts[parts.length] = "]";
    }
    if (obj.text) {
      parts[parts.length] = ",\"text\":";
      parts[parts.length] = jsString(obj.text);
    }
    if (obj.href) {
      parts[parts.length] = ",\"href\":";
      parts[parts.length] = jsString(obj.href);
    }
    if (obj.value) {
      parts[parts.length] = ",\"value\":";
      parts[parts.length] = jsString(obj.value);
    }
    if (obj.label) {
      parts[parts.length] = ",\"label\":";
      parts[parts.length] = jsString(obj.label);
    }
    if (obj.checked === true) parts[parts.length] = ",\"checked\":true";
    else if (obj.checked === false) parts[parts.length] = ",\"checked\":false";
    if (obj.options) {
      parts[parts.length] = ",\"options\":[";
      for (var k = 0; k < obj.options.length; k++) {
        if (k) parts[parts.length] = ",";
        parts[parts.length] = jsString(obj.options[k] || "");
      }
      parts[parts.length] = "]";
    }
    if (obj.selected === 0 || obj.selected) {
      parts[parts.length] = ",\"selected\":";
      parts[parts.length] = jsNum(obj.selected);
    }
    if (obj.disabled) parts[parts.length] = ",\"disabled\":true";
    if (obj.password) parts[parts.length] = ",\"password\":true";
    parts[parts.length] = "}";
    return join(parts);
  }

  function publish(rootId, focus) {
    var parts = [];
    parts[parts.length] = "{\"truncated\":";
    parts[parts.length] = truncated ? "true" : "false";
    parts[parts.length] = ",\"root\":";
    parts[parts.length] = jsNum(rootId);
    parts[parts.length] = ",\"focus\":";
    parts[parts.length] = focus == null ? "null" : jsNum(focus);
    parts[parts.length] = ",\"nodes\":[";
    var wrote = false;
    for (var i = 0; i < nodes.length; i++) {
      var node = nodes[i];
      if (!node || node.dropped) continue;
      if (wrote) parts[parts.length] = ",";
      parts[parts.length] = emitNode(node);
      wrote = true;
    }
    parts[parts.length] = "]}";
    return join(parts);
  }

  try {
    state = loadState(STATE_KEY);
    prune(state);
    state.fresh = blank();
    typeCache = new WeakMap();
    started = now();
    var root = blank();
    root.id = idOf(document);
    root.kind = "document";
    root.children = [];
    if (emitted) emitted.add(document);
    if (!pushNode(root)) return fallback(state);
    var docFr = frame();
    docFr.op = "doc";
    docFr.doc = document;
    docFr.depth = 0;
    docFr.skipText = false;
    pushFrame(docFr);
    root.children = trimEdges(run());
    return publish(root.id, focusId(document));
  } catch (e) {
    return fallback(state);
  }
})()
