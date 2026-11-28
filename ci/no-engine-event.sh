#!/usr/bin/env bash
# Usage: ci/no-engine-event.sh <path>...
# Fails if a Rust or Markdown file under <path> names engine_event outside ironflowx-events and
# ironflowx-execution (RUN-14, D17's pattern). Run on the repository root, it reads every such
# file but target directories, ci/fixtures (each fixture is checked on its own) and the docs that
# describe the system: docs/, CHANGELOG.md and ci/README.md. Plain // comment lines are skipped;
# doc comments are not, since a doctest is code, and neither is Markdown, which a crate can take
# as its docs with include_str!.
# Clippy bans the call; this grep backs it up by name where clippy does not look: code behind a
# feature or cfg that CI's build leaves off, a doctest, a file pulled in with include!, and a lint
# config or an #[allow] that loses the ban. It matches the bare name, so an alias's import is
# caught too.
# Not caught: a name a macro builds from pieces (`paste`); a module or include! of a file with
# another extension (`#[path = "x.inc"]`; clippy still lints an included one); a function an
# exempt crate re-exports under another name; and a doctest in the docs above, if a crate took
# one as its docs. No one writes these by accident; every #[path] here names a .rs file, and no
# crate includes a file but its own build output. Code it wrongly refuses: another crate's own
# function, prose, a string, a block comment or a trailing comment naming engine_event; this repo
# has none.
set -euo pipefail
# The perl program is single-quoted on purpose: its $ARGV is perl's, not the shell's.
# shellcheck disable=SC2016
found=$(find "$@" -type f \( -name '*.rs' -o -name '*.md' \) -not -path '*/target/*' \
  -not -path './ci/fixtures/*' -not -path './docs/*' -not -path './CHANGELOG.md' \
  -not -path './ci/README.md' -not -path './.git/*' -print0 \
  | xargs -0 perl -ne '
      print "$ARGV:$.: $_" if /(?<!\w)engine_event(?!\w)/ && !m{^\s*//([^/!]|$)}
        && $ARGV !~ m{^(\./)?crates/ironflowx-(events|execution)/};
      close ARGV if eof;
    ')
if [ -n "$found" ]; then
  echo "engine_event outside ironflowx-events and ironflowx-execution:"
  echo "$found"
  exit 1
fi
