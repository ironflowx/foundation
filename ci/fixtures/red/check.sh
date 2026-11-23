#!/usr/bin/env bash
# Fixture for ci/red.sh: a "test" that fails, naming itself, unless value.txt says ok. Like
# cargo's, it names itself when it passes too, so its bare name is no evidence of a failure.
grep -qx ok "$(dirname "$0")/value.txt" || { echo "thread 'value_is_ok' panicked"; exit 1; }
echo "test value_is_ok ... ok"
