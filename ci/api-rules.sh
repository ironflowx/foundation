#!/usr/bin/env bash
# Usage: ci/api-rules.sh <public-api snapshot...>
# Fails if the public API breaks one of two rules over the snapshots. Each is an allowlist, so a
# new shape fails until it is reviewed:
# - TEN-8: an Address only goes out: no function, function pointer or callable takes one
#   (ci/takes.pl) but its own std methods, it has no method but `new` and std ones, and no impl
#   mentions it but its own derives (so nothing turns an Address into an ID);
# - TEN-4: KeyDigest's bytes stay private: its tuple field is hidden, its methods are ct_eq, the
#   storage codec and std ones, its only impls are Clone and Debug, and nothing else takes one.
# Neither type has a public alias, which a signature would name instead.
set -euo pipefail
found=$(perl -ne '
  BEGIN { require shift }
  my $std = qr/^(clone|clone_from|eq|ne|fmt|hash|assert_fields_are_eq|assert_receiver_is_total_eq)$/;
  print "TEN-8: $_" if /^pub type .* = .*\bAddress\b/;
  print "TEN-4: $_" if /^pub type .* = .*\bKeyDigest\b/;
  # TEN-8
  if (/\bfn\s+[\w:]*\bAddress::(\w+)[<(]/) {
    print "TEN-8: $_" unless $1 =~ $std || $1 eq "new";
  } elsif (takes_type($_, "Address")) {
    print "TEN-8: $_";
  }
  if (/^impl\b/ && /\bAddress\b/) {
    print "TEN-8: $_" unless /^impl (core::(clone::Clone|cmp::Eq|cmp::PartialEq|fmt::Debug|fmt::Display|hash::Hash|marker::StructuralPartialEq) for )?ironflowx_core::Address$/;
  }
  # TEN-4
  print "TEN-4: $_" if /^pub struct [\w:]*\bKeyDigest\b/ && !/^pub struct ironflowx_authz::KeyDigest\(_\)$/;
  print "TEN-4: $_" if /^pub [\w:]*\bKeyDigest::\w+:/;
  if (/\bfn\s+[\w:]*\bKeyDigest::(\w+)[<(]/) {
    print "TEN-4: $_" unless $1 =~ $std || $1 =~ /^(ct_eq|to_stored|from_stored)$/;
  } elsif (takes_type($_, "KeyDigest")) {
    print "TEN-4: $_";
  }
  if (/^impl\b/ && /\bKeyDigest\b/) {
    print "TEN-4: $_" unless /^impl (core::(clone::Clone|fmt::Debug) for )?ironflowx_authz::KeyDigest$/;
  }
' "$(cd "$(dirname "$0")" && pwd)/takes.pl" "$@")
if [ -n "$found" ]; then
  echo "public API breaks a rule:"
  echo "$found"
  exit 1
fi
