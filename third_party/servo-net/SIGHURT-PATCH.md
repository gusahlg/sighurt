# Sighurt's patch of servo-net 0.7.0

This directory is the crates.io source of servo-net 0.7.0 (the version in `Cargo.lock`),
including the MPL-2.0 notices on its sources. The published crate has no separate license
file. The workspace uses it through `[patch.crates-io]`.

The patch makes the public profile crash-safe and private. One new module,
`persistence.rs`, owns the disk protocol. The other edits are hooks and log redaction.

## Commits

`resource_thread` used to write `cookie_jar.json`, `hsts_list.json` and `auth_cache.json`
from the `Exit` handler with `servo_base::write_json_to_file` (create/truncate, panic on
failure). That is gone.

`persistence::ProfileWriter` is spawned when the public resource thread starts, if there is
a profile directory. It is the only writer of cookies and HSTS. It commits at once when it
starts, then at most every 5 seconds while the jar or the HSTS list is dirty, and once more
when it is stopped. `Exit` takes the writer and joins it before answering, so Servo's
shutdown wait covers the final commit. Private `HttpState` is never passed to the writer.

A generation counter on `CookieStorage` and `HstsList` (`#[serde(skip)]`, so the on-disk
JSON is unchanged) is bumped where the jar or the dynamic HSTS list mutates. The writer
compares the counter under the lock and clones a snapshot only when it differs. Serialization
and disk I/O happen after the lock is released. `HstsList::from_entries` replaces struct
literals in `tests/hsts.rs` because the counter is private. The preload list is not written.

The snapshot keeps persistent, unexpired cookies only, cloned as stored. Session cookies
(`persistent == false`) and expired cookies are omitted. Attributes are the stored ones:
host-only, path, flags, and the absolute expiry. Nothing re-parses the cookie or restarts
Max-Age. At load, session and expired entries are dropped; the generation bump makes the
writer's first commit replace the file. A missing file is an empty jar. A damaged file, a
non-regular file (including a symlink), or a file over 32 MiB is renamed to the same name
plus `.damaged` and logged. Load does not panic.

Each commit creates an exclusive temporary file in the same directory (mode 0600), writes,
flushes, fsyncs the file, renames it over the destination, and fsyncs the directory. A
failure deletes the temporary file, leaves the destination and the dirty generation alone,
logs once until a later commit succeeds, and does not panic. Stale `.*.tmp` files from a
killed writer are removed at startup.

## HTTP authentication

The auth cache stays memory-only. `create_http_states` no longer reads `auth_cache.json`.
`prepare_profile_dir` deletes an existing one.

## Logs

`cookie_storage` no longer logs the Cookie header it is about to send. `cookie`'s debug
line no longer includes the cookie value (name, domain and path stay). `http_loader`'s
request and response header dumps skip `Cookie`, `Set-Cookie`, `Authorization` and
`Proxy-Authorization`.

## Tests

`[lib] test` is `true` so `persistence`'s unit tests build (`cargo test -p servo-net --lib`).
Upstream had it `false`. The upstream integration target `tests/main.rs` and the
dev-dependencies it needed are removed: Cargo will not test a path crate that is not a
workspace member while it has dev-dependencies, and this crate stays out of the workspace
so debug builds keep the optimized dependency profile. `serde_json` 1.0.151, the version
already in `Cargo.lock`, is a direct dependency because the crate used to reach it only
through `servo_base`.
