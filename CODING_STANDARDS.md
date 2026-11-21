# Coding standards

Judgement rules for review. Anything a tool can check belongs in CI instead (`ci/README.md`).

- **Docs stay true.** A change that departs from a line in `docs/` (`architecture.md`,
  `decisions.md`, `invariants.md`, the behaviors) updates that line in the same PR. A new decision
  gets the next D-number in `decisions.md`; an overturned one is marked superseded, not deleted.
  The issue that planned the work is not updated after it closes.
- **Checks prove themselves.** A new CI check lands with a fixture that triggers every pattern it
  claims to catch, and `must-reject.sh` asserts it fails for that reason, not any failure.
- **Rules are probed at their edges.** For code that accepts or refuses input, the reviewer runs
  each limit's boundary (max, max+1, escaped forms) and the dependency features a downstream
  build can switch on, rather than reading the tests.
- **Limits leave with proof.** A change that deletes a documented limit, caveat or failure cause
  includes a test showing it no longer happens. Without one, the line stays.
- **Tables are tested by row.** For each table in `architecture.md` that maps an input or a state
  to an outcome (the Open table, the Errors table), every row has a test that produces that state
  and checks the outcome.
