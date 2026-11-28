// Fixture: execution rules' test file with its one helper's call, and a second use beside it.
fn now() -> SequencedTime {
    SequencedTime::from_event_clock(NOW)
}

fn later() -> SequencedTime {
    SequencedTime::from_event_clock(LATER)
}
