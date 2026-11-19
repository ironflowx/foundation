#!/usr/bin/env bash
# Usage: ci/no-bare-env-id.sh <public-api snapshot...>
# Fails if a public function takes an EnvironmentId without its token (LED-9): as a parameter
# anywhere in its parameter list, or as `self` in any method of EnvironmentId other than its
# std trait impls. A struct with a public EnvironmentId field is not caught.
set -euo pipefail
found=$(perl -ne '
  next unless /^pub\b/;
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
  echo "public functions taking a bare EnvironmentId (take an EnvRef):"
  echo "$found"
  exit 1
fi
