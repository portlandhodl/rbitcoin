use rbitcoin_primitives::{read_compact_size, read_uleb128, write_compact_size};
use rbitcoin_store::compact::*;
use rbitcoin_store::error::StoreError;

#[test]
fn amount_exp_mantissa_strips_trailing_tens() {
    assert_eq!(amount_exp_mantissa(0), (0, 0));
    assert_eq!(amount_exp_mantissa(546), (0, 546));
    assert_eq!(amount_exp_mantissa(1_000), (3, 1));
    assert_eq!(amount_exp_mantissa(100_000_000), (8, 1));
    assert_eq!(amount_exp_mantissa(5_000_000_000), (9, 5));
    assert_eq!(amount_exp_mantissa(10_000_000_000), (9, 10));
    assert_eq!(amount_exp_mantissa(330), (1, 33));
    assert_eq!(amount_exp_mantissa(1_250_000_000), (7, 125));
    assert_eq!(amount_exp_mantissa(2_500_000_000), (8, 25));
    for sats in [
        0u64,
        330,
        546,
        1_000,
        1_250_000_000,
        2_500_000_000,
        5_000_000_000,
    ] {
        let (e, m) = amount_exp_mantissa(sats);
        assert_eq!(scale_amount_exp(e, m).unwrap(), sats, "{sats}");
    }
    assert_eq!(scale_amount_exp(8, 1).unwrap(), 100_000_000);
    for e in 0..=AMOUNT_EXP_MAX {
        assert_eq!(
            scale_amount_exp(e, 1).unwrap(),
            10u64.pow(u32::from(e)),
            "{e}"
        );
    }
    assert!(scale_amount_exp(9, u64::MAX).is_err());
    assert!(scale_amount_exp(10, 1).is_err());
    assert!(scale_amount_exp(1, 10).is_err());
    assert!(scale_amount_exp(3, 0).is_err());
    assert_eq!(scale_amount_exp(0, 0).unwrap(), 0);
}
