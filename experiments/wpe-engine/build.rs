//! Links WPE WebKit from the prefix `nix/build-wpewebkit.sh` installs, found only through
//! `SIGHURT_WPE_PKG_CONFIG_PATH` (shell.nix): never through `PKG_CONFIG_PATH`, which ten of
//! Servo's build scripts track (docs/rewrite/WPE.md, "Why not PKG_CONFIG_PATH").

fn main() {
    println!("cargo:rerun-if-env-changed=SIGHURT_WPE_PKG_CONFIG_PATH");
    let Ok(path) = std::env::var("SIGHURT_WPE_PKG_CONFIG_PATH") else {
        panic!(
            "SIGHURT_WPE_PKG_CONFIG_PATH is not set: build WPE WebKit with \
             nix/build-wpewebkit.sh and use shell.nix (tools/cargo does)"
        );
    };
    // SAFETY: build scripts are single-threaded; pkg-config reads the variable.
    unsafe { std::env::set_var("PKG_CONFIG_PATH", &path) };
    for package in ["wpe-webkit-2.0", "wpe-platform-2.0"] {
        let library = pkg_config::Config::new()
            .atleast_version("2.52")
            .probe(package)
            .unwrap_or_else(|error| panic!("{package}: {error}"));
        // The prefix is outside the Nix store and off the default search path: binaries find it
        // without LD_LIBRARY_PATH.
        for dir in library.link_paths {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir.display());
        }
    }
}
