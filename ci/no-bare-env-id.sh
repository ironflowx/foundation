#!/usr/bin/env bash
# Usage: ci/no-bare-env-id.sh <public-api snapshot...>
# Fails if a public function takes an EnvironmentId without its token (LED-9): anywhere but in what
# it returns (ci/takes.pl), in a function pointer's or a callable's parameters, or as `self` in any
# method of EnvironmentId other than its std trait impls. Also fails on a public field or enum
# variant holding one, since a request type would carry it in, and on a public type alias of one,
# which a signature would name instead (a private alias or a renamed import does not show: the
# snapshot names the type itself).
set -euo pipefail
found=$(perl -ne '
  BEGIN { require shift }
  next unless /^pub\b/ && /\bEnvironmentId\b/;
  if (/^pub type /) {
    print if / = .*\bEnvironmentId\b/;
  } elsif (!/\bfn\s/) {
    # A field, tuple field or variant: `pub a::B::f: T`, `pub a::B::V(T)`, `pub struct a::B(pub T)`.
    print unless /^pub (use|mod|impl) / || /^pub struct ironflowx_core::EnvironmentId\b/;
  } elsif (/\bfn\s+ironflowx_core::EnvironmentId::(\w+)[<(]/) {
    print unless $1 =~ /^(eq|ne|fmt|hash|clone|clone_from|from_str|assert_fields_are_eq|assert_receiver_is_total_eq)$/;
  } else {
    print if takes_type($_, "EnvironmentId");
  }
' "$(cd "$(dirname "$0")" && pwd)/takes.pl" "$@")
if [ -n "$found" ]; then
  echo "public API taking a bare EnvironmentId (take an EnvRef):"
  echo "$found"
  exit 1
fi
