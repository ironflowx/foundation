#!/usr/bin/env bash
# Usage: ci/api-rules.sh <public-api snapshot...>
# Fails if the public API breaks one of three rules over the snapshots. Each is an allowlist, so a
# new shape fails until it is reviewed:
# - TEN-3: no error type (one with an `impl Error`, or named *Error, *Refused, *Malformed) has a
#   field or variant holding a Secret, or a type that holds one, however deeply;
# - TEN-8: an Address only goes out: no function takes one but its own std methods, it has no
#   method but `new` and std ones, and no impl mentions it but its own derives (so nothing turns
#   an Address into an ID);
# - TEN-4: KeyDigest's bytes stay private: its tuple field is hidden, its methods are ct_eq, the
#   storage codec and std ones, its only impls are Clone and Debug, and no other function takes
#   one and returns bytes.
set -euo pipefail
found=$(perl -e '
  my @lines = map { chomp; $_ } <>;
  my $last = sub { my ($p) = @_; $p =~ s/^.*:://; $p };
  # TEN-3: members (fields, tuple fields, variants) by owner type.
  my (%members, %errors);
  for (@lines) {
    $errors{$1} = 1 if /^impl (?:core|std)::error::Error for ([\w:]+)$/;
    if (/^pub (?!fn |const |struct |enum |mod |use |trait |type |impl )([\w:]+)::(\w+)[(:]\s*(.*)$/) {
      push @{$members{$1}}, [$_, $3];
    } elsif (/^pub struct ([\w:]+)\((.+)\)$/) {
      push @{$members{$1}}, [$_, $2];
    }
  }
  my %holds = (Secret => 1);
  for my $round (1 .. 10) {
    for my $owner (keys %members) {
      for my $m (@{$members{$owner}}) {
        for my $h (keys %holds) { $holds{$last->($owner)} = 1 if $m->[1] =~ /\b\Q$h\E\b/ }
      }
    }
  }
  for my $owner (keys %members) {
    # A variant struct field: its owner path ends in the variant.
    my $type = (grep { $errors{$_} } ($owner, $owner =~ s/::\w+$//r))[0]
      // ($owner =~ /(?:Error|Refused|Malformed)(?:::\w+)?$/ ? $owner : undef);
    next unless defined $type;
    for my $m (@{$members{$owner}}) {
      for my $h (keys %holds) {
        if ($m->[1] =~ /\b\Q$h\E\b/) { print "TEN-3: $m->[0]\n"; last }
      }
    }
  }
  my $std = qr/^(clone|clone_from|eq|ne|fmt|hash|assert_fields_are_eq|assert_receiver_is_total_eq)$/;
  for (@lines) {
    # TEN-8
    if (/\bfn\s+([\w:]+)/) {
      my $name = $1;
      if ($name =~ /\bAddress::(\w+)$/) {
        print "TEN-8: $_\n" unless $1 =~ $std || $1 eq "new";
      } elsif (/\bAddress\b/ && /\bfn\s+[\w:]+(?:<[^(]*>)?\s*\(([^)]*\bAddress\b)/) {
        print "TEN-8: $_\n";
      }
    }
    if (/^impl\b/ && /\bAddress\b/) {
      print "TEN-8: $_\n" unless /^impl (core::(clone::Clone|cmp::Eq|cmp::PartialEq|fmt::Debug|fmt::Display|hash::Hash|marker::StructuralPartialEq) for )?ironflowx_core::Address$/;
    }
    # TEN-4
    print "TEN-4: $_\n" if /^pub struct [\w:]*\bKeyDigest\b/ && !/^pub struct ironflowx_authz::KeyDigest\(_\)$/;
    print "TEN-4: $_\n" if /^pub [\w:]*\bKeyDigest::\w+:/;
    if (/\bfn\s+[\w:]*\bKeyDigest::(\w+)\b/) {
      print "TEN-4: $_\n" unless $1 =~ $std || $1 =~ /^(ct_eq|to_stored|from_stored)$/;
    } elsif (my ($params, $ret) = /\bfn\s+[\w:]+(?:<[^(]*>)?\s*\(([^)]*)\)\s*->\s*(.*)$/) {
      print "TEN-4: $_\n" if $params =~ /\bKeyDigest\b/ && $ret =~ /\bu8\b/;
    }
    if (/^impl\b/ && /\bKeyDigest\b/) {
      print "TEN-4: $_\n" unless /^impl (core::(clone::Clone|fmt::Debug) for )?ironflowx_authz::KeyDigest$/;
    }
  }
' "$@")
if [ -n "$found" ]; then
  echo "public API breaks a rule:"
  echo "$found"
  exit 1
fi
