#!/usr/bin/env bash
# Usage: containers.sh <spike, musl> <spike, glibc> <ironflowx, musl>. Each case of #160 that needs
# a container with no glibc, its output printed; fails at the first that is not as named here.
set -euo pipefail
musl=$1 glibc=$2 ironflowx=$3
work=$(mktemp -d)
trap 'docker rm -f sp160-serve >/dev/null 2>&1 || true; kill "$server" 2>/dev/null || true' EXIT
server=
public=https://index.crates.io/config.json
bundle=/etc/ssl/certs/ca-certificates.crt
alpine=alpine:3.22

cp "$musl" "$work/spike"; cp "$glibc" "$work/spike-glibc"; cp "$ironflowx" "$work/ironflowx"
printf 'FROM scratch\nCOPY spike spike-glibc ironflowx /\n' >"$work/Dockerfile"
docker build -q -t sp160-scratch "$work"
docker pull -q "$alpine"; docker image inspect -f '{{.RepoDigests}} {{.Architecture}}' "$alpine"

fail() { echo "FAIL: $1"; exit 1; }
# <name> <pattern in the output> -- <command...>: it must exit 0.
passes() { run 0 "$@"; }
# The same, and it must exit 1: not 125 to 127, which are docker's own.
refuses() { run 1 "$@"; }
run() {
  local want=$1 name=$2 pattern=$3 code=0 out; shift 4
  echo "== $name"; echo "\$ $*"
  out=$("$@" 2>&1) || code=$?
  echo "$out"; echo "exit $code"
  [ "$code" -eq "$want" ] || fail "$name: exit $code, not $want"
  grep -qF -- "$pattern" <<<"$out" || fail "$name: no '$pattern'"
  echo "OK: $name"
}
scratch=(docker run --rm --network host --tmpfs /data)
in_alpine=(docker run --rm --network host --tmpfs /data -v "$work/spike:/spike:ro" -v "$work/spike-glibc:/spike-glibc:ro")

echo "== no glibc in either container"
id=$(docker create sp160-scratch /spike); docker export "$id" | tar -t; docker rm "$id" >/dev/null
passes "alpine has no glibc" "glibc files: 0" -- docker run --rm "$alpine" sh -c \
  'echo "glibc files: $(find / -xdev \( -name "ld-linux*" -o -name "libc.so.6" -o -name "libc-2*" \) | wc -l)"; ls /lib/ld-* ; cat /etc/alpine-release'
passes "alpine's ldd" "spike" -- "${in_alpine[@]}" "$alpine" ldd /spike
# Docker's own code for a program it cannot start.
echo "== control: the glibc build starts in neither"
for where in "${scratch[*]} sp160-scratch" "${in_alpine[*]} $alpine"; do
  # shellcheck disable=SC2086
  if out=$($where /spike-glibc engine /data 2>&1); then fail "the glibc build ran in $where"; fi
  echo "$out" | tail -1; grep -q 'no such file or directory' <<<"$out" || fail "not for want of its loader"
done

passes "engine, scratch" "engine: reopened, read order.placed" -- "${scratch[@]}" sp160-scratch /spike engine /data
passes "engine, alpine" "engine: reopened, read order.placed" -- "${in_alpine[@]}" "$alpine" /spike engine /data
passes "server and client over HTTP, scratch" "serve: /health answered 200 OK" -- "${scratch[@]}" sp160-scratch /spike serve

