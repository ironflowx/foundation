#!/usr/bin/env bash
# Uses the checked Apple silicon archive to exercise platform-independent release checks.
# Usage: ci/release-archive-fixtures.sh <target> <version> <archive>
set -euo pipefail
target=$1 version=$2 archive=$3
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/original" "$work/pack" "$work/untrusted" "$work/extra-scope"
tar -xzf "$archive" -C "$work/original"
fixture="$work/pack/fixture.tar.gz"
verify=ci/verify-release-archive.sh

repack() (
  cd "$work/pack"
  COPYFILE_DISABLE=1 tar -czf fixture.tar.gz -C "$work/original" "$@"
  shasum -a 256 fixture.tar.gz >fixture.tar.gz.sha256
)

repack ironflowx LICENSE
printf 'corrupt' >> "$fixture"
reason="archive SHA-256 checksum does not match"
ci/must-reject.sh "$reason" -- "$verify" "$target" "$version" "$fixture"
repack ironflowx LICENSE
(cd "$work/pack" && shasum -a 512 fixture.tar.gz >fixture.tar.gz.sha256)
ci/must-reject.sh "$reason" -- "$verify" "$target" "$version" "$fixture"

repack ironflowx
ci/must-reject.sh 'unexpected archive members' -- "$verify" "$target" "$version" "$fixture"
repack ironflowx LICENSE LICENSE
ci/must-reject.sh 'unexpected archive members' -- "$verify" "$target" "$version" "$fixture"

printf 'wrong licence\n' >"$work/original/LICENSE"
repack ironflowx LICENSE
ci/must-reject.sh 'archive licence does not match LICENSE' -- "$verify" "$target" "$version" "$fixture"
ci/must-reject.sh 'archive binary did not report version 0.0.0' -- "$verify" "$target" 0.0.0 "$archive"

cat >"$work/cli" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "$1" != whoami ]; then
  exec "$RELEASE_FIXTURE_BINARY" "$@"
fi
case "$RELEASE_FIXTURE_MODE" in
  untrusted) SSL_CERT_FILE=/dev/null exec "$RELEASE_FIXTURE_BINARY" "$@" ;;
  extra-scope) "$RELEASE_FIXTURE_BINARY" "$@" | jq '.scopes += ["extra"]' ;;
esac
SH
chmod +x "$work/cli"
export RELEASE_FIXTURE_BINARY="$work/original/ironflowx"
ci/must-reject.sh '"code":"unavailable"' -- env RELEASE_FIXTURE_MODE=untrusted \
  python3 ci/release-https.py "$work/cli" "$work/untrusted"
ci/must-reject.sh "archive's HTTPS whoami did not authenticate as admin" -- \
  env RELEASE_FIXTURE_MODE=extra-scope python3 ci/release-https.py "$work/cli" "$work/extra-scope"
