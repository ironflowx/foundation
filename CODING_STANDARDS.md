# Coding standards

Judgement rules for review. Anything a tool can check belongs in CI instead (`ci/README.md`).

- **Docs stay true.** A change that departs from a line in `docs/` (`architecture.md`,
  `decisions.md`, `invariants.md`, the behaviors) updates that line in the same PR. A new decision
  gets the next D-number in `decisions.md`, or the one its milestone's PRD issue assigned; an
  overturned one is marked superseded, not deleted.
  The issue that planned the work is not updated after it closes.
- **Assertions can fail.** For each new or changed assertion, the reviewer names the defect it
  catches and the nearest defect it lets through. One that no defect can fail is deleted; one
  looser than its case allows is tightened: when the test knows the exact expected value, the
  assertion compares to it, not `>=`, "non-decreasing" or "is some". Generalizing a shared test
  helper keeps each existing caller's assertions as strict as before.
- **Test checks land red.** A new check in a test is shown failing against the defect named for
  it (a one-line change to the code under test, then reverted), and the PR says which change
  turned it red. `ci/red.sh` applies the change, requires the failure, and only the failure, to
  name the check, restores the file and prints the PR's table row. The other way round, each
  condition the change adds to the code under test (every SQL conjunct, every error branch, each
  `?` included, its error mapped to another outcome) is dropped once: a survivor gets a test, or
  the PR says why no test can see it.
- **Interleavings are named.** For a decision made from more than one read (a row and the clock,
  two rows, a row and a lock), the reviewer states what another call can commit between the reads,
  and names the lock, the transaction or the test that closes it. A gap nothing closes is a defect.
  A test of an interleaving waits for an event at the contested step (`ironflowx-store`'s
  `tests/writes` shows a writer's `BEGIN IMMEDIATE`), never for a length of silence, and stays red
  with the second party 350 ms late.
- **Claims say how they are known.** A finding, or a line of a PR, that a test cannot pass or that
  something never happens is marked *run* or *read*. A probe that met no error says how many times
  it tried.
- **Checks prove themselves.** A new CI check lands with a fixture that triggers every pattern it
  claims to catch, and `must-reject.sh` asserts it fails for that reason, not any failure.
- **Rules are probed at their edges.** For code or a CI check that accepts or refuses input, the
  reviewer runs each limit's boundary (max, max+1, escaped forms) and the dependency features a
  downstream build can switch on, rather than reading the tests. For a CI check, that means
  throwaway fixtures that try to get past it, and legitimate cases it should still pass. The
  review lists every bypass tried and its outcome. A CI check, or a test that searches files and
  output for a secret, stops accidents, not evasion: a bypass that gets through one is a defect to
  fix in this PR, whatever an older check shares the gap, if a contributor would plausibly write it
  without trying to get past the check. Any other bypass that gets through one is listed in a code
  comment on that check, and the PR shows this repo does not have it. So is code the check wrongly
  refuses that this repo does not have; if this repo has it, the check is fixed in this PR. A
  bypass that gets through anything else, product code included, is a defect to fix in this PR. If
  the author and a reviewer, or two reviewers, disagree on which kind a bypass is, the maintainer
  decides.
- **Limits leave with proof.** A change that deletes a documented limit, caveat or failure cause
  includes a test showing it no longer happens. Without one, the line stays.
- **Tables are tested by row.** For each table in `architecture.md` that maps an input or a state
  to an outcome (the Open table, the Errors table), every row has a test that produces that state
  and checks the outcome.
