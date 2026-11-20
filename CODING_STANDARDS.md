# Coding standards

Judgement rules for review. Anything a tool can check belongs in CI instead (`ci/README.md`).

- **Specs stay true.** A change that departs from a line in a spec, `architecture.md` or
  `invariants.md` updates that line in the same PR.
- **Checks prove themselves.** A new CI check lands with a fixture that triggers every pattern it
  claims to catch, and `must-reject.sh` asserts it fails for that reason, not any failure.
- **Rules are probed at their edges.** For code that accepts or refuses input, the reviewer runs
  each limit's boundary (max, max+1, escaped forms) and the dependency features a downstream
  build can switch on, rather than reading the tests.
- **Tables are tested by row.** For each table in `architecture.md` that maps an input or a state
  to an outcome (the Open table, the Errors table), every row has a test that produces that state
  and checks the outcome.
