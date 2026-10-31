# CEF (Chromium Embedded Framework), the binary distribution unpacked into a plain prefix outside
# the Nix store (docs/rewrite/CEF.md says why; same scheme as nix/wpewebkit.nix). `libs` are the
# libraries libcef.so and the helper link or load; `runtime` is a GC root listing them;
# `env` is what a shell needs to build and run against the prefix.
{
  pkgs ? import ./nixpkgs.nix { },
}:
let
  inherit (pkgs) lib;
  version = "150.0.22";
  full = "${version}+gc1a097e+chromium-150.0.7871.256";
  prefix = "/data/sig-builds/deps/cef-${version}";

  # libcef.so's DT_NEEDED (readelf -d), plus what Chromium dlopens: libGL/EGL/Vulkan and the
  # driver directory (GPU process), fontconfig/freetype (they are linked statically, but fonts
  # come from fontconfig's files), libpulse/pipewire for audio, libva for video decode.
  libs = with pkgs; [
    glib nspr nss at-spi2-core dbus cups expat cairo pango alsa-lib
    libxkbcommon libgbm libdrm systemdLibs libGL vulkan-loader wayland pipewire
    libx11 libxcb libxcomposite libxdamage libxext libxfixes libxrandr
    (lib.getLib stdenv.cc.cc)
  ];
in
{
  inherit prefix version full libs;
  libraryPath = lib.makeLibraryPath libs;
  runtime = pkgs.writeText "cef-${version}-runtime" (
    lib.concatMapStringsSep "\n" (p: "${lib.getLib p}") libs
  );
  shell = pkgs.mkShell {
    nativeBuildInputs = with pkgs; [ patchelf pkg-config gcc ];
    buildInputs = libs;
  };
}
