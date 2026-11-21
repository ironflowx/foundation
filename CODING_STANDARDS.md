# Coding standards

Judgement rules for review. Anything a tool can check belongs in CI instead (`ci/README.md`).

- **Docs stay true.** A change that departs from a line in `docs/` (`architecture.md`,
  `decisions.md`, `invariants.md`, the behaviors) updates that line in the same PR. A new decision
  gets the next D-number in `decisions.md`; an overturned one is marked superseded, not deleted.
  The issue that planned the work is not updated after it closes.
- **Assertions can fail.** For each new or changed assertion, the reviewer names the defect it
  catches and the nearest defect it lets through. One that no defect can fail is deleted; one
  looser than its case allows is tightened: when the test knows the exact expected value, the
  assertion compares to it, not `>=`, "non-decreasing" or "is some". Generalizing a shared test
  helper keeps each existing caller's assertions as strict as before.
- **Test checks land red.** A new check in a test is shown failing against the defect named for
  it (a one-line change to the code under test, then reverted), and the PR says which change
  turned it red.
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
