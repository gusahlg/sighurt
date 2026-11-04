# Test font

`Cantarell-Regular.ttf` is used only by tests (sighurt-render's, and the chromes' in sighurt-ui
and sighurt-chrome-line), so text shaping and the golden images don't depend on the fonts
installed on the machine running them. The browser itself uses
the system's fonts.

- **Font:** Cantarell Regular, a subset covering Basic Latin, Latin-1 and Latin Extended-A
  (so å, ä and ö are in it; emoji are not).
- **Copyright:** 2009–2011 Understanding Limited; 2010–2011 Jakub Steiner.
- **License:** SIL Open Font License 1.1, with no Reserved Font Name; the full text is in
  [`OFL.txt`](OFL.txt).
- **Source:** the unmodified file shipped in the `sctk-adwaita` crate, version 0.10.1
  (`src/title/Cantarell-Regular.ttf`), SHA-256
  `d15f25bcdaba2e25f54c32d4e29e68fedeb1b7a1d8fbc83f7c4475916e93b55f`.
