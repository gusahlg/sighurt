# sighurt-config-example

The configuration compiled into `sig`. `configure()` returns an `Experience`: the default
experience of `sighurt-config` (a NORMAL mode, where keys are only commands and nothing reaches the
page, an INSERT mode, where every key but Escape reaches the page, the terminal's keys, and three
actions, `y y`, `y f` and `c o`, computed from the browser's state), changed as this crate says.
As it ships it changes nothing; the comments in `src/lib.rs` show each kind of change: binding and
unbinding a key, a sequence, a key in one table or one frontend, bindings for an address, a mode of
your own, an action computed from the browser's state, and a different handling of unmatched keys.

Edit `configure()`, or point the build at a copy of this crate, and rebuild `sig`. A binding replaces
only what it binds, so the other keys stay. `sig` takes the experience apart when it starts: its keys
go to the core, and the configuration file's `[keys]` tables apply over them. An experience with any
problem is not used: `sig` says what is wrong and runs the default experience.

The crate depends on `sighurt-config` alone. It takes no settings from the TOML file, which has its
own key tables. Design: docs/rewrite/experience-2026-11-15.md.
