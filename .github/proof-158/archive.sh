#!/usr/bin/env bash
set -euo pipefail
umask 077

repo=$PWD
work=$(mktemp -d "${RUNNER_TEMP:-/tmp}/ironflowx-archive.XXXXXXXX")
host_pid=
cleanup() {
  if [ -n "$host_pid" ]; then
    kill "$host_pid" 2>/dev/null || true
    wait "$host_pid" || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT
trap 'echo "Archive proof failed at line $LINENO (exit $?)" >&2' ERR

cd "$work"
test "$(uname -m)" = "$MACHINE"
printf 'Runner: %s %s\n' "$(uname -s)" "$(uname -m)"
archive="ironflowx-0.3.0-$TARGET.tar.gz"
base=https://github.com/ironflowx/foundation/releases/download/v0.3.0
curl -fsSL --retry 3 -o "$archive" "$base/$archive"
curl -fsSL --retry 3 -o "$archive.sha256" "$base/$archive.sha256"
"$repo/ci/release-checksum.sh" "$archive"
cat "$archive.sha256"
test "$(tar -tzf "$archive")" = "$(printf '%s\n' ironflowx LICENSE)"
tar -xzf "$archive"
cmp "$repo/LICENSE" LICENSE
test -x ironflowx
binary_type=$(file -b ironflowx)
printf '%s\n' "$binary_type"
case "$TARGET" in
  x86_64-unknown-linux-musl) [[ "$binary_type" == "ELF 64-bit"*x86-64* ]] ;;
  aarch64-unknown-linux-musl) [[ "$binary_type" == "ELF 64-bit"*aarch64* ]] ;;
  aarch64-apple-darwin) [[ "$binary_type" == "Mach-O 64-bit executable arm64"* ]] ;;
  *) exit 1 ;;
esac
if [[ "$TARGET" == *-linux-musl ]]; then
  "$repo/ci/release-static.sh" ./ironflowx
fi
./ironflowx version >version.json 2>cli.log
jq -es ' . == [{
  version: "0.3.0",
  commit: "3c1acb1ed062abd2516e341ae9c78776d059fb9e",
  protocol: "ironflowx.v1"
}]' version.json >/dev/null
cat version.json

# Run describe before the server exists, and use its returned example directly.
unset IRONFLOWX_KEY IRONFLOWX_URL IRONFLOWX_SEED IRONFLOWX_SEED_FILE
./ironflowx describe events emit >describe.json 2>cli.log
jq -ce 'select(.command == "events emit") | .example | objects' describe.json >example.json
test ! -e data
./ironflowx serve --data-dir "$work/data" --listen 127.0.0.1:9310 >host.log 2>&1 &
host_pid=$!
ready=false
for ((attempt = 0; attempt < 100; attempt++)); do
  kill -0 "$host_pid" 2>/dev/null || break
  if curl -fsS --max-time 1 http://127.0.0.1:9310/ready >/dev/null 2>&1; then
    ready=true
    break
  fi
  sleep 0.1
done
if [ "$ready" != true ]; then
  echo 'Published server did not become ready' >&2
  exit 1
fi
python3 - <<'PY'
import stat
from pathlib import Path

info = Path("data/admin.key").lstat()
if not stat.S_ISREG(info.st_mode) or stat.S_IMODE(info.st_mode) != 0o400 or info.st_size == 0:
    raise SystemExit("Expected a nonempty admin.key file with mode 0400")
PY
echo 'Fresh server became ready and wrote admin.key with mode 0400'

# Keep all server and CLI output private except validated evidence.
export IRONFLOWX_URL=http://127.0.0.1:9310
cli() {
  ./ironflowx --key-file "$work/data/admin.key" "$@" 2>cli.log
}
cli events emit --name proof.release --data '{"ticket":158}' >emit.json
jq -e '(.event_id | type == "string" and length > 0) and .duplicate == false' emit.json >/dev/null
cli events list >list.json
jq -e --slurpfile emitted emit.json '
  any(.events[]; .event_id == $emitted[0].event_id
    and .name == "proof.release" and .data == {ticket: 158})
' list.json >/dev/null
echo 'Published CLI emitted and listed the fixture by event ID'

cli events emit --json - <example.json >example-emit.json
jq -e '(.event_id | type == "string" and length > 0) and .duplicate == false' example-emit.json >/dev/null
cli events list >example-list.json
jq -e --slurpfile emitted example-emit.json --slurpfile example example.json '
  any(.events[]; .event_id == $emitted[0].event_id and .name == $example[0].name)
' example-list.json >/dev/null
echo 'The exact describe events emit example succeeded and appeared in the list'

kill -TERM "$host_pid"
wait "$host_pid"
host_pid=
