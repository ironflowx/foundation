# Coding standards

Judgement rules for review. Anything a tool can check belongs in CI instead (`ci/README.md`).

- **Specs stay true.** A change that departs from a line in a spec, `architecture.md` or
  `invariants.md` updates that line in the same PR.
- **Checks prove themselves.** A new CI check lands with a fixture that triggers every pattern it
  claims to catch, and `must-reject.sh` asserts it fails for that reason, not any failure.
