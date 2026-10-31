#!/usr/bin/env python3
"""Emit engines/sighurt-engine-cef/src/capi.rs from the pinned CEF C headers.

Not run by build.rs. Regenerating requires the prefix and gcc. The output is
checked against gcc's sizeof for every vtable it emits.
"""
import re
import subprocess
import sys
from pathlib import Path

PREFIX = Path("/data/sig-builds/deps/cef-150.0.22")
INC = PREFIX
HEADERS = [
    "include/capi/cef_app_capi.h",
    "include/capi/cef_browser_process_handler_capi.h",
    "include/capi/cef_client_capi.h",
    "include/capi/cef_command_line_capi.h",
    "include/capi/cef_context_menu_handler_capi.h",
    "include/capi/cef_dialog_handler_capi.h",
    "include/capi/cef_display_handler_capi.h",
    "include/capi/cef_download_handler_capi.h",
    "include/capi/cef_focus_handler_capi.h",
    "include/capi/cef_frame_capi.h",
    "include/capi/cef_jsdialog_handler_capi.h",
    "include/capi/cef_keyboard_handler_capi.h",
    "include/capi/cef_life_span_handler_capi.h",
    "include/capi/cef_load_handler_capi.h",
    "include/capi/cef_menu_model_capi.h",
    "include/capi/cef_navigation_entry_capi.h",
    "include/capi/cef_permission_handler_capi.h",
    "include/capi/cef_render_handler_capi.h",
    "include/capi/cef_request_capi.h",
    "include/capi/cef_request_handler_capi.h",
    "include/capi/cef_task_capi.h",
    "include/capi/cef_browser_capi.h",
    "include/capi/cef_callback_capi.h",
]

VALUE = {
    "void": "()",
    "int": "i32",
    "unsigned": "u32",
    "unsigned int": "u32",
    "int32_t": "i32",
    "int64_t": "i64",
    "uint32_t": "u32",
    "uint64_t": "u64",
    "size_t": "usize",
    "float": "f32",
    "double": "f64",
    "char": "libc::c_char",
    "char16_t": "u16",
    "cef_color_t": "u32",
    "cef_basetime_t": "i64",
    "cef_string_userfree_t": "*mut CefStringUtf16",
    "cef_string_list_t": "*mut c_void",
    "cef_event_handle_t": "*mut c_void",
    "cef_cursor_handle_t": "*mut c_void",
    "cef_window_handle_t": "usize",
}

POINTERS = {
    "void": "c_void",
    "char": "libc::c_char",
    "char16_t": "u16",
    "int": "i32",
    "cef_string_t": "CefStringUtf16",
    "cef_string_utf16_t": "CefStringUtf16",
    "cef_rect_t": "CefRect",
    "cef_size_t": "CefSize",
    "cef_point_t": "CefPoint",
    "cef_range_t": "CefRange",
    "cef_screen_info_t": "CefScreenInfo",
    "cef_mouse_event_t": "CefMouseEvent",
    "cef_key_event_t": "CefKeyEvent",
    "cef_cursor_info_t": "CefCursorInfo",
    "cef_insets_t": "CefInsets",
}


def rust_struct_name(c_name: str) -> str:
    name = c_name.strip()
    if name.startswith("_"):
        name = name[1:]
    if name.startswith("cef_"):
        name = name[4:]
    if name.endswith("_t"):
        name = name[:-2]
    parts = name.split("_")
    return "Cef" + "".join(p[:1].upper() + p[1:] for p in parts if p)


KNOWN: set[str] = set()

RUST_KEYWORDS = {
    "type", "box", "ref", "mut", "async", "await", "dyn", "impl", "fn", "move",
    "mod", "in", "where", "virtual", "abstract", "become", "final", "override",
    "priv", "typeof", "unsized", "yield", "try", "gen", "self", "super", "crate",
}


def rust_ty(raw: str) -> str:
    s = re.sub(r"\s+", " ", raw).strip()
    s = s.replace("struct ", "")
    const = bool(re.search(r"\bconst\b", s))
    base = s.replace("const", "")
    base = re.sub(r"\s+", " ", base).strip()
    stars = base.count("*")
    base = base.replace("*", "").strip()
    if stars == 0:
        if base in VALUE:
            return VALUE[base]
        if base.endswith("_t") or base == "":
            return "i32"
        raise SystemExit(f"unknown value type {raw!r}")
    pointee = POINTERS.get(base)
    if pointee is None:
        if base.startswith("_cef_") or base.startswith("cef_"):
            named = rust_struct_name(base)
            pointee = named if named in KNOWN else "c_void"
        else:
            pointee = "c_void"
    kind = "*const" if const else "*mut"
    ty = pointee
    for _ in range(stars):
        ty = f"{kind} {ty}"
        kind = "*mut"
    return ty


