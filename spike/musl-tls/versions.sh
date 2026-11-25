#!/usr/bin/env bash
# Usage: versions.sh <target>. Every package the build holds, and that none that the workspace's
# Cargo.lock pins is here in a version it does not pin.
set -euo pipefail
pins() { awk '/^name = /{n=$3} /^version = /{gsub(/"/,"",n); v=$3; gsub(/"/,"",v); print n, "v" v}' "$1" | sort -u; }
pins ../../Cargo.lock >/tmp/pins
cargo tree --locked --target "$1" -e normal,build --prefix none -f '{p}' | awk '{print $1, $2}' | sort -u >/tmp/built
echo "packages: $(wc -l </tmp/built), of them new to Cargo.lock:"
wrong=0
while read -r name version; do
  [ "$name" = spike ] && continue
  if ! grep -qxF "$name $version" /tmp/pins; then
    if grep -q "^$name " /tmp/pins; then echo "WRONG: $name $version, pinned: $(grep "^$name " /tmp/pins)"; wrong=1
    else echo "  $name $version"; fi
  fi
done </tmp/built
echo "in both, as pinned:"
grep -E '^(connectrpc|hyper|hyper-util|axum|tokio|rusqlite|libsqlite3-sys|segment-log-core|ironflowx-[a-z]+|cc) ' /tmp/built
echo "who needs the TLS library, and with which features:"
cargo tree --locked --target "$1" -e normal,build -i aws-lc-sys
cargo tree --locked --target "$1" -e features -i aws-lc-rs | head -20
echo "ring: $(grep -c '^ring ' /tmp/built || true) (0: not built)"
exit "$wrong"
