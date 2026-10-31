# sighurt-engine-netsurf

NetSurf 3.11 as a Sighurt content engine: one in-process runtime, painting on the CPU into the host's shared-memory target. The design and the measurements are in `docs/rewrite/NETSURF.md`.

## License: do not distribute a combined binary

This crate is **GPL-2.0-only**, because NetSurf's core is. Its source headers say "version 2 of the License" and not "or any later version". Sighurt is AGPL-3.0-or-later. Those licenses cannot both cover one program that is conveyed to someone else: each requires the whole work under its own terms, and GPLv2 has no clause that accepts the extra terms of GPLv3 or AGPLv3.

NetSurf's libraries (libcss, libdom, libhubbub and the rest named in the design) are MIT. The GPL-2.0-only part is the NetSurf core compiled into this crate, the C frontend under `csrc/`, and the patches under `csrc/patches/`.

The `engine-netsurf` feature is not in any crate's default features, and it must stay that way. Building `sig` with it is for your own use. **Do not distribute a `sig` binary that was built with `engine-netsurf`.** Running the combination yourself, without giving it to anyone, is not restricted by either license.

## What this process-global engine does

`netsurf_init` ignores `SIGPIPE` (OpenSSL can raise it) and, before the patch in `csrc/patches/`, would call `setlocale`. The patch stops the locale change: `sig` shares this process with the other engines. There is no second runtime in the process; `nsh_init` after a shutdown fails cleanly.
