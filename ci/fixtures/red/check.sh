#!/usr/bin/env bash
# Fixture for ci/red.sh: a "test" that fails, naming itself, unless value.txt says ok.
grep -qx ok "$(dirname "$0")/value.txt" || { echo "thread 'value_is_ok' panicked"; exit 1; }
