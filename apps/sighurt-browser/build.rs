//! Links `sig` with `-Wl,--exclude-libs,ALL` (decision 65): the C libraries statically linked
//! into it (Servo's HarfBuzz, FreeType and libpng, SQLite, mozjpeg, SpiderMonkey's environment
//! wrappers) stay private instead of being exported, where the shared libraries WebKit, FFmpeg
//! and fontconfig load would bind to them in place of their own versions (docs/rewrite/WPE.md,
//! "Symbols"). WPE WebKit isn't linked: its engine opens it by path when a WPE page first opens.

fn main() {
    println!("cargo:rustc-link-arg=-Wl,--exclude-libs,ALL");
}
