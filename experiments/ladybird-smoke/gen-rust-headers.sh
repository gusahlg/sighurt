#!/usr/bin/env bash
# Generates the Rust FFI headers (cbindgen output of the crates' build.rs) that nixpkgs' `ladybird`
# does not install, from the exact source nixpkgs built, with the vendored crates nixpkgs fetched.
# Offline; builds only the build scripts and `cargo check`s the crates, in its own target directory.
#
#   tools/heavy experiments/ladybird-smoke/gen-rust-headers.sh OUT_DIR
#
# OUT_DIR gets LibURL/RustFFI.h, LibWeb/*RustFFI.h, ... laid out as the installed headers include them.
set -euo pipefail
out=$(realpath -m "${1:?usage: gen-rust-headers.sh OUT_DIR}")
src=$(nix-build --no-out-link -E '(import '"$(dirname "$0")"'/../../nix/nixpkgs.nix {}).ladybird.src')
vendor=$(nix-build --no-out-link -E '(import '"$(dirname "$0")"'/../../nix/nixpkgs.nix {}).ladybird.cargoDeps')
work=$(mktemp -d)
mkdir -p "$work/home" "$work/target" "$out"
cat > "$work/home/config.toml" << EOT
[source.crates-io]
replace-with = "vendored"
[source.vendored]
directory = "$vendor/source-registry-0"
EOT
export CARGO_HOME=$work/home CARGO_TARGET_DIR=$work/target RUSTC_BOOTSTRAP=1
cargo check --frozen --manifest-path "$src/Cargo.toml" \
  -p libweb_rust --features libweb_rust/style-recording \
  -p libweb_content_blocker_rust -p libgfx_rust -p libjs_rust -p liburl_rust -p libunicode_rust \
  -p libtextcodec_rust -p libregex_rust -p libimagedecoders_rust -p libwasm_cranelift
# <crate>-<hash>/out/<header>: each crate's headers belong under its library's include directory.
place() { # place CRATE LIBDIR
  for d in "$work"/target/debug/build/"$1"-*/out; do
    [[ -d $d ]] || continue
    (cd "$d" && find . \( -name '*.h' -o -name '*.inc' \) -print0) | while IFS= read -r -d '' f; do
      mkdir -p "$out/$2/$(dirname "$f")"; cp "$d/$f" "$out/$2/$f"
    done
  done
}
place libweb_rust LibWeb
place libweb_content_blocker_rust LibWeb/ContentBlocker
place libgfx_rust LibGfx
place libjs_rust LibJS
place liburl_rust LibURL
place libunicode_rust LibUnicode
place libtextcodec_rust LibTextCodec
place libregex_rust LibRegex
place libimagedecoders_rust LibImageDecoders
place libwasm_cranelift LibWasm
find "$out" -type f | sort
rm -rf "$work"
