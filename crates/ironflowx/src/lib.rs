//! Ironflow foundation crates in one dependency: what an app uses. The engine's own crates
//! (`ironflowx-store`, `ironflowx-authz`, `ironflowx-events`) stay published, outside it.
//!
//! Opt in to `client` for the proto client over HTTP, or `client-tls` for HTTPS as well.
//! Both features are off by default. The proto client and the engine implement `proto::Operations`.

pub use ironflowx_cell as cell;
pub use ironflowx_core as core;
pub use ironflowx_operations as operations;
pub use ironflowx_proto as proto;