echo "== a CA no store trusts, and a server it signed for localhost"
cd "$work"
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 2 -subj '/CN=spike test root' \
  -addext 'basicConstraints=critical,CA:TRUE' -addext 'keyUsage=critical,keyCertSign' -keyout ca.key -out ca.pem 2>/dev/null
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj '/CN=localhost' -keyout leaf.key -out leaf.csr 2>/dev/null
printf 'subjectAltName=DNS:localhost\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n' >ext
openssl x509 -req -in leaf.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 2 -extfile ext -out leaf.pem 2>/dev/null
openssl s_server -accept 9443 -cert leaf.pem -key leaf.key -www >server.log 2>&1 & server=$!
for _ in $(seq 50); do curl -s --cacert ca.pem -o /dev/null https://localhost:9443/ && break; sleep 0.1; done
curl -s --cacert ca.pem -o /dev/null -w 'curl, with the test root: %{http_code}\n' https://localhost:9443/
private=https://localhost:9443/
unknown='rustls: InvalidCertificate(UnknownIssuer)'
ca=(-v "$work/ca.pem:/ca.pem:ro" -e SSL_CERT_FILE=/ca.pem)

passes  "alpine's roots, a public server: trusted" "answered HTTP/2.0 200 OK" -- "${in_alpine[@]}" "$alpine" /spike https "$public"
refuses "alpine's roots, the test server: refused" "$unknown" -- "${in_alpine[@]}" "$alpine" /spike https "$private"
passes  "the test root, the test server: trusted" "200" -- "${in_alpine[@]}" "${ca[@]}" "$alpine" /spike https "$private"
refuses "the test root, a public server: refused" "$unknown" -- "${in_alpine[@]}" "${ca[@]}" "$alpine" /spike https "$public"
refuses "the test root, the test server by another name: refused" "NotValidForName" -- "${in_alpine[@]}" "${ca[@]}" "$alpine" /spike https https://127.0.0.1:9443/
passes  "the runner's roots, a public server: trusted" "200 OK" -- ./spike https "$public"
refuses "the runner's roots, the test server: refused" "$unknown" -- ./spike https "$private"

refuses "no store, a public server: refused" "$unknown" -- "${scratch[@]}" sp160-scratch /spike https "$public"
passes  "  which found no root and no error" "https: native roots 0, errors []" -- sh -c "${scratch[*]} sp160-scratch /spike https $public; true"
refuses "no store, the test server: refused" "$unknown" -- "${scratch[@]}" sp160-scratch /spike https "$private"
refuses "SSL_CERT_FILE names no file: refused" "kind: NotFound" -- "${scratch[@]}" -e SSL_CERT_FILE=/missing sp160-scratch /spike https "$public"
passes  "control, scratch given the roots by SSL_CERT_FILE: trusted" "200 OK" -- \
  "${scratch[@]}" -v "$bundle:/roots.pem:ro" -e SSL_CERT_FILE=/roots.pem sp160-scratch /spike https "$public"
passes  "control, scratch given the roots at $bundle: trusted" "200 OK" -- \
  "${scratch[@]}" -v "$bundle:$bundle:ro" sp160-scratch /spike https "$public"
passes  "control, scratch given the test root, the test server: trusted" "200" -- \
  "${scratch[@]}" "${ca[@]}" sp160-scratch /spike https "$private"

echo "== control: a server that is not there is not read as refused"
if out=$("${scratch[@]}" sp160-scratch /spike https https://localhost:9444/ 2>&1); then fail "it answered"; fi
echo "$out"; if grep -q 'rustls: ' <<<"$out"; then fail "a closed port reads as a TLS refusal"; fi
echo "== the test server saw each refusal"
grep -i alert server.log | sort | uniq -c
grep -qi 'alert unknown ca' server.log || fail "the test server saw no 'unknown ca' alert"

echo "== extra: the ironflowx of main, in scratch"
passes "version" '"protocol"' -- "${scratch[@]}" sp160-scratch /ironflowx version
docker run -d --name sp160-serve --network host --tmpfs /data sp160-scratch /ironflowx serve --data-dir /data/engine --listen 127.0.0.1:9310 >/dev/null
for _ in $(seq 50); do curl -sf -o /dev/null http://127.0.0.1:9310/ready && break; sleep 0.1; done
passes "serve is ready" "ready 200" -- curl -s -o /dev/null -w 'ready %{http_code}\n' http://127.0.0.1:9310/ready
docker logs sp160-serve
echo "ALL OK"
