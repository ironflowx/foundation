# Spec 1: Core types and the CI skeleton

Build-order step 1 of [0.1 Ledger](../README.md). Nothing comes before it.

## Problem Statement

Every later module of the foundation needs the same IDs, time types and error kinds, and none exist. The core crate instead holds a hierarchical resource-name type, which contradicts the rule that an environment ID is opaque (LED-10). The repo also has no CI, so nothing stops a pure crate from gaining I/O, or a public type from gaining a constructor that breaks an invariant.

## Solution

The core crate ships validated opaque IDs, the environment reference, one stored-time type, one event-clock time type, the clock trait and the error kinds. The resource-name type is deleted. CI runs on Linux and macOS and enforces lints, licences, purity of the pure crates and the shape of the public API, each check proven by a fixture it must reject.

## User Stories

1. As an embedder, I want an environment ID that only parses from a UUIDv7, so that a path-like string, a UUIDv4 or an empty string can never name an environment.
2. As an embedder, I want an event ID that is a validated UUIDv7, so that every resource has one opaque ID format.
3. As an embedder, I want an environment reference that always pairs an environment ID with its incarnation token, so that I cannot address an environment by its ID alone.
4. As an embedder, I want the environment reference to have a text form (`<uuid>.<32 hex digits>`) that parses back to the same value, so that I can store it.
5. As an embedder, I want a malformed environment reference to be refused on parse, so that a bad stored value fails early.
6. As a maintainer, I want the incarnation token to have private fields and no constructor other than parsing its text form, so that a token cannot be invented from other data.
7. As a maintainer, I want no public function in the foundation to take a bare environment ID without its token, so that work is never looked up by ID alone (LED-9).
8. As an implementer of later modules, I want one stored-time type, a UTC instant in whole milliseconds, so that every module stores time the same way.
9. As a maintainer, I want that type to convert only to and from one `i64` millisecond value, so that there is a single encoding (LED-13).
10. As an implementer, I want the stored-time type to order correctly across its whole range, extremes included, so that sorting by it is safe.
11. As an implementer of event rules, I want an event-clock time type that pure code can receive, so that event rules never read a clock.
12. As a maintainer, I want event-clock time to compare only with itself, so that it cannot be mixed with stored time or the system clock by mistake (LED-1).
13. As a maintainer, I want the constructor of event-clock time hidden from documentation and banned by a lint outside the ledger crate, so that only the ledger produces event time.
14. As an implementer, I want a clock trait declared in core, which core never calls, so that the ledger and tests can supply clocks.
15. As a caller, I want four error kinds (`Invalid`, `NotFound`, `Unknown`, `Unavailable`) defined once, so that every module and door reports failures the same way.
16. As a maintainer, I want the resource-name type removed, so that the crate holds one ID model.
17. As a maintainer, I want clippy with warnings denied, `cargo-deny` and the tests to run on Linux and macOS for every PR, so that the "done when" checks exist from the first PR.
18. As a maintainer, I want a dependency allowlist for each pure crate, checked from `cargo tree` for that crate alone, so that a pure crate cannot gain an async runtime or an I/O crate (LED-15).
19. As a maintainer, I want lints that ban the ambient clock, the filesystem, the network, threads and OS randomness in pure crates, so that purity is checked in code, not only in dependencies.
20. As a maintainer, I want each purity check to come with a fixture it must reject, so that a check that silently stops working is noticed.
21. As a maintainer, I want the public API of each foundation crate compared with a committed snapshot, so that a new constructor or conversion is a visible, reviewed change (LED-6, LED-9, LED-13).
22. As an embedder, I want the facade to keep re-exporting core, so that one dependency still gives me the foundation.

## Implementation Decisions

- **Module:** core types, in the core crate. It is pure: no I/O, no async runtime, no ambient clock, no randomness. It may depend on the `uuid` crate for parsing and validation only, with no generation feature.
- **IDs:** environment ID and event ID are newtypes over a UUIDv7; parsing refuses every other version and form. Core does not generate IDs; the ledger does (Spec 4).
- **Incarnation token:** 128 bits, private fields, constructed only by parsing its text form. The ledger mints one by drawing random bytes and parsing their text form, so core needs no second constructor.
- **Environment reference:** environment ID and incarnation token, always together, with `Display` and `FromStr` for the text form.
- **Stored time (`Timestamp`):** private fields; `from_millis` and `as_millis` over `i64` are its only conversions (D18). No SQLite trait impls in core.
- **Event-clock time (`SequencedTime`):** defined in core so pure event rules can take one. It implements ordering against itself only. Its single constructor is `#[doc(hidden)]`; clippy `disallowed-methods` bans it in every crate except the ledger crate (D17).
- **Clock:** a trait with one method returning the stored-time type. Core declares it and never calls it.
- **Errors:** the four error kinds live in core. Messages belong to the modules that produce them.
- **Removal:** the resource-name type and its tests are deleted outright, with no deprecation step. The crate description and the repo README's crate table are updated.
- **CI:** one workflow, Linux and macOS. `cargo-deny` runs once, on Linux, and checks both platforms' dependency graphs through the targets in `deny.toml`. The allowlist reads `cargo tree` for one crate, not `cargo metadata`, which unifies features across the workspace: the ledger's `uuid` `v7` would otherwise leak `getrandom` into core. Jobs: format, clippy with warnings denied, tests, `cargo-deny`, the purity checks, and the public-API snapshot. The snapshot job alone uses a pinned nightly toolchain (D21); every other job uses the pinned stable.
- **Purity checks cover** core now and the event-rules crate as soon as it exists.

## Testing Decisions

- A good test here checks what a user of the types can observe: what parses, what is refused, what compiles. It does not reach into private fields.
- **Parse tests:** a UUIDv4, a path-like string and the empty string are refused as environment IDs; the environment reference round-trips through its text form.
- **Round-trip and ordering test** for stored time over the extremes of `i64`.
- **Compile-fail tests:** comparing event-clock time with stored time, and with the system clock, must not compile.
- **Lint fixture:** a crate that calls the event-clock constructor outside the ledger must fail the lint.
- **Purity fixtures:** one crate with a forbidden dependency and one with a forbidden call must each fail their check.
- **Public-API snapshot:** any difference from the committed snapshot fails.
- **Prior art:** none in this repo; the only existing tests belong to the type being deleted.

## Out of Scope

- Generating IDs or tokens, and reading any clock: the ledger does both (Spec 4).
- The SQLite encoding of stored time (Spec 4).
- The parent-as-a-field half of LED-10, which arrives with tenancy in 0.2.
- Windows.

## Further Notes

- Invariants proven here: LED-1 (comparison half and lint), LED-9, LED-10, LED-13, LED-15, and the public-API half of LED-6. See [`../invariants.md`](../invariants.md).
- The types are described in [`../architecture.md`](../architecture.md#modules).
