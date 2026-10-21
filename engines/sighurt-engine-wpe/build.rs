//! Finds WPE WebKit in the prefix `nix/build-wpewebkit.sh` installs, only through
//! `SIGHURT_WPE_PKG_CONFIG_PATH` (shell.nix sets it when the prefix exists): never through
//! `PKG_CONFIG_PATH`, which ten of Servo's build scripts track (docs/rewrite/WPE.md, "Why not
//! PKG_CONFIG_PATH"). Without it, the build stops with what to do: `sig`'s default features
//! include this engine. The library isn't linked: its path goes to the crate as
//! `SIGHURT_WPE_LIBRARY`, which `ffi::load` opens when the runtime first starts.

const MISSING: [&str; 3] = [
    "WPE WebKit 2.52 is not installed on this machine, and the WPE engine (feature engine-wpe, \
     one of sig's default features) needs it.",
    "To install it, build it with `nix/build-wpewebkit.sh`, or copy it from a machine that has \
     it with `nix/build-wpewebkit.sh --pull louise-pc`; then build inside shell.nix (tools/cargo \
     does), which sets SIGHURT_WPE_PKG_CONFIG_PATH. See docs/rewrite/WPE.md.",
    "Or build sig without WPE: `cargo build -p sighurt-browser --no-default-features --features \
     engine-servo,engine-wasm`.",
];

fn main() {
    println!("cargo:rerun-if-env-changed=SIGHURT_WPE_PKG_CONFIG_PATH");
    let Ok(path) = std::env::var("SIGHURT_WPE_PKG_CONFIG_PATH") else {
        return missing("SIGHURT_WPE_PKG_CONFIG_PATH is not set");
    };
    // SAFETY: build scripts are single-threaded; pkg-config reads the variable.
    unsafe { std::env::set_var("PKG_CONFIG_PATH", &path) };
    let mut dirs = std::collections::BTreeSet::new();
    for package in ["wpe-webkit-2.0", "wpe-platform-2.0"] {
        match pkg_config::Config::new()
            .atleast_version("2.52")
            .cargo_metadata(false)
            .probe(package)
        {
            Ok(library) => dirs.extend(library.link_paths),
            Err(error) => return missing(&format!("{package}: {error}")),
        }
    }
    let name = "libWPEWebKit-2.0.so.1";
    let Some(library) = dirs
        .iter()
        .map(|dir| dir.join(name))
        .find(|path| path.exists())
    else {
        return missing(&format!("no {name} in {dirs:?}"));
    };
    // By its path: the prefix is outside the Nix store and off the loader's search path, and the
    // library finds its own dependencies through its RPATH.
    println!("cargo:rustc-env=SIGHURT_WPE_LIBRARY={}", library.display());
}

/// Stops the build with `why` and what to do about it.
fn missing(why: &str) {
    for line in MISSING {
        println!("cargo::error={line}");
    }
    // pkg-config's errors span lines; cargo takes one line per message.
    println!(
        "cargo::error=({})",
        why.lines().collect::<Vec<_>>().join(" ")
    );
}
