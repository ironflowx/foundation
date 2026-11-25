#!/usr/bin/env bash
# Usage: ci/no-facade-http.sh [--features client-tls]
# With the facade alone and default features, its normal and build graphs omit these HTTP and TLS
# packages on every target. Extra flags are for the fixture that turns the client on deliberately.
set -euo pipefail
cd "$(dirname "$0")/.."

deps=$(cargo tree -p ironflowx --locked --target all -e normal,build --prefix none --format '{p}' \
  "$@" | awk '{print $1}' | sort -u)
status=0
while IFS= read -r dep; do
  case $dep in
    connectrpc | http | hyper | hyper-util | rustls | rustls-native-certs)
      echo "ironflowx: HTTP or TLS dependency '$dep'"
      status=1
      ;;
  esac
done <<<"$deps"
exit "$status"
