//! The size limits of what a caller sends, and of a message on the wire, each in one place (D120).
//! Event rules, credentials, attribution, the door and the proto client check them; a door and the
//! CLI can show them without linking a rule crate, and an embedder or executor sizes a payload
//! before it sends it.

/// An event's name.
pub const MAX_EVENT_NAME_BYTES: usize = 256;
/// An event's data, as sent.
pub const MAX_DATA_BYTES: usize = 524_288;
/// An event's metadata, as sent.
pub const MAX_METADATA_BYTES: usize = 65_536;
/// An event's idempotency key.
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 256;
/// The events of one batch. A count, not bytes: it is here because a caller sizes a batch by it.
pub const MAX_BATCH_EVENTS: usize = 100;
/// The data and metadata of a batch's accepted events together.
pub const MAX_BATCH_BYTES: usize = 4 * 1024 * 1024;
/// A key's label.
pub const MAX_KEY_LABEL_BYTES: usize = 128;
/// An embedder's name, as in `embedder:<name>`.
pub const MAX_EMBEDDER_NAME_BYTES: usize = 128;
/// A request body or a message on the wire, either way, in the door and the proto client.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
