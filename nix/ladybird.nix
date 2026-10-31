# nixpkgs' `ladybird`, the pinned one (nix/nixpkgs.nix), with the patches of nix/ladybird/ on top and
# nothing else changed, so the shim built against it (docs/rewrite/LADYBIRD.md) keeps its ABI guard:
#
#   0001: LibWebView gets `ViewImplementation::on_navigation_start_requested`, a hook that holds a
#         page-started navigation of the top-level document before its unload check, and
#         `Application::default_path_for_download`, which tells the override which view a download
#         belongs to. Without a hook set, behavior is unchanged.
#   (and the helpers' wrappers set FONTCONFIG_FILE when it is not set)
#   0002: WebContent cancels a download's stream inside an execution context (upstream aborts the
#         process with a failed VERIFY when the UI process cancels a running download).
#
# Build (a multi-hour build; on louise-pc, never on the desktop; see docs/rewrite/LADYBIRD.md):
#   tools/remote nix/build-ladybird-patched.sh
{ pkgs ? import ./nixpkgs.nix { } }:
pkgs.ladybird.overrideAttrs (old: {
  patches = (old.patches or [ ]) ++ [
    ./ladybird/0001-navigation-hold-and-download-view.patch
    ./ladybird/0002-webcontent-download-cancel-context.patch
  ];
  # The helper processes are wrapped (wrapQtAppsHook); the wrappers set fontconfig's configuration when
  # the embedder has not, so a helper started by `sig` finds fonts without `sig` writing its environment
  # (the store's own fontconfig has no config file at its default path: "Cannot load default config
  # file"). The system's, as NixOS and most distributions have it.
  qtWrapperArgs = (old.qtWrapperArgs or [ ]) ++ [ "--set-default" "FONTCONFIG_FILE" "/etc/fonts/fonts.conf" ];
})
