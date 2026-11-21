#!/usr/bin/env bash
# Usage: ci/check-pins.sh <Cargo.toml> <path...>
# Fails if a file under <path> names a segment-log version other than the one <Cargo.toml> pins:
# a `segment-log/blob/v<x>` link, or a full x.y.z version on a line that names segment-log.
# Cargo.toml is the one source of the version; docs that need one (tag-pinned links) must match it.
set -euo pipefail
manifest=$1
shift
pin=$(sed -nE 's/^segment-log-core = "=([0-9]+\.[0-9]+\.[0-9]+)"$/\1/p' "$manifest")
if [ -z "$pin" ]; then
  echo "$manifest: no exact segment-log-core pin found"
  exit 1
fi

status=0
while IFS= read -r hit; do
  line=${hit#*:*:}
  stale=$( { grep -oE 'segment-log/blob/v[0-9]+\.[0-9]+\.[0-9]+' <<<"$line" | sed 's/.*blob\/v//'
    grep -oE '\b[0-9]+\.[0-9]+\.[0-9]+\b' <<<"$line"; } | grep -vxF "$pin" || true)
  if [ -n "$stale" ]; then
    echo "${hit%%:"$line"}: segment-log $(echo "$stale" | sort -u | tr '\n' ' ')is not the pinned $pin ($manifest)"
    status=1
  fi
done < <(grep -rnE 'segment-log' --exclude=Cargo.lock "$@" || true)
exit $status
