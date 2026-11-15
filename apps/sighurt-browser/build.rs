//! Links `sig` with `-Wl,--exclude-libs,ALL` (decision 65): the C libraries statically linked
//! into it (Servo's HarfBuzz, FreeType and libpng, SQLite, mozjpeg, SpiderMonkey's environment
//! wrappers) stay private instead of being exported, where the shared libraries WebKit, FFmpeg
//! and fontconfig load would bind to them in place of their own versions (docs/rewrite/WPE.md,
//! "Symbols"). WPE WebKit isn't linked: its engine opens it by path when a WPE page first opens.
//!
//! Linux only: the flag is GNU ld's (and lld's) for ELF. Apple's linker and MSVC's reject it, and
//! neither platform binds a library's symbols to another's that way (two-level namespaces, DLL
//! imports), so other targets are linked without it.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg=-Wl,--exclude-libs,ALL");
    }
}
