//! Links the executable the way `sig` should (docs/rewrite/WPE.md, "Symbols"): with
//! `--exclude-libs ALL`, so the C libraries statically linked into it (Servo's bundled HarfBuzz,
//! FreeType and libpng, mozjpeg, zlib, SpiderMonkey's environment wrappers) stay private
//! instead of being exported and interposing on the shared libraries WebKit and its
//! dependencies load. `WPE_COEXIST_EXPORT=1` builds it without the flag, for comparison.

fn main() {
    println!("cargo:rerun-if-env-changed=WPE_COEXIST_EXPORT");
    println!("cargo:rerun-if-env-changed=SIGHURT_WPE_PREFIX");
    if std::env::var_os("WPE_COEXIST_EXPORT").is_none() {
        println!("cargo:rustc-link-arg-bins=-Wl,--exclude-libs,ALL");
    }
    if let Some(prefix) = std::env::var_os("SIGHURT_WPE_PREFIX") {
        let lib = std::path::Path::new(&prefix).join("lib");
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,{}", lib.display());
    }
}
