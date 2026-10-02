# WPE WebKit, built from its release tarball into a plain prefix outside the Nix store
# (docs/rewrite/WPE.md says why). nix/build-wpewebkit.sh builds it in `shell`, roots `runtime` and
# checks the result; shell.nix exposes `env` when the prefix exists.
{
  pkgs ? import ./nixpkgs.nix { },
}:
let
  inherit (pkgs) lib;
  gst = pkgs.gst_all_1;
  version = "2.52.6";

  # The same absolute path on every machine: the prefix's own RPATH and the directories WebKit
  # compiles in (its helper processes, the injected bundle, the sandbox's binds) point into it.
  prefix = "${builtins.getEnv "HOME"}/.local/share/sighurt/deps/wpewebkit-${version}";

  # What WPE WebKit links: nixpkgs' webkitgtk_6_0 inputs without the GTK port's own and the ones
  # the WPE port doesn't look for, plus libwpe, the WPE platforms' libraries (libinput, udev,
  # libdrm, gbm) and wpebackend-fdo for the MiniBrowser's legacy backends.
  buildInputs = with pkgs; [
    at-spi2-core # ATK, the ATK bridge and AT-SPI
    expat
    flite
    fontconfig
    freetype
    glib
    gst.gst-plugins-bad
    gst.gst-plugins-base
    gst.gstreamer
    harfbuzzFull
    hyphen
    icu
    lcms2
    libGL
    libavif
    libbacktrace
    libdrm
    libepoxy
    libgbm
    libgcrypt
    libgpg-error
    libinput
    libjpeg
    libjxl
    libmanette
    libpng
    libseccomp
    libsoup_3
    libsysprof-capture
    libtasn1
    libwebp
    libwpe
    libwpe-fdo
    libxkbcommon
    libxml2
    libxslt
    sqlite
    systemdLibs # libudev, and libsystemd for journald logging
    wayland
    wayland-protocols
    woff2
    zlib
  ];

  # Used at run time without being linked. bwrap and xdg-dbus-proxy are compiled in by path.
  gstPlugins = [
    gst.gstreamer
    gst.gst-plugins-base
    gst.gst-plugins-good
    gst.gst-plugins-bad
    gst.gst-libav
  ];
  runtimeInputs = gstPlugins ++ [
    pkgs.glib-networking # TLS for libsoup
    pkgs.bubblewrap
    pkgs.xdg-dbus-proxy
    # libstdc++: clangStdenv's clang uses this GCC's (--gcc-toolchain), and the RPATHs name it.
    (lib.getLib pkgs.stdenv.cc.cc)
  ];

  # The packages whose pkg-config files a program compiled against WPE WebKit reads, with
  # everything they propagate.
  withPropagated =
    ps:
    lib.unique (
      lib.concatMap (p: [ (lib.getDev p) ] ++ withPropagated (p.propagatedBuildInputs or [ ])) ps
    );
  pkgConfigDeps = withPropagated [
    pkgs.glib
    pkgs.libsoup_3
    pkgs.libwpe
  ];
in
{
  inherit version prefix;

  src = pkgs.fetchurl {
    url = "https://wpewebkit.org/releases/wpewebkit-${version}.tar.xz";
    # From wpewebkit-2.52.6.tar.xz.sums, whose tarball's .asc signature checks out against
    # Adrián Pérez de Castro's key 5AA3 BC33 4FD7 E336 9E7C 77B2 91C5 59DB E4C9 123B.
    sha256 = "b2bafef2751625b7fdf530f230ff0f542ff0eeba3590c3a989d931b2a55c858e";
  };

  # The build environment, entered with --pure by nix/build-wpewebkit.sh. Clang, as nixpkgs
  # builds WebKitGTK (https://webkitgtk.org/2024/10/04/webkitgtk-2.46.html).
  shell = (pkgs.mkShell.override { stdenv = pkgs.clangStdenv; }) {
    nativeBuildInputs = with pkgs; [
      bison
      cmake
      gettext
      glib # gdbus-codegen, glib-mkenums
      gobject-introspection
      gperf
      ninja
      perl
      perl.pkgs.FileCopyRecursive
      pkg-config
      python3
      ruby
      unifdef
      wayland-scanner
    ];
    inherit buildInputs;
    # Otherwise the linker wrapper adds this shell's (nonexistent) output to every RPATH.
    NIX_NO_SELF_RPATH = true;
    WPE_BWRAP = lib.getExe pkgs.bubblewrap;
    WPE_DBUS_PROXY = lib.getExe pkgs.xdg-dbus-proxy;
  };

  # nixpkgs' sandbox patch for WebKitGTK, which applies to WPE as is: the sandboxed web process
  # sees /nix/store, /run/current-system and the GL driver.
  sandboxPatch =
    pkgs.replaceVars (pkgs.path + "/pkgs/by-name/we/webkitgtk_6_0/fix-bubblewrap-paths.patch")
      {
        inherit (builtins) storeDir;
        inherit (pkgs.addDriverRunpath) driverLink;
      };

  # Every store path the installed prefix may refer to. nix/build-wpewebkit.sh roots it next to
  # the prefix (a plain directory has no references Nix could see) and checks the prefix against it.
  runtime = pkgs.writeText "wpewebkit-${version}-runtime" (
    lib.concatLines (
      map toString (
        lib.concatMap (p: [
          (lib.getLib p)
          (lib.getBin p)
          (lib.getDev p)
        ]) (buildInputs ++ runtimeInputs)
      )
    )
  );

  # For programs using the prefix (shell.nix). Cargo reruns every build script that uses
  # pkg-config when PKG_CONFIG_PATH changes, and half of Servo's graph has one, so WPE's
  # pkg-config path is a variable of its own for the WPE engine's build script to use.
  env = {
    SIGHURT_WPE_PREFIX = prefix;
    SIGHURT_WPE_PKG_CONFIG_PATH = lib.concatStringsSep ":" (
      [ "${prefix}/lib/pkgconfig" ]
      ++ map (p: "${p}/lib/pkgconfig") pkgConfigDeps
      ++ map (p: "${p}/share/pkgconfig") pkgConfigDeps
    );
    GIO_EXTRA_MODULES = "${pkgs.glib-networking}/lib/gio/modules";
    GST_PLUGIN_SYSTEM_PATH_1_0 = lib.makeSearchPathOutput "lib" "lib/gstreamer-1.0" gstPlugins;
  };
}
