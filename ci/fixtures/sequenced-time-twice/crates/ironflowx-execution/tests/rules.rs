// Fixture: two uses on one line of the exempt file.
fn now() -> (SequencedTime, SequencedTime) {
    (SequencedTime::from_event_clock(NOW), SequencedTime::from_event_clock(LATER))
}
