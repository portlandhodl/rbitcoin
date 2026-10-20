use rbitcoin_primitives::median_time::*;

#[test]
fn unsorted_three() {
    assert_eq!(median_time_past_times(&[3, 1, 2]).unwrap(), 2);
}

#[test]
fn empty_median_time_is_an_error() {
    let err = median_time_past_times(&[]).unwrap_err();
    assert!(err.contains("median"), "{err}");
}
