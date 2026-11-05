//! Builds the internal pages' guests (`pages/sig-page-<name>`, all but the kit) for
//! `wasm32-unknown-unknown` with a cargo of its own, and writes `blobs.rs`, the `blob!` macro
//! `src/lib.rs` embeds them with.
//!
//! The nested cargo is the one building this crate (`$CARGO`), run directly: never through
//! `tools/cargo`, whose machine lock the outer build already holds. It builds into
//! `$OUT_DIR/pages`, never a shared target directory (cargo locks a build directory for its
//! whole build), `--release --locked --offline`, the guests' only dependency being the SDK in
//! this tree, with the outer build's flags and wrappers taken out of its environment so the host
//! build's settings never reach the guests. `SIGHURT_PAGES_PREBUILT=<dir>` skips the build and
//! takes `<dir>/sig_page_<name>.wasm` instead, for a packager that builds the guests itself.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The outer build's settings that must not reach the nested one.
const SCRUBBED: [&str; 13] = [
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
    "RUSTDOCFLAGS",
    "CARGO_ENCODED_RUSTDOCFLAGS",
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET",
    "CARGO_BUILD_TARGET_DIR",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTC_WRAPPER",
    "CLIPPY_ARGS",
    "CARGO_PRIMARY_PACKAGE",
    "__CARGO_FIX_PLZ",
];

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let root = manifest.join("../..");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets it"));
    for watched in ["pages", "sdk/sighurt-sdk", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={}", root.join(watched).display());
    }
    println!("cargo:rerun-if-env-changed=SIGHURT_PAGES_PREBUILT");

    let names = pages(&root.join("pages"));
    let dir = match env::var_os("SIGHURT_PAGES_PREBUILT") {
        Some(dir) => PathBuf::from(dir),
        None if names.is_empty() => out.clone(),
        None => build(&root, &out.join("pages"), &names),
    };
    let mut blobs = String::from(
        "/// The module of the page named in a string literal, `Some(&[u8])` if this build has \
         it.\nmacro_rules! blob {\n",
    );
    for name in &names {
        let file = dir.join(format!("sig_page_{}.wasm", name.replace('-', "_")));
        assert!(
            file.is_file(),
            "the guest of sig://{name} is not at {}",
            file.display()
        );
        writeln!(
            blobs,
            "    ({name:?}) => {{ Some(include_bytes!({:?}) as &[u8]) }};",
            file.display().to_string()
        )
        .expect("a string");
    }
    blobs.push_str("    ($other:literal) => { None };\n}\n");
    fs::write(out.join("blobs.rs"), blobs).expect("OUT_DIR is writable");
}

/// The pages in `dir`: `sig-page-<name>` directories, the kit left out, by name.
fn pages(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().join("Cargo.toml").is_file())
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter_map(|name| name.strip_prefix("sig-page-").map(str::to_owned))
                .filter(|name| name != "kit")
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Builds `names`' guests into `target` and returns where the modules are.
fn build(root: &Path, target: &Path, names: &[String]) -> PathBuf {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .current_dir(root)
        .args(["build", "--release", "--locked", "--offline"])
        .args(["--target", "wasm32-unknown-unknown", "--target-dir"])
        .arg(target)
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"));
    for name in names {
        command.args(["-p", &format!("sig-page-{name}")]);
    }
    for variable in SCRUBBED {
        command.env_remove(variable);
    }
    for (variable, _) in env::vars_os() {
        let variable = variable.to_string_lossy();
        if variable.starts_with("CARGO_TARGET_") && variable.ends_with("_RUSTFLAGS") {
            command.env_remove(variable.as_ref());
        }
    }
    let status = command
        .status()
        .unwrap_or_else(|e| panic!("cannot run cargo for the internal pages: {e}"));
    assert!(
        status.success(),
        "building the internal pages failed ({status}); SIGHURT_PAGES_PREBUILT=<dir> takes \
         prebuilt ones"
    );
    target.join("wasm32-unknown-unknown/release")
}
