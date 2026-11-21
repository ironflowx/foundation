## Agent skills

### Issue tracker

Issues and specs live as GitHub issues on `ironflowx/foundation`, through the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

The five default labels: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `GLOSSARY.md` at the root, and one `docs/decisions.md` numbered across milestones. See `docs/agents/domain.md`.

### Docs

Milestone plans (PRDs, specs, slices) live in GitHub issues. `docs/` describes the system as it is now, by topic, never in per-milestone folders. A PR that changes behavior, an interface or a decision updates `docs/` in the same PR. See `docs/README.md`.

## CI

Changing a crate's dependencies or public API, or adding a crate: read `ci/README.md`.

## Review

Reviewing a change: apply `CODING_STANDARDS.md`.
