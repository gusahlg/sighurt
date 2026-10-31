# Development shell for NixOS (plan §21): the pinned Rust toolchain and every native library the
# workspace builds and runs with. `tools/cargo` and `tools/heavy` run in it; `nix-shell`, then
# build as usual (`cargo build -p sighurt-browser`).
{ pkgs ? import ./nix/nixpkgs.nix { } }:
let
  # Loaded at runtime by the WASM engine and Servo.
  runtimeLibs = with pkgs; [
    libxkbcommon wayland vulkan-loader libGL mesa
    libx11 libxcb libxcursor libxrandr libxi libxext libxfixes libxrender
    fontconfig freetype alsa-lib udev dbus openssl libv4l
  ];
  # WPE WebKit, once nix/build-wpewebkit.sh has installed it on this machine (docs/rewrite/WPE.md).
  wpe = import ./nix/wpewebkit.nix { inherit pkgs; };
  hasWpe = builtins.pathExists "${wpe.prefix}/lib/pkgconfig/wpe-webkit-2.0.pc";
  # CEF, once nix/build-cef.sh has installed it (docs/rewrite/CEF.md). The shell
  # only exports the prefix path. libcef is not on the link line or the loader path.
  cef = import ./nix/cef.nix { inherit pkgs; };
  hasCef = builtins.pathExists "${cef.prefix}/dist/libcef.so";
in
pkgs.mkShell ({
  nativeBuildInputs = with pkgs; [
    # The pin's Rust (1.98.1), the workspace's `rust-version`: the same compiler on every
    # machine. It is the desktop's own, so Servo's build in target/ stays fresh there.
    rustc cargo clippy rustfmt
    pkg-config cmake gnumake python3 perl yasm nasm m4
    llvmPackages.clang llvmPackages.libclang.lib llvmPackages.lld
  ];
  buildInputs = with pkgs; runtimeLibs ++ [
    ffmpeg_8.dev gtk3 glib pango atk gdk-pixbuf cairo harfbuzz libxkbcommon.dev
    libxcb.dev pipewire libdrm zlib zstd bzip2 libunwind
  ];
  # Nix's fortify hardening breaks jemalloc's -O0 configure step inside Servo's build.
  hardeningDisable = [ "fortify" ];
  # nixpkgs' rustc has no bundled rust-lld; link wasm guests with lld's wasm-ld.
  CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER = "wasm-ld";
  LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs
    + pkgs.lib.optionalString hasWpe ":${wpe.prefix}/lib";
} // pkgs.lib.optionalAttrs hasWpe wpe.env // pkgs.lib.optionalAttrs hasCef cef.env)
