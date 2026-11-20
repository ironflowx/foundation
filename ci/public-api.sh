#!/usr/bin/env bash
# Usage: ci/public-api.sh [--bless]
# Compares each foundation crate's public API with its snapshot in public-api/ (D21); any
# difference fails. --bless rewrites the snapshots after a reviewed change.
# Then checks LED-9 and that no dependency type is public over the snapshots, and each check
# rejecting its fixture.
# Needs cargo-public-api and the nightly pinned in ci/tools.env (ci/install-tools.sh).
set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck source=ci/tools.env
source ci/tools.env
rustup toolchain install "$NIGHTLY" --profile minimal --no-self-update >/dev/null
mkdir -p public-api
for crate in ironflowx-core ironflowx-events ironflowx-proto ironflowx-cell ironflowx ironflowx-behaviors; do
  # -ss keeps derived impls, so a new derive (an Ord, a From) shows in the diff. Hidden items
  # are documented too, so a second #[doc(hidden)] constructor shows.
  RUSTDOCFLAGS="-Zunstable-options --document-hidden-items" cargo "+$NIGHTLY" public-api --color never -p "$crate" -ss > "public-api/$crate.txt"
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

ci/no-bare-env-id.sh public-api/*.txt
ci/no-dependency-types.sh public-api/*.txt
ci/must-reject.sh Ledger::open find_after ffi_open EnvironmentId::lookup \
  OpenRequest::env Target::Env proto::Wrapper -- \
  ci/no-bare-env-id.sh ci/fixtures/bare-environment-id.txt
ci/must-reject.sh rusqlite::error::Error segment_log_core::engine tokio::runtime uuid::Uuid -- \
  ci/no-dependency-types.sh ci/fixtures/dependency-type.txt
