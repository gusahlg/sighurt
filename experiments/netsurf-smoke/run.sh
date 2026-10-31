#!/usr/bin/env bash
# Runs the smoke test (inside the shell.nix shell):
#   [NS_W=1024 NS_H=768] run.sh BUILD_DIR URL OUT.png [netsurf options, e.g. -v]
set -euo pipefail
b=$1/netsurf-3.11; shift
export NS_RES=$b/frontends/sighurt/res
export SSL_CERT_FILE=${SSL_CERT_FILE:-/etc/ssl/certs/ca-certificates.crt}
exec "$b/nssighurt" "$@"
