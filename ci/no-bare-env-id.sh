#!/usr/bin/env bash
# Usage: ci/no-bare-env-id.sh <public-api snapshot...>
# Fails if a public function takes an EnvironmentId without its token (LED-9): as a parameter
# anywhere in its parameter list, or as `self` in any method of EnvironmentId other than its
# std trait impls. Also fails on a public field or enum variant holding one, since a request
# type would carry it in, and on a public type alias of one, which a signature would name instead
# (a private alias or a renamed import does not show: the snapshot names the type itself).
set -euo pipefail
found=$(perl -ne '
  next unless /^pub\b/;
  print if /^pub type .* = .*\bEnvironmentId\b/;
  if (!/\bfn\s/) {
    # A field, tuple field or variant: `pub a::B::f: T`, `pub a::B::V(T)`, `pub struct a::B(pub T)`.
    print if /EnvironmentId/ && !/^pub (use|mod|type|impl) / && !/^pub struct ironflowx_core::EnvironmentId\b/;
    next;
  }
  # The name, then the balanced parameter list: a closure parameter may hold its own "->".
  next unless /\bfn\s+([^\s(<]+)[^(]*(\((?:[^()]++|(?2))*\))/;
  my ($name, $params) = ($1, $2);
  if ($name =~ /^ironflowx_core::EnvironmentId::(\w+)$/) {
    print unless $1 =~ /^(eq|ne|fmt|hash|clone|clone_from|from_str|assert_fields_are_eq|assert_receiver_is_total_eq)$/;
  } elsif ($params =~ /EnvironmentId/) {
    print;
  }
' "$@")
if [ -n "$found" ]; then
  echo "public API taking a bare EnvironmentId (take an EnvRef):"
  echo "$found"
  exit 1
fi
