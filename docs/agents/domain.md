# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

## Before exploring, read these

- **`GLOSSARY.md`** at the repo root.
- **`docs/decisions.md`**: one table of decisions (D1, D2, …), numbered across all milestones, each tagged with the milestone that made it. Treat those rows as ADRs. This repo is single-context and has no `docs/adr/`.
- **`docs/architecture.md`** and **`docs/invariants.md`**: the system as it is now, not as one milestone planned it. Milestone plans (PRDs, specs) live in GitHub issues, not in the repo.

If any of these files don't exist, **proceed silently**. Don't flag their absence; don't suggest creating them upfront. The `/domain-modeling` skill (reached via `/grill-with-docs` and `/improve-codebase-architecture`) creates them lazily when terms or decisions actually get resolved; here a decision is a new row in `docs/decisions.md`.

## File structure

This repo is single-context:

```
/
├── GLOSSARY.md
└── docs/
    ├── architecture.md
    ├── decisions.md      ← every decision, D1, D2, …; the next one takes the next number
    └── invariants.md
```

Record a new decision as the next D-row in `docs/decisions.md`, never as a separate ADR file or a `docs/adr/` folder.

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor proposal, a hypothesis, a test name), use the term as defined in `GLOSSARY.md`. Don't drift to synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal: either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for `/domain-modeling`).

## Flag decision conflicts

If your output contradicts an existing decision in `docs/decisions.md`, surface it explicitly rather than silently overriding:

> _Contradicts D8 (refuse a directory whose format version differs), but worth reopening because…_
