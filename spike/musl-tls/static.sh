#!/usr/bin/env bash
# Usage: static.sh <binary>. Prints what each tool says and fails unless all agree it is static.
set -uo pipefail
bin=$1
wrong=0
said=$(file -L "$bin"); echo "file: $said"
grep -qE 'statically linked|static-pie linked' <<<"$said" || { echo "static: file says it is not"; wrong=1; }
said=$(ldd "$bin" 2>&1); echo "ldd: $said"
grep -qE 'statically linked|not a dynamic executable' <<<"$said" || { echo "static: ldd says it is not"; wrong=1; }
said=$(readelf -lW "$bin") || wrong=1
if grep -q 'INTERP' <<<"$said"; then echo "static: it names an interpreter"; wrong=1; fi
said=$(readelf -dW "$bin") || wrong=1
if grep -q 'NEEDED' <<<"$said"; then grep NEEDED <<<"$said"; echo "static: it needs a library"; wrong=1; fi
[ "$wrong" -eq 0 ] && echo "static: $bin is static: no interpreter, no needed library"
exit "$wrong"
