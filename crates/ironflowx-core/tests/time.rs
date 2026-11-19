use ironflowx_core::Timestamp;

#[test]
fn timestamp_round_trips_and_orders_over_the_whole_range() {
    let millis = [
        i64::MIN,
        i64::MIN + 1,
        -1,
        0,
        1,
        1_763_510_400_000,
        i64::MAX - 1,
        i64::MAX,
    ];
    for &m in &millis {
        assert_eq!(Timestamp::from_millis(m).as_millis(), m);
    }
    for pair in millis.windows(2) {
        assert!(
            Timestamp::from_millis(pair[0]) < Timestamp::from_millis(pair[1]),
            "{pair:?}"
        );
    }
}
