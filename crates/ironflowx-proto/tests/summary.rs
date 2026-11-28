//! The `EmitBatch` summary, which a CLI shows, states the batch count of `limits` (D120): a proto
//! cannot read a Rust constant, so this test holds the two together.

use ironflowx_proto::limits::MAX_BATCH_EVENTS;

#[test]
fn the_emit_batch_summary_states_the_batch_count() {
    let proto = include_str!("../proto/ironflowx/v1/event.proto");
    let summary = format!("\"Emit up to {MAX_BATCH_EVENTS} events at once,");
    assert_eq!(proto.matches(&summary).count(), 1, "{summary}");
}
