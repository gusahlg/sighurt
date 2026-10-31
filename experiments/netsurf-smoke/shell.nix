# Build environment of the NetSurf smoke test (docs/rewrite/NETSURF.md): the pinned nixpkgs'
# NetSurf 3.11 source and libraries, and what its Makefile needs. `nix-shell shell.nix --run ./build.sh`.
{ pkgs ? import ../../nix/nixpkgs.nix { } }:
let
  nsLibs = with pkgs; [
    libcss libdom libhubbub libnsbmp libnsgif libnslog libnspsl libnsutils libparserutils
    libsvgtiny libutf8proc libwapcaplet
  ];
in
pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    pkg-config gnumake perl perlPackages.HTMLParser xxd gperf nsgenbind netsurf-buildsystem
  ];
  buildInputs = nsLibs ++ (with pkgs; [
    curl openssl libjpeg libpng libwebp libxml2 expat libidn freetype zlib
  ]);
  # The pinned NetSurf source tarball, and DejaVu for the smoke test's text.
  NS_SRC = pkgs.netsurf-browser.src;
  NS_FONT = "${pkgs.dejavu_fonts}/share/fonts/truetype/DejaVuSans.ttf";
  NS_FONT_BOLD = "${pkgs.dejavu_fonts}/share/fonts/truetype/DejaVuSans-Bold.ttf";
  NS_FONT_MONO = "${pkgs.dejavu_fonts}/share/fonts/truetype/DejaVuSansMono.ttf";
  NS_FONT_SERIF = "${pkgs.dejavu_fonts}/share/fonts/truetype/DejaVuSerif.ttf";
  NIX_CFLAGS_COMPILE = "-fcommon";
}