def strip_comments(text: str) -> str:
    out = []
    i = 0
    n = len(text)
    while i < n:
        if text.startswith("//", i):
            i = text.find("\n", i)
            if i < 0:
                break
        elif text.startswith("/*", i):
            j = text.find("*/", i + 2)
            i = n if j < 0 else j + 2
        else:
            out.append(text[i])
            i += 1
    return "".join(out)


# `*( name)` and the wrapped form `*(\n * name)` both name one function-pointer slot.
FN_DECL = re.compile(
    r"(?:\(\s*)?\*\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)\s*\("
)


def extract_structs(text: str):
    """Yield (c_name, body) for each typedef struct, respecting nested braces."""
    i = 0
    key = "typedef struct "
    while True:
        start = text.find(key, i)
        if start < 0:
            return
        brace = text.find("{", start)
        if brace < 0:
            return
        depth = 0
        k = brace
        while k < len(text):
            if text[k] == "{":
                depth += 1
            elif text[k] == "}":
                depth -= 1
                if depth == 0:
                    break
            k += 1
        tail = text[k + 1 : k + 80]
        name_m = re.match(r"\s*(cef_[A-Za-z0-9_]+)\s*;", tail)
        if name_m:
            yield name_m.group(1), text[brace + 1 : k]
        i = k + 1


def parse_structs(text: str):
    text = strip_comments(text)
    structs = []
    for c_name, body in extract_structs(text):
        if not re.match(r"\s*cef_base_ref_counted_t\s+base\s*;", body):
            continue
        fields = []
        for m in FN_DECL.finditer(body):
            prev = body.rfind(";", 0, m.start())
            ret = body[prev + 1 : m.start()].strip()
            arg_start = m.end() - 1
            depth = 0
            k = arg_start
            while k < len(body):
                if body[k] == "(":
                    depth += 1
                elif body[k] == ")":
                    depth -= 1
                    if depth == 0:
                        break
                k += 1
            fields.append((m.group(1), ret, parse_args(body[arg_start + 1 : k])))
        if fields:
            structs.append((c_name, fields))
    return structs


def parse_args(src: str):
    src = src.strip()
    if src == "" or src == "void":
        return []
    parts = []
    depth = 0
    cur = []
    for ch in src:
        if ch == "," and depth == 0:
            parts.append("".join(cur).strip())
            cur = []
        else:
            if ch == "(":
                depth += 1
            elif ch == ")":
                depth -= 1
            cur.append(ch)
    if cur and "".join(cur).strip():
        parts.append("".join(cur).strip())
    args = []
    for i, part in enumerate(parts):
        part = re.sub(r"\s+", " ", part).strip()
        # name is the last identifier, unless it ends with * 
        m = re.match(r"^(.*?)([A-Za-z_][A-Za-z0-9_]*)$", part)
        if not m or m.group(2) in {"int", "void", "const", "unsigned", "long", "short", "char"}:
            ty = part
            name = f"arg{i}"
        else:
            ty = m.group(1).strip()
            name = m.group(2)
            if ty == "":
                ty = name
                name = f"arg{i}"
        if name in RUST_KEYWORDS:
            name = name + "_"
        args.append((name, rust_ty(ty)))
    return args


