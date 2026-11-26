#!/usr/bin/env bash
set -euo pipefail
umask 077

work=$(mktemp -d "${RUNNER_TEMP:-/tmp}/ironflowx-embed.XXXXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/src" "$work/cargo" "$work/data"
cp examples/embed/Cargo.toml "$work/"
cp examples/embed/src/main.rs "$work/src/"
export CARGO_HOME="$work/cargo"
rustup toolchain install 1.100.0 --profile minimal
cd "$work"
test ! -e Cargo.lock
cargo +1.100.0 metadata --format-version 1 >metadata.json
python3 - <<'PY'
import json
from pathlib import Path

expected = {
    "ironflowx", "ironflowx-authz", "ironflowx-cell", "ironflowx-core",
    "ironflowx-events", "ironflowx-operations", "ironflowx-proto", "ironflowx-store",
}
metadata = json.loads(Path("metadata.json").read_text())
resolved = sorted(
    (p["name"], p["version"], p["source"])
    for p in metadata["packages"]
    if p["name"] == "ironflowx" or p["name"].startswith("ironflowx-")
)
wanted = [(name, "0.3.0", "registry+https://github.com/rust-lang/crates.io-index")
          for name in sorted(expected)]
for name, version, source in resolved:
    print(f"{name} {version} ({source})")
if resolved != wanted:
    raise SystemExit("The example did not resolve all eight published v0.3.0 crates")
PY
TMPDIR="$work/data" cargo +1.100.0 run --locked >embed.out 2>build.log || {
  # Cargo's build diagnostics are safe. Runtime output is kept private.
  sed -n '/^     Running /q; p' build.log >&2
  exit 1
}
grep -Fx "embed: bootstrapped an engine, emitted one event, read it back with a read key and saw that key's emit refused" embed.out
