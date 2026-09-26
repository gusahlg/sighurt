# Development shell for NixOS: `nix-shell`, then build as usual
# (`cargo run -p sighurt-browser`, and the Servo engine with
# `cargo build --release --manifest-path sighurt-engine-servo/Cargo.toml`).
{ pkgs ? import <nixpkgs> { } }:
let
  # Loaded at runtime by GPUI, the WASM engine and Servo.
  runtimeLibs = with pkgs; [
    libxkbcommon wayland vulkan-loader libGL mesa
    libx11 libxcb libxcursor libxrandr libxi libxext libxfixes libxrender
    fontconfig freetype alsa-lib udev dbus openssl libv4l
  ];
in
pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
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
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
}
