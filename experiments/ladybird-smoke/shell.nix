# Build shell of the Ladybird smoke test (docs/rewrite/LADYBIRD.md): the compiler and every dev
# output nixpkgs' `ladybird` builds with, so its installed headers (AK, LibWeb, LibWebView, the
# generated IPC endpoints) compile as they did for nixpkgs. Nothing here is part of the workspace.
{ pkgs ? import ../../nix/nixpkgs.nix { }
# A relocated copy of the patched package (nix/pull-ladybird-patched.sh) to build the shim against.
, patchedPrefix ? null
}:
pkgs.mkShell {
  inputsFrom = [ pkgs.ladybird ];
  packages = [ pkgs.ladybird pkgs.pkg-config ];
  LADYBIRD = if patchedPrefix == null then "${pkgs.ladybird}" else patchedPrefix;
  LADYBIRD_SRC = "${pkgs.ladybird.src}";
  LADYBIRD_ANGLE = "${pkgs.angle}";
  LADYBIRD_SKIA = "${pkgs.skia}";
}
