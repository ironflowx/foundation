#!/usr/bin/env bash
# Usage: ci/api-rules.sh <public-api snapshot...>
# Fails if the public API breaks one of three rules over the snapshots:
# - TEN-3: no error type holds a Secret (a type named *Error, Refused or Malformed with a field
#   or variant of type Secret);
# - TEN-8: nothing turns an Address into an ID (no From<Address> for an ID, no function taking
#   an Address, or a method of Address, that returns an ID or an EnvRef);
# - TEN-4: KeyDigest exposes its bytes only through ct_eq and its storage codec (no other
#   method, and no trait impl but Clone and Debug).
set -euo pipefail
found=$(perl -ne '
  my $id = qr/\b(?:TenantId|ProjectId|EnvironmentId|EventId|KeyId|EnvRef)\b/;
  my $error = qr/\w*(?:Error|Refused|Malformed)\w*/;
  # TEN-3: a field, tuple field or variant of an error type that holds a Secret.
  print "TEN-3: $_" if /^pub [\w:]+::${error}::\w+[(:].*\bSecret\b/
    || /^pub struct [\w:]+::$error\(.*\bSecret\b/;
  # TEN-8
  print "TEN-8: $_" if /^impl\b.*\bFrom<[\w:]*\bAddress>\s+for\s+.*$id/;
  if (/\bfn\s+([\w:]+)\s*(\((?:[^()]++|(?2))*\))\s*->\s*(.*)$/) {
    my ($name, $params, $ret) = ($1, $2, $3);
    print "TEN-8: $_" if ($name =~ /\bAddress::\w+$/ || $params =~ /\bAddress\b/) && $ret =~ $id;
  }
  # TEN-4
  if (/\bfn\s+[\w:]*\bKeyDigest::(\w+)\b/) {
    print "TEN-4: $_" unless $1 =~ /^(ct_eq|to_stored|from_stored|clone|clone_from|fmt)$/;
  }
  if (/^impl\b(.*)\bfor\s+[\w:]*\bKeyDigest\b/) {
    print "TEN-4: $_" unless $1 =~ /^\s*core::(clone::Clone|fmt::Debug)\s*$/;
  }
' "$@")
if [ -n "$found" ]; then
  echo "public API breaks a rule:"
  echo "$found"
  exit 1
fi