def emit(structs):
    lines = []
    lines.append("//! CEF 150 C API layouts this crate calls.")
    lines.append("//!")
    lines.append("//! Generated from the pinned headers (`CEF_API_VERSION` 15000) and checked")
    lines.append("//! against `sizeof`. Vtable slots this crate does not fill stay null; CEF")
    lines.append("//! skips a null slot. The slots it fills are real functions.")
    lines.append("//!")
    lines.append("//! Owns the C shapes. Must not own a CEF object or call one.")
    lines.append("")
    lines.append("#![allow(non_camel_case_types)]")
    lines.append("#![allow(clippy::upper_case_acronyms)]")
    lines.append("#![allow(clippy::too_many_arguments)]")
    lines.append("#![allow(clippy::type_complexity)]")
    lines.append("#![allow(dead_code)]")
    lines.append("")
    lines.append("use std::os::raw::c_void;")
    lines.append("")
    lines.append("use libc::size_t;")
    lines.append("")
    # data
    lines.append(DATA)
    seen = set()
    for c_name, fields in structs:
        rust = rust_struct_name(c_name)
        if rust in seen:
            continue
        seen.add(rust)
        lines.append("#[repr(C)]")
        lines.append(f"pub struct {rust} {{")
        lines.append("    pub base: CefBaseRefCounted,")
        for name, ret, args in fields:
            ret_ty = rust_ty(ret)
            arg_s = ", ".join(f"{n}: {t}" for n, t in args)
            lines.append(
                f"    pub {name}: Option<unsafe extern \"C\" fn({arg_s}) -> {ret_ty}>,"
            )
        lines.append("}")
        lines.append("")
    return "\n".join(lines) + "\n", seen


DATA = r'''
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CefStringUtf16 {
    pub str_: *mut u16,
    pub length: usize,
    pub dtor: Option<unsafe extern "C" fn(*mut u16)>,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CefRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CefSize {
    pub width: i32,
    pub height: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CefPoint {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CefRange {
    pub from: i32,
    pub to: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CefInsets {
    pub top: i32,
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
}

#[repr(C)]
pub struct CefBaseRefCounted {
    pub size: size_t,
    pub add_ref: Option<unsafe extern "C" fn(*mut CefBaseRefCounted)>,
    pub release: Option<unsafe extern "C" fn(*mut CefBaseRefCounted) -> i32>,
    pub has_one_ref: Option<unsafe extern "C" fn(*mut CefBaseRefCounted) -> i32>,
    pub has_at_least_one_ref: Option<unsafe extern "C" fn(*mut CefBaseRefCounted) -> i32>,
}

#[repr(C)]
pub struct CefMainArgs {
    pub argc: i32,
    pub _pad: i32,
    pub argv: *mut *mut libc::c_char,
}

#[repr(C)]
pub struct CefMouseEvent {
    pub x: i32,
    pub y: i32,
    pub modifiers: u32,
}

#[repr(C)]
pub struct CefKeyEvent {
    pub size: size_t,
    pub type_: i32,
    pub modifiers: u32,
    pub windows_key_code: i32,
    pub native_key_code: i32,
    pub is_system_key: i32,
    pub character: u16,
    pub unmodified_character: u16,
    pub focus_on_editable_field: i32,
    pub _pad: i32,
}

#[repr(C)]
pub struct CefCursorInfo {
    pub hotspot: CefPoint,
    pub image_scale_factor: f32,
    pub buffer: *mut c_void,
    pub size: CefSize,
}

#[repr(C)]
pub struct CefScreenInfo {
    pub size: size_t,
    pub device_scale_factor: f32,
    pub depth: i32,
    pub depth_per_component: i32,
    pub is_monochrome: i32,
    pub rect: CefRect,
    pub available_rect: CefRect,
}

/// `cef_window_info_t` for Linux. 88 bytes at API 15000.
#[repr(C)]
pub struct CefWindowInfo {
    pub size: size_t,
    pub window_name: CefStringUtf16,
    pub bounds: CefRect,
    pub parent_window: usize,
    pub windowless_rendering_enabled: i32,
    pub shared_texture_enabled: i32,
    pub external_begin_frame_enabled: i32,
    pub _pad: i32,
    pub window: usize,
    pub runtime_style: i32,
    pub _pad2: i32,
}

/// `cef_settings_t` at API 15000. 448 bytes. Strings are `cef_string_utf16_t`.
#[repr(C)]
pub struct CefSettings {
    pub size: size_t,
    pub no_sandbox: i32,
    pub _pad0: i32,
    pub browser_subprocess_path: CefStringUtf16,
    pub framework_dir_path: CefStringUtf16,
    pub main_bundle_path: CefStringUtf16,
    pub multi_threaded_message_loop: i32,
    pub external_message_pump: i32,
    pub windowless_rendering_enabled: i32,
    pub command_line_args_disabled: i32,
    pub cache_path: CefStringUtf16,
    pub root_cache_path: CefStringUtf16,
    pub persist_session_cookies: i32,
    pub _pad1: i32,
    pub user_agent: CefStringUtf16,
    pub user_agent_product: CefStringUtf16,
    pub locale: CefStringUtf16,
    pub log_file: CefStringUtf16,
    pub log_severity: i32,
    pub log_items: i32,
    pub javascript_flags: CefStringUtf16,
    pub resources_dir_path: CefStringUtf16,
    pub locales_dir_path: CefStringUtf16,
    pub remote_debugging_port: i32,
    pub uncaught_exception_stack_size: i32,
    pub background_color: u32,
    pub _pad2: i32,
    pub accept_language_list: CefStringUtf16,
    pub cookieable_schemes_list: CefStringUtf16,
    pub cookieable_schemes_exclude_defaults: i32,
    pub _pad3: i32,
    pub chrome_policy_id: CefStringUtf16,
    pub chrome_app_icon_id: i32,
    pub disable_signal_handlers: i32,
    pub use_views_default_popup: i32,
    pub _pad4: i32,
}

/// The first field is the frame rate. The rest stays zero (`STATE_DEFAULT`).
#[repr(C)]
pub struct CefBrowserSettings {
    pub size: size_t,
    pub windowless_frame_rate: i32,
    pub _rest: [u8; 252],
}

const _: () = {
    assert!(std::mem::size_of::<CefStringUtf16>() == 24);
    assert!(std::mem::size_of::<CefRect>() == 16);
    assert!(std::mem::size_of::<CefBaseRefCounted>() == 40);
    assert!(std::mem::size_of::<CefMainArgs>() == 16);
    assert!(std::mem::size_of::<CefMouseEvent>() == 12);
    assert!(std::mem::size_of::<CefKeyEvent>() == 40);
    assert!(std::mem::size_of::<CefScreenInfo>() == 56);
    assert!(std::mem::size_of::<CefWindowInfo>() == 88);
    assert!(std::mem::size_of::<CefSettings>() == 448);
    assert!(std::mem::size_of::<CefBrowserSettings>() == 264);
};

'''


