#!/usr/bin/env bash
# Usage: ci/public-api.sh [--bless]
# Compares each foundation crate's public API with its snapshot in public-api/ (D21); any
# difference fails. --bless rewrites the snapshots after a reviewed change.
# Needs cargo-public-api and the nightly below, whose rustdoc JSON it reads.
set -euo pipefail
toolchain=nightly-2026-11-12
cd "$(dirname "$0")/.."
rustup toolchain install "$toolchain" --profile minimal --no-self-update >/dev/null
mkdir -p public-api
for crate in ironflowx-core ironflowx-events ironflowx-proto ironflowx-cell ironflowx; do
  # -ss keeps derived impls, so a new derive (an Ord, a From) shows in the diff. Hidden items
  # are documented too, so a second #[doc(hidden)] constructor shows.
  RUSTDOCFLAGS="-Zunstable-options --document-hidden-items" cargo "+$toolchain" public-api --color never -p "$crate" -ss > "public-api/$crate.txt"
done
[ "${1:-}" = "--bless" ] && exit 0
# Untracked counts: a snapshot that was never added must fail too.
untracked=$(git ls-files --others --exclude-standard -- public-api/)
if ! git diff --quiet -- public-api/ || [ -n "$untracked" ]; then
  git diff -- public-api/
  echo "$untracked"
  echo "public API changed: review the diff, then run ci/public-api.sh --bless and commit it"
  exit 1
fi
