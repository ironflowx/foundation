# Docs

The system as it is now. Each PR that changes it updates these files; milestone plans (PRDs, specs, slices) live in [GitHub issues](https://github.com/ironflowx/foundation/issues), not here.

| File | Read it for |
|---|---|
| [`architecture.md`](architecture.md) | C1, C2, C3, the flows, modules, seams, interfaces, event rules, errors, storage, wire, and where the host and CLI are |
| [`decisions.md`](decisions.md) | Every decision (D1, D2, …), numbered across milestones |
| [`invariants.md`](invariants.md) | Ledger, tenancy, serving and run properties and their proofs, including the crash tests |
| [`behaviors/`](behaviors/README.md) | The executable scenarios, how they run, what is out of scope |
| [`agents/`](agents/) | How agents use the issue tracker, triage labels and these docs |

The terms are in [`GLOSSARY.md`](../GLOSSARY.md).

New topics get a new file or a section here, never a per-milestone folder.
