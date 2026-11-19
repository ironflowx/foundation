#!/usr/bin/env bash
# Usage: ci/install-tools.sh
# Installs the tools CI pins in ci/tools.env that `cargo` cannot fetch on its own: the nightly
# for the public-API snapshot, cargo-public-api and cargo-deny. Skips any already at its pin.
set -euo pipefail
# shellcheck source=ci/tools.env
source "$(dirname "$0")/tools.env"

rustup toolchain install "$NIGHTLY" --profile minimal --no-self-update

for spec in "cargo-public-api@$CARGO_PUBLIC_API" "cargo-deny@$CARGO_DENY"; do
  name=${spec%@*}
  version=${spec#*@}
  if "$name" --version 2>/dev/null | grep -qxF "$name $version"; then
    continue
  fi
  if command -v cargo-binstall >/dev/null; then
    cargo binstall -y --locked "$name@$version"
  else
    cargo install --locked "$name" --version "$version"
  fi
done

for tool in actionlint shellcheck; do
  command -v "$tool" >/dev/null || echo "missing $tool: install it with your package manager (brew install $tool)"
done
