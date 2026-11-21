#!/usr/bin/env bash
# Usage: ci/doc-pins.sh
# The doc-pins job: the docs name only the segment-log version Cargo.toml pins, and the check
# still rejects its fixture.
set -euo pipefail
cd "$(dirname "$0")/.."
ci/check-pins.sh crates/ironflowx-cell/Cargo.toml docs crates README.md GLOSSARY.md
ci/must-reject.sh "ci/fixtures/stale-pin/doc.md:3:" "ci/fixtures/stale-pin/doc.md:5:" -- \
  ci/check-pins.sh ci/fixtures/stale-pin/Cargo.toml ci/fixtures/stale-pin/doc.md
