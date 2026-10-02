# The nixpkgs every Nix file in this repository builds with (plan §21: pin the toolchain and the
# native dependencies). Evaluates to the package set; `import ./nix/nixpkgs.nix { }`.
#
# The revision is the one the `sighurt` desktop's system flake had on 2 October 2026 (Rust 1.98.1,
# WebKitGTK 2.52.6), so its shell stayed bit-identical when the pin was introduced. To move it:
# change `rev`, run `nix-prefetch-url --unpack <url>` for `sha256`, and expect Servo and WPE WebKit
# to be rebuilt (docs/rewrite/WPE.md).
{ ... }@args:
let
  rev = "4975466d324710c576dc11ad614684e6bd8cad8e";
  src = builtins.fetchTarball {
    # Named like a flake input, so the store path is the same as that flake's.
    name = "source";
    url = "https://github.com/NixOS/nixpkgs/archive/${rev}.tar.gz";
    sha256 = "1if9h4d8rkgd7a41j978swbixif81iqfd7hk302w0fbd23i9g7y4";
  };
in
# No user configuration or overlays: the same packages on every machine.
import src (
  {
    config = { };
    overlays = [ ];
  }
  // args
)
