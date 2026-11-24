# foundation

[![crates.io](https://img.shields.io/crates/v/ironflowx)](https://crates.io/crates/ironflowx)
[![docs.rs](https://img.shields.io/docsrs/ironflowx)](https://docs.rs/ironflowx)
[![CI](https://github.com/ironflowx/foundation/actions/workflows/ci.yml/badge.svg?branch=main&event=push)](https://github.com/ironflowx/foundation/actions/workflows/ci.yml?query=branch%3Amain)
[![MSRV](https://img.shields.io/crates/msrv/ironflowx)](https://github.com/ironflowx/foundation/blob/main/Cargo.toml)
[![License: MIT](https://img.shields.io/crates/l/ironflowx)](https://github.com/ironflowx/foundation/blob/main/LICENSE)

An experimental MIT-licensed rewrite of the Ironflow engine. Not the shipping product; Ironflow 1.x is under the Ironflow License.

Foundation crates are 0.x with no semver-stability promise.

## Purpose

Every part of the engine writes through one commit path. Foundation is that path, proven on its own: the crates any Ironflow X product links to commit one environment's events durably, in order, and read them back.

![Foundation's layers: a host calls the engine with a key; the engine checks the key against the control plane, then runs event calls on the ledger, which commits events durably and reads them back; an embedder without keys calls the ledger directly; beneath them are the pure credentials, event rules and core types](https://raw.githubusercontent.com/ironflowx/foundation/main/docs/img/layers.svg)

## What 0.2 is

In `0.2.0`: an engine that admits every call by its key, in front of one environment's ledger, linked into your process.

- Open an engine in a directory. The first open bootstraps it and returns an admin key, or takes the one you supply.
- Create keys scoped `read`, `write` or `admin`, read-only unless asked otherwise, with an optional expiry; list them, and revoke one.
- Emit an event, or a batch of up to 100, with data, metadata, a version and an idempotency key; each records the key that emitted it.
- Repeat an emit with its idempotency key and get the original event's ID back, with nothing new stored.
- Read an event by ID, or list events newest first, as soon as any emit returns.
- Reopen after a crash with nothing acknowledged lost, and recover an admin key from the data directory when the last one is lost.

An app that needs no keys opens a ledger directly.

## Architecture

The engine is linked, not run: a host opens it on a data directory, and every call to it carries a key. An embedder that needs no keys opens a ledger in a directory instead. Event rules, credentials and core are pure; only the ledger, the control plane and the engine touch disk, the clock and randomness.

A write is acknowledged once its record is fsynced, and a read sees every write acknowledged before it arrived, whoever made it:

![Emit, then read your write](https://raw.githubusercontent.com/ironflowx/foundation/main/docs/img/flow-emit-read.svg)

More diagrams (crates, modules and seams, crash recovery) and the details are in [`docs/architecture.md`](https://github.com/ironflowx/foundation/blob/main/docs/architecture.md); the decisions behind them are in [`docs/decisions.md`](https://github.com/ironflowx/foundation/blob/main/docs/decisions.md).

| Crate | Contents |
| --- | --- |
| ironflowx-core | IDs, the environment reference, attribution, scopes, addresses, secrets, stored and event-clock time, the clock trait, error kinds |
| ironflowx-events | event rules: what an event is and when an emit is refused |
| ironflowx-proto | the Operations interface and its request and answer types |
| ironflowx-cell | the ledger: one environment's durable commit log and read store |
| ironflowx-authz | credentials: the key format, and whether a key is valid and may make a call |
| ironflowx-store | the control plane: which tenants, projects, environments and keys exist |
| ironflowx-operations | the engine: admits every call, then runs it on the ledger or the keys |
| ironflowx | every foundation crate, re-exported |

## Use it

Depend on the facade, with `tokio` for the runtime the engine and the ledger run on:

```toml
[dependencies]
ironflowx = "0.2"
tokio = { version = "1", default-features = false, features = ["rt"] }
```

[`examples/embed`](https://github.com/ironflowx/foundation/blob/v0.2.0/examples/embed/src/main.rs) bootstraps an engine, emits an event, reads it back with a read-only key and sees that key's emit refused. A 0.1 ledger directory does not open in 0.2 ([changelog](https://github.com/ironflowx/foundation/blob/main/CHANGELOG.md)).
