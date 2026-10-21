#!/usr/bin/env python3
"""Regenerate crates/sighurt-core/src/servo_prefs.rs from Servo 0.5's prefs.rs.

The `ServoPreferences` derive names each preference by its field. When Servo
is upgraded, point this at the new servo-config crate's prefs.rs and run it
from the repo root, then update the pinned field count in
`servo_preferences_are_checked_when_the_configuration_loads` if it changed.

    python3 crates/sighurt-core/tests/gen_servo_prefs.py
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
OUT = ROOT / "crates/sighurt-core/src/servo_prefs.rs"
KINDS = {
    "bool": "Bool",
    "i64": "Int",
    "u64": "Uint",
    "String": "String",
    "[f64; 4]": "Rgba",
}


def prefs_source():
    roots = list(Path.home().glob(".cargo/registry/src/*/servo-config-0.5.0/prefs.rs"))
    if len(roots) != 1:
        sys.exit(f"expected one servo-config 0.5.0 prefs.rs, found {roots}")
    return roots[0]


def fields(text):
    start = text.index("pub struct Preferences {")
    depth = 0
    end = start
    for index, char in enumerate(text[start:], start):
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                end = index
                break
    body = text[start:end]
    found = re.findall(r"^\s+pub\s+([A-Za-z0-9_]+)\s*:\s*([^,\n]+),", body, re.M)
    if not found:
        sys.exit("no Preferences fields")
    return [(name, ty.strip()) for name, ty in found]


def main():
    source = prefs_source()
    parsed = fields(source.read_text())
    lines = [
        "//! Servo 0.5 preference names and the value each takes.",
        "//!",
        "//! Generated from `servo-config` 0.5.0 `prefs.rs` (the published crate's commit",
        "//! `77fccacc1f1fdce10498d50173aafaa09d02879e`; the `ServoPreferences` derive names",
        "//! each preference by its field). Do not edit by hand. Regenerate when Servo is",
        "//! upgraded:",
        "//!",
        "//! ```sh",
        "//! python3 crates/sighurt-core/tests/gen_servo_prefs.py",
        "//! ```",
        "//!",
        "//! `config.rs` includes this file with `#[path = \"servo_prefs.rs\"]`.",
        "//!",
        f"//! Source this run read: `{source}`.",
        "",
        "/// The kind of value a Servo 0.5 preference takes.",
        "#[derive(Clone, Copy, Debug, PartialEq, Eq)]",
        "pub(super) enum ServoPrefKind {",
        "    /// `bool`.",
        "    Bool,",
        "    /// `i64`.",
        "    Int,",
        "    /// `u64`.",
        "    Uint,",
        "    /// `String`.",
        "    String,",
        "    /// `[f64; 4]`, `shell_background_color_rgba`.",
        "    Rgba,",
        "}",
        "",
        "/// How many fields `Preferences` has in Servo 0.5.",
        f"pub(super) const PREFERENCE_COUNT: usize = {len(parsed)};",
        "",
        "/// Preference names, in the order of the fields in `prefs.rs`.",
        "pub(super) const PREFERENCE_NAMES: &[&str] = &[",
    ]
    for name, _ty in parsed:
        lines.append(f'    "{name}",')
    lines += [
        "];",
        "",
        "/// The kind of each name in [`PREFERENCE_NAMES`], in the same order.",
        "pub(super) const PREFERENCE_KINDS: &[ServoPrefKind] = &[",
    ]
    for name, ty in parsed:
        kind = KINDS.get(ty)
        if kind is None:
            sys.exit(f"{name}: no kind for type {ty}")
        lines.append(f"    ServoPrefKind::{kind},")
    lines += [
        "];",
        "",
        "const _: () = {",
        "    assert!(PREFERENCE_NAMES.len() == PREFERENCE_COUNT);",
        "    assert!(PREFERENCE_KINDS.len() == PREFERENCE_COUNT);",
        "};",
        "",
        "/// The kind of preference `name`, or `None` when Servo 0.5 has no such field.",
        "pub(super) fn kind_of(name: &str) -> Option<ServoPrefKind> {",
        "    let index = PREFERENCE_NAMES.iter().position(|known| *known == name)?;",
        "    Some(PREFERENCE_KINDS[index])",
        "}",
        "",
    ]
    OUT.write_text("\n".join(lines))
    print(f"wrote {len(parsed)} preferences to {OUT}")


if __name__ == "__main__":
    main()
