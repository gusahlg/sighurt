# nixpkgs' `ladybird`, the pinned one (nix/nixpkgs.nix), with the patches of nix/ladybird/ on top and
# nothing else changed, so the shim built against it (docs/rewrite/LADYBIRD.md) keeps its ABI guard:
#
#   0001: LibWebView gets `ViewImplementation::on_navigation_start_requested`, a hook that holds a
#         page-started navigation of the top-level document before its unload check, and
#         `Application::default_path_for_download`, which tells the override which view a download
#         belongs to. Without a hook set, behavior is unchanged.
#
# Build (a multi-hour build; on louise-pc, never on the desktop; see docs/rewrite/LADYBIRD.md):
#   tools/remote nix/build-ladybird-patched.sh
{ pkgs ? import ./nixpkgs.nix { } }:
pkgs.ladybird.overrideAttrs (old: {
  patches = (old.patches or [ ]) ++ [ ./ladybird/0001-navigation-hold-and-download-view.patch ];
})
