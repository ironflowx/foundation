# foundation

[![crates.io](https://img.shields.io/crates/v/ironflowx)](https://crates.io/crates/ironflowx)
[![docs.rs](https://img.shields.io/docsrs/ironflowx)](https://docs.rs/ironflowx)
[![CI](https://github.com/ironflowx/foundation/actions/workflows/ci.yml/badge.svg?branch=main&event=push)](https://github.com/ironflowx/foundation/actions/workflows/ci.yml?query=branch%3Amain)
[![MSRV](https://img.shields.io/crates/msrv/ironflowx)](https://github.com/ironflowx/foundation/blob/main/Cargo.toml)
[![License: MIT](https://img.shields.io/crates/l/ironflowx)](https://github.com/ironflowx/foundation/blob/main/LICENSE)

The experimental, MIT-licensed library crates that Ironflow X is built on.

Foundation crates are 0.x with no semver-stability promise.

## Purpose

Every part of the engine writes through one commit path. Foundation is that path, proven on its own: the crates any Ironflow X product links to commit one environment's events durably, in order, and read them back.

![Foundation's layers: a host calls the engine with a key; the engine checks the key against the control plane, then runs event calls on the ledger, which commits events durably and reads them back; an embedder without keys calls the ledger directly; beneath them are the pure credentials, event rules and core types](https://raw.githubusercontent.com/ironflowx/foundation/main/docs/img/layers.svg)

## What it is

Library crates: an engine that opens on a data directory and admits every call with a key, the HTTP door that serves it, and the proto client that calls it. The `ironflowx` binary, which serves an engine over Connect, gRPC and gRPC-Web, and its CLI are built in [`ironflowx/ironflow`](https://github.com/ironflowx/ironflow) from these crates (D85).

- The first open bootstraps the engine: it mints the admin key, or takes the key you supply.
- Create keys scoped `read`, `write` or `admin`, read-only unless asked otherwise, with an optional expiry; list them, and revoke one.
- Emit an event, or a batch of up to 100, with data, metadata, a version and an idempotency key; each records the key that emitted it.
- Repeat an emit with its idempotency key and get the original event's ID back, with nothing new stored.
- Read an event by ID, or list events newest first, as soon as any emit returns.
- Reopen after a crash with nothing acknowledged lost, and recover an admin key from the data directory when the last one is lost.

An app that needs no keys opens a ledger directly.

## Architecture

The engine is a library: a host opens it on a data directory, and every call to it carries a key. An embedder that needs no keys opens a ledger in a directory instead. The `ironflowx` binary in [`ironflowx/ironflow`](https://github.com/ironflowx/ironflow) is the first host. Event rules, credentials and core are pure; only the ledger, the control plane, the engine and a host touch disk, the clock and randomness.

A write is acknowledged once its record is fsynced, and a read sees every write acknowledged before it arrived, whoever made it:

![Emit, then read your write](https://raw.githubusercontent.com/ironflowx/foundation/main/docs/img/flow-emit-read.svg)

More diagrams (crates, modules and seams, crash recovery) and the details are in [`docs/architecture.md`](https://github.com/ironflowx/foundation/blob/main/docs/architecture.md); the decisions behind them are in [`docs/decisions.md`](https://github.com/ironflowx/foundation/blob/main/docs/decisions.md).

| Crate | Contents |
| --- | --- |
| ironflowx-core | IDs, the environment reference, attribution, scopes, addresses, secrets, stored and event-clock time, the clock trait, error kinds |
| ironflowx-events | event rules: what an event is and when an emit is refused |
| ironflowx-proto | the Operations interface and its request and answer types; behind features, the proto, its wire messages and the proto client |
| ironflowx-cell | the ledger: one environment's durable commit log and read store |
| ironflowx-authz | credentials: the key format, and whether a key is valid and may make a call |
| ironflowx-store | the control plane: which tenants, projects, environments and keys exist |
| ironflowx-operations | the engine: admits every call, then runs it on the ledger or the keys |
| ironflowx | the crates an app uses, re-exported: `core`, `proto`, `cell` and `operations` |
| ironflowx-doors | the HTTP door a host serves an engine through; published from 0.4 |
| ironflowx-codegen | the generator: a CLI's commands, from the proto, at build time; published from 0.4 |

## Use it

### Run the binary

The `ironflowx` binary, its CLI and their release archives are in [`ironflowx/ironflow`](https://github.com/ironflowx/ironflow).

### Embed the engine

Depend on the facade, with `tokio` for the runtime the engine and the ledger run on:

```toml
[dependencies]
ironflowx = "0.3"
tokio = { version = "1", default-features = false, features = ["rt"] }
```

From 0.4 (unreleased; 0.3 re-exports every crate), the facade re-exports `core`, `proto`, `cell` and `operations`. The engine's own crates, `ironflowx-store`, `ironflowx-authz` and `ironflowx-events`, are published for it, not for apps.

[`examples/embed`](https://github.com/ironflowx/foundation/tree/main/examples/embed) bootstraps an engine, emits an event, reads it back with a read-only key and sees that key's emit refused. The engine refuses 0.1 ledger directories (D39, [changelog](https://github.com/ironflowx/foundation/blob/main/CHANGELOG.md)).

### Call a served engine

Add the `client` feature to your `ironflowx` dependency to use `ironflowx::proto::Client` over HTTP. Use `client-tls` for HTTPS, with roots from the system's trust store. Both features are off by default, so an embedder can build without the HTTP stack. `Engine` and `Client` both implement `Operations`.

Start the `ironflowx` binary from [`ironflowx/ironflow`](https://github.com/ironflowx/ironflow) on loopback with a fresh data directory.

```sh
ironflowx serve --listen 127.0.0.1:8080 --data-dir ./whoami-demo
```

Once it is ready, run the example from this checkout in another terminal.

```sh
cargo run --locked -p ironflowx --features client --example whoami -- http://127.0.0.1:8080 ./whoami-demo/admin.key
```

The [example](https://github.com/ironflowx/foundation/blob/main/crates/ironflowx/examples/whoami.rs) asks `whoami` and prints the calling key's ID and scopes.
