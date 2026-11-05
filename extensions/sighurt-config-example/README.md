# sighurt-config-example

The example configuration: a NORMAL mode, where keys are only commands and nothing reaches the
page, and an INSERT mode, where every key but Escape reaches the page and no binding listens; the
terminal's keys in both frontends; and three actions (`y y`, `y f`, `c o`) computed from the
browser's state. Select it with
`[input] keymap = "example"`. Copy the crate and edit `src/table.rs` and `src/actions.rs` to
make your own; it takes no settings from the TOML file. Design: docs/rewrite/config-extension.md.
