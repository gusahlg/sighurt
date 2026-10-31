//! Finds the CEF runtime directory (`dist/` of the prefix `nix/build-cef.sh` installs) through
//! `SIGHURT_CEF_PREFIX` (shell.nix sets it when the prefix exists) and hands its path to the
//! crate as `SIGHURT_CEF_DIST`. Nothing is linked: `ffi::load` opens `libcef.so` when the
//! runtime first starts. A missing prefix does not stop the build (the engine is opt-in, and a
//! workspace-wide check must still work): the runtime then refuses to start with what to do.

fn main() {
    println!("cargo:rerun-if-env-changed=SIGHURT_CEF_PREFIX");
    println!("cargo:rerun-if-changed=build.rs");
    let dist = std::env::var("SIGHURT_CEF_PREFIX")
        .ok()
        .map(|prefix| std::path::Path::new(&prefix).join("dist"))
        .filter(|dist| dist.join("libcef.so").exists());
    match dist {
        Some(dist) => println!("cargo:rustc-env=SIGHURT_CEF_DIST={}", dist.display()),
        None => {
            println!("cargo:rustc-env=SIGHURT_CEF_DIST=");
            println!(
                "cargo::warning=CEF is not installed (SIGHURT_CEF_PREFIX has no dist/libcef.so): \
                 run nix/build-cef.sh; the cef engine will refuse to start"
            );
        }
    }
}
