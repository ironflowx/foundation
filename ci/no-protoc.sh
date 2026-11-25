#!/usr/bin/env bash
# Usage: ci/no-protoc.sh <command...>
# SRV-14: runs <command>, a build, with neither protoc nor buf on PATH and in a target directory of
# its own, so that every build script runs. It drops each PATH directory that holds one, so a
# machine that keeps one beside cargo or the C compiler cannot run it. PROTOC and BUF, which some
# build scripts read for a path, are unset. Not caught, and not in this repo: a protoc or buf a
# build script downloads, vendors or finds by a full path.
set -euo pipefail
path=
IFS=: read -ra dirs <<<"$PATH"
for dir in "${dirs[@]}"; do
  [ -e "$dir/protoc" ] || [ -e "$dir/buf" ] || path+="${path:+:}$dir"
done
target=$(mktemp -d)
trap 'rm -rf "$target"' EXIT
env -u PROTOC -u BUF PATH="$path" CARGO_TARGET_DIR="$target" "$@"
