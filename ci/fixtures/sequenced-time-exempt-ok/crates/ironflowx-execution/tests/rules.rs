// Fixture: an exempt file with its one helper's use, which passes.
fn now() -> SequencedTime {
    SequencedTime::from_event_clock(NOW)
}
