#!/usr/bin/env bash
# In a bare ubuntu:24.04, as root, with the checkout at $1: which Debian packages the build needs.
# Adds one at a time; every set but the last must fail, for the reason its line names.
set -uo pipefail
src=$1
target=$(uname -m)-unknown-linux-musl
export DEBIAN_FRONTEND=noninteractive
apt-get -qq update >/dev/null
apt-get -qq install -y --no-install-recommends ca-certificates curl >/dev/null 2>&1
curl -sSf https://sh.rustup.rs | sh -s -- -q -y --profile minimal --default-toolchain none >/dev/null 2>&1
export PATH=$HOME/.cargo/bin:$PATH
cp -r "$src" /build && cd /build/spike/musl-tls || exit 1
rustup toolchain install 1.100.0 --profile minimal --target "$target" >/dev/null 2>&1
export RUSTUP_TOOLCHAIN=1.100.0
cargo fetch --locked --target "$target" 2>/dev/null
tools() {
  for tool in cc gcc g++ make cmake perl go clang bindgen musl-gcc protoc; do
    printf '%s=%s ' "$tool" "$(command -v $tool || echo none)"
  done
  echo
}
build() {
  rm -rf target
  tools
  cargo build --release --locked --target "$target" >/tmp/build.log 2>&1
}
must_fail() { # <set> <reason>
  echo "== $1"
  if build; then echo "packages: built with $1, so the last package is not needed"; exit 1; fi
  if ! grep -qF -- "$2" /tmp/build.log; then
    echo "packages: failed with $1, but not by '$2':"; tail -40 /tmp/build.log; exit 1
  fi
  grep -F -- "$2" /tmp/build.log | head -2
  echo "packages: $1: fails"
}
must_fail "ca-certificates, curl and rustup's" "linker \`cc\` not found"
apt-get -qq install -y --no-install-recommends gcc >/dev/null 2>&1
# GNU ld says "cannot find", rust-lld (x86-64) "cannot open".
must_fail "gcc" " Scrt1.o: No such file or directory"
apt-get -qq install -y --no-install-recommends libc6-dev >/dev/null 2>&1
must_fail "gcc, libc6-dev" "error occurred in cc-rs: failed to find tool \"$(uname -m)-linux-musl-gcc\""
apt-get -qq install -y --no-install-recommends musl-tools >/dev/null 2>&1
echo "== gcc, libc6-dev, musl-tools"
build || { tail -60 /tmp/build.log; exit 1; }
tail -2 /tmp/build.log
dpkg-query -W -f '${Package} ${Version}\n' gcc gcc-13 libc6-dev musl-tools musl-dev
ls -l "target/$target/release/spike"
echo "packages: gcc, libc6-dev, musl-tools: builds"
