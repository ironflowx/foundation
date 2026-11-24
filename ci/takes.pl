# For the checks over public-API snapshots: what a function takes in.
use strict;
use warnings;

# A function's line less what it returns: its name, its generic parameters and their bounds, its
# parameters and its where clause. Undefined for any other line.
sub takes {
  my ($line) = @_;
  my ($rest) = $line =~ /\bfn\s+(.*)$/ or return undef;
  my ($depth, $returns, $out) = (0, 0, "");
  for my $token ($rest =~ /->|\bwhere\b|./g) {
    if ($token eq "->" || $token eq "where") {
      $returns = $token eq "->" if $depth == 0;
    } elsif ($token =~ /[(\[<{]/) {
      $depth++;
    } elsif ($token =~ /[)\]>}]/) {
      $depth--;
    }
    $out .= $token unless $returns;
  }
  return $out;
}

# Whether a line takes <type>: in a function's line as above, or as a parameter of a function
# pointer or a callable bound anywhere in the line, what it returns included.
sub takes_type {
  my ($line, $type) = @_;
  return 1 if (takes($line) // "") =~ /\b$type\b/;
  while ($line =~ /\b(?:fn|\w*Fn\w*)(\((?:[^()]++|(?1))*\))/g) {
    return 1 if $1 =~ /\b$type\b/;
  }
  return 0;
}

1;
