# sighurt-semantic

The DOM extraction script and the parser that turns its JSON into a
`sighurt_api::semantic::SemanticSnapshot`. Servo and WPE adapters share it. This crate does not
call an engine.

Evaluate `EXTRACT.as_str()` in the page (WPE in an isolated world, Servo in the main world). The
value is a JSON string. Pass it to `parse(json, document)` with the document generation the
adapter is extracting: the script cannot name a document, and a `document` field in the JSON is
ignored. Evaluate `action_script(&action)` in the **same world** to act on a node id from that
snapshot. Its value is `{"ok":true}` or `{"ok":false,"why":"..."}` (`missing`, `detached`,
`kind`, `range`, `threw`, `disabled`, `readonly`). A disabled control is refused.
A read-only control refuses set, select and activate. Submit calls `requestSubmit` only,
so constraint validation runs; there is no `submit()` fallback.

`EXTRACT` is built on first use. The bounds in the script are `MAX_NODES`, `MAX_DEPTH`,
`MAX_STRING_BYTES`, `MAX_TOTAL_BYTES` and `MAX_OPTIONS` from `sighurt_api::semantic`, plus
`TIME_BUDGET_MS` (50). A per-process unguessable name holds a `WeakMap` of nodes and a `Map` of
`WeakRef`s, capped at twice `MAX_NODES` (the previous generation is dropped first), on a
non-enumerable property of `globalThis`. Ids are not recycled. If `WeakRef` is missing the map
stays empty and actions miss; the script does not keep a strong reference.

Password values are never read. Setting one with `SemanticAction::SetValue` is allowed. File
input values are not read either.

`parse` refuses input longer than `max_input_bytes()` before scanning it. The scan builds nodes
directly and stops at `MAX_NODES`, `MAX_STRING_BYTES`, `MAX_TOTAL_BYTES` and a nesting depth,
without a `serde_json::Value` tree. `max_input_bytes()` is larger than the text budget because
JSON escaping can double a string. An unknown kind (and a heading level outside 1..=6, and an
unknown landmark name) becomes `Container`, then `SemanticSnapshot::check` runs. A failure is an
error, not a partial snapshot. The walk's own budgets set `truncated` instead of failing.

## JSON

```json
{
  "truncated": false,
  "root": 1,
  "focus": null,
  "nodes": [
    {"id": 1, "kind": "document", "children": [2]},
    {"id": 2, "kind": "heading", "level": 1, "children": [3]},
    {"id": 3, "kind": "text", "text": "Hello"}
  ]
}
```

`kind` is `document`, `heading` (with `level` 1–6), `paragraph`, `text`, `link`, `button`,
`textfield`, `textarea`, `checkbox`, `radio`, `select`, `image`, `list`, `listitem`, `table`,
`row`, `cell`, `pre`, `code`, `quote`, `separator`, `landmark`, or `container`. `landmark` is
`banner`, `navigation`, `main`, `complementary`, `contentinfo`, `search`, `form`, or `region`.
Optional fields: `children`, `text`, `href`, `value`, `label`, `checked`, `options`, `selected`,
`disabled`, `password`. Missing `children` is an empty list. Missing `truncated`, `disabled` and
`password` are false.

A form control's label is the `<label for>` text, otherwise a wrapping `<label>`, then
`aria-label`, `placeholder`, or the `name` attribute. Link `href`s and cross-origin frame labels
are absolute; one that does not fit `MAX_STRING_BYTES` is dropped, not cut.

## Fixtures

`tests/pages/` holds pages for later engine tests: headings, links, forms (including a password
sentinel that must not appear in a snapshot), hidden content, a same-origin and a cross-origin
frame, an open and a closed shadow root, landmarks, and a list large enough to pass `MAX_NODES`.
Closed shadow roots are not visible to the script.

## What this crate does not do

It does not run a browser. `node --check` validates the script's syntax when `node` is on `PATH`.
Whitespace collapsing, shadow walking and the password rule are what the script says; an engine
test is what shows a page producing this JSON.
