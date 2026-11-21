# foundation

An experimental MIT-licensed rewrite of the Ironflow engine. Not the shipping product; Ironflow 1.x is under the Ironflow License.

Foundation crates are 0.x with no semver-stability promise.

Depend on the facade, with `tokio` for the runtime the ledger runs on:

```toml
[dependencies]
ironflowx = "0.1"
tokio = { version = "1", default-features = false, features = ["rt"] }
```

[`examples/embed`](examples/embed/src/main.rs) opens a ledger, emits an event and reads it back.

| Crate | Contents |
| --- | --- |
| ironflowx-core | IDs, the environment reference, stored and event-clock time, the clock trait, error kinds |
| ironflowx-events | event rules: what an event is and when an emit is refused |
| ironflowx-proto | the Operations interface and its request and answer types |
| ironflowx-cell | the ledger: one environment's durable commit log and read store, and `LocalOperations` |
| ironflowx | every foundation crate, re-exported |
