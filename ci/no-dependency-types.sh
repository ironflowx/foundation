#!/usr/bin/env bash
# Usage: ci/no-dependency-types.sh <public-api snapshot...>
# Fails if a public API names a type from outside std and this workspace: the log's types stay
# private to ironflowx-cell (architecture.md, "Depends on"), and neither the read store, the runtime
# nor the wire's libraries may leak through a signature or a trait impl, where a version bump
# becomes a breaking change. The exceptions, in unpublished crates, are the router a door returns
# and the command line the CLI shares with the host. A re-export's line does not say whose type it
# is, so each is named here: ours. Not caught: one of those names pointed at a dependency's type.
set -euo pipefail
crates=$(cd "$(dirname "$0")/../crates" && printf '%s|' * | tr - _)
# perl: the path's root is what no path and no name precedes. Its $ARGV is perl's.
# shellcheck disable=SC2016
found=$(CRATES=${crates%|} perl -ne '
  my $line = $_;
  if (/^pub (use|extern crate) /
      && !/^pub use ironflowx::(cell|core|operations|proto)$/
      && !/^pub use ironflowx_cell::(Emit|Event|EventPage|ItemOutcome|Refusal|Submission)$/
      && !/^pub use ironflowx_operations::(Fault|HostClock|LedgerState|OpenError|RecoverError|SeedState|StoreError)$/) {
    print "$ARGV:$.: $_";
    next;
  }
  $line =~ s/-> axum::routing::Router$// if /^pub fn ironflowx_doors::router</;
  $line =~ s/\bclap_builder::(builder::command::Command|parser::matches::arg_matches::ArgMatches)\b//g
    if /^pub fn ironflowx_cli::(parse|run)\(/;
  while ($line =~ /(?<![\w:])([a-z_]\w*)::/g) {
    next if $1 =~ /^(core|alloc|std|$ENV{CRATES})$/;
    print "$ARGV:$.: $_";
    last;
  }
  close ARGV if eof' "$@")
if [ -n "$found" ]; then
  echo "public API naming a dependency's type (wrap it in a private type), or a re-export that" \
    "$0 does not name:"
  echo "$found"
  exit 1
fi