def preprocess() -> str:
    include = "\n".join(f'#include "{rel}"' for rel in HEADERS)
    src = f"#define CEF_API_VERSION 15000\n{include}\n"
    proc = subprocess.run(
        ["gcc", "-E", "-P", f"-I{PREFIX}", "-"],
        input=src,
        check=True,
        capture_output=True,
        text=True,
    )
    return proc.stdout


def c_sizeof() -> dict[str, int]:
    sizes = {}
    for line in Path("/tmp/cef-sizes.txt").read_text().splitlines():
        name, sz = line.split()
        sizes[name] = int(sz)
    return sizes


def main():
    global KNOWN
    text = preprocess()
    structs = parse_structs(text)
    # Pointers to structs this file defines, plus the hand-written data types.
    KNOWN = {rust_struct_name(name) for name, _ in structs}
    KNOWN.update(
        {
            "CefStringUtf16",
            "CefRect",
            "CefSize",
            "CefPoint",
            "CefRange",
            "CefInsets",
            "CefBaseRefCounted",
            "CefScreenInfo",
            "CefMouseEvent",
            "CefKeyEvent",
            "CefCursorInfo",
        }
    )
    # Re-parse so argument types see KNOWN. The field list does not depend on it,
    # but rust_ty does, and parse_args runs during parse_structs.
    structs = parse_structs(text)
    sizes = c_sizeof()
    kept = []
    bad = []
    for c_name, fields in structs:
        expect = 40 + 8 * len(fields)
        actual = sizes.get(c_name)
        if actual != expect:
            bad.append(f"{c_name}: C={actual} rust={expect} fns={len(fields)}")
            continue
        kept.append((c_name, fields, actual))
    if bad:
        print("SIZE MISMATCH")
        print("\n".join(bad))
        raise SystemExit(1)
    src, _seen = emit([(n, f) for n, f, _ in kept])
    # Size asserts for the vtables. Data structs already assert in DATA.
    asserts = ["const _: () = {"]
    for c_name, _fields, actual in kept:
        rust = rust_struct_name(c_name)
        asserts.append(f"    assert!(std::mem::size_of::<{rust}>() == {actual});")
    asserts.append("};")
    src = src + "\n".join(asserts) + "\n"
    out = Path(__file__).resolve().parents[1] / "src" / "capi.rs"
    out.write_text(src)
    print(f"wrote {out} ({src.count(chr(10))} lines) from {len(kept)} structs")


if __name__ == "__main__":
    main()
