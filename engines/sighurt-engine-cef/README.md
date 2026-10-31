# sighurt-engine-cef

Chromium for Sighurt, through the Chromium Embedded Framework's C API: windowless rendering into
CPU frames, compiled into `sig` with the feature `engine-cef` (opt-in; not in any default feature
set). The design, the measurements and the limits are in `docs/rewrite/CEF.md`.

- `libcef.so` is opened with `dlopen` when the first CEF page opens; nothing is linked.
- It comes from the prefix `nix/build-cef.sh` installs (`/data/sig-builds/deps/cef-150.0.22/dist`;
  `SIGHURT_CEF_PREFIX` names it, `shell.nix` exports it).
- `sig-cef-helper` (`helper/`) is Chromium's renderer, GPU and utility process; `sig` has no CEF
  code of its own. It also reports which DOM node holds the focus (text-input state).
- `src/capi.rs` is generated from the pinned headers by `tools/gen_capi.py` (vtable sizes are
  asserted against gcc's).
- Tests: `cargo test -p sighurt-engine-cef` (pure parts), and `tests/offscreen.sh` (Chromium in a
  private headless sway).

## Licences

This crate is `AGPL-3.0-or-later`. The CEF binary distribution it loads is
BSD-3-Clause, Copyright (c) 2008-2020 Marshall A. Greenblatt, Copyright (c) 2006-2009 Google Inc.,
and the Chromium Authors' components under their own licences (the distribution's
`CREDITS.html`, 19 MB, must accompany any redistribution of it). Chromium is
Copyright (c) The Chromium Authors, BSD-3-Clause with the third-party notices in that file.
The CEF C API headers the bindings are generated from are under CEF's BSD-3-Clause licence.
Neither is bundled in this repository.
