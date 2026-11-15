//! Finds the shim library (`libsighurt-ladybird-shim.so`) and hands its path to the crate. It
//! links nothing: the engine opens the library when its runtime first starts. A missing library is
//! not a build error: the runtime says so when it starts (build it with nix/build-ladybird-shim.sh).
//! Linux only, as built here (nixpkgs' Ladybird, eventfds and pidfds): for any other target the
//! build stops at once.

use std::path::PathBuf;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        println!(
            "cargo::error=The Ladybird engine (feature engine-ladybird) is Linux-only: \
             build sig for this system without it (docs/rewrite/PORTING.md)"
        );
        return;
    }
    println!("cargo:rerun-if-env-changed=SIGHURT_LADYBIRD_SHIM");
    println!("cargo:rerun-if-changed=build.rs");
    let name = "lib/libsighurt-ladybird-shim.so";
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("SIGHURT_LADYBIRD_SHIM") {
        candidates.push(path.into());
    }
    candidates.push(PathBuf::from("/data/sig-builds/ladybird").join(name));
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(
            PathBuf::from(home)
                .join(".cache/sighurt/ladybird")
                .join(name),
        );
    }
    let found = candidates.iter().find(|path| path.exists());
    let path = found.unwrap_or(&candidates[0]);
    if found.is_none() {
        println!(
            "cargo:warning=no Ladybird shim found; run nix/build-ladybird-shim.sh (docs/rewrite/LADYBIRD.md)"
        );
    }
    println!(
        "cargo:rustc-env=SIGHURT_LADYBIRD_SHIM_DEFAULT={}",
        path.display()
    );
}
