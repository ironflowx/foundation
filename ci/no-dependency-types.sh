#!/usr/bin/env bash
# Usage: ci/no-dependency-types.sh <public-api snapshot...>
# Fails if a public API names a type from outside std and this workspace: the log's types stay
# private to ironflowx-cell (architecture.md, "Depends on"), and neither the read store, the runtime
# nor the wire's libraries may leak through a signature or a trait impl, where a version bump
# becomes a breaking change. The one exception is the router a door returns, in an unpublished
# crate.
set -euo pipefail
# perl: the path's root is what no path and no name precedes. Its $ARGV is perl's.
# shellcheck disable=SC2016
crates=$(cd "$(dirname "$0")/../crates" && printf '%s|' * | tr - _)
found=$(CRATES=${crates%|} perl -ne '
  my $line = $_;
  $line =~ s/-> axum::routing::Router$// if /^pub fn ironflowx_doors::router</;
  while ($line =~ /(?<![\w:])([a-z_]\w*)::/g) {
    next if $1 =~ /^(core|alloc|std|$ENV{CRATES})$/;
    print "$ARGV:$.: $_";
    last;
  }
  close ARGV if eof' "$@")
if [ -n "$found" ]; then
  echo "public API naming a dependency's type (wrap it in a private type):"
  echo "$found"
  exit 1
fi
