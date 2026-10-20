use rbitcoin_primitives::script_sigops::*;

#[test]
fn accurate_checkmultisig_uses_op_n() {
    assert_eq!(script_sigop_count(&[0xac], false), 1);
    assert_eq!(script_sigop_count(&[0xad], false), 1);
    assert_eq!(script_sigop_count(&[0xae], false), 20);
    assert_eq!(script_sigop_count(&[0xaf], false), 20);
    assert_eq!(script_sigop_count(&[0x52, 0xae], true), 2);
    assert_eq!(script_sigop_count(&[0x53, 0xae], true), 3);
    assert_eq!(script_sigop_count(&[0x51, 0xae], true), 1);
    assert_eq!(script_sigop_count(&[0x60, 0xae], true), 16);
    assert_eq!(script_sigop_count(&[0x51, 0xaf], true), 1);
    assert_eq!(script_sigop_count(&[0x60, 0xaf], true), 16);
    assert_eq!(script_sigop_count(&[0x51, 0xae], false), 20);
    assert_eq!(script_sigop_count(&[0x50, 0xae], true), 20);
    assert_eq!(script_sigop_count(&[0x61, 0xae], true), 20);
    assert_eq!(script_sigop_count(&[0x01, 0xff, 0xac], false), 1);
    assert_eq!(script_sigop_count(&[0x4c, 0x01, 0xab, 0xac], false), 1);
}

#[test]
fn truncated_pushdata_does_not_count_leftover_checksig() {
    assert_eq!(script_sigop_count(&[0x4c], false), 0);
    assert_eq!(script_sigop_count(&[0x4c, 0x05, 0xac], false), 0);
    assert_eq!(script_sigop_count(&[0x4d, 0xac], false), 0);
    assert_eq!(script_sigop_count(&[0x4d, 0xac, 0xad], false), 0);
    assert_eq!(script_sigop_count(&[0x4d, 0x05, 0x00, 0xac], false), 0);
    assert_eq!(
        script_sigop_count(&[0x4d, 0x01, 0x00, 0xcd, 0xac], false),
        1
    );
    assert_eq!(script_sigop_count(&[0x4e, 0xac], false), 0);
    assert_eq!(script_sigop_count(&[0x4e, 0, 0, 0], false), 0);
    assert_eq!(script_sigop_count(&[0x4e, 1, 0, 0, 0, 0xac], false), 0);
    assert_eq!(
        script_sigop_count(&[0x4e, 0x01, 0x00, 0x00, 0x00, 0xcd, 0xac], false),
        1
    );
    assert_eq!(script_sigop_count(&[0x4c, 2, 0xac], true), 0);
    assert_eq!(script_sigop_count(&[0x4c, 1, 0xac], true), 0);
    assert_eq!(script_sigop_count(&[0x4d, 1, 0, 0xac], true), 0);
    assert_eq!(script_sigop_count(&[0x02, 0x00], true), 0);
    assert_eq!(script_sigop_count(&[0x4c, 2, 0xac, 0xac], true), 0);
    assert_eq!(script_sigop_count(&[0x4c, 2, 0xac, 0xac, 0xac], true), 1);
}

#[cfg(miri)]
#[test]
fn miri_accurate_op_n_checkmultisig_matrix() {
    for op_n in 0x51u8..=0x60 {
        let n = u64::from(op_n - 0x50);
        for cms in [0xaeu8, 0xaf] {
            assert_eq!(script_sigop_count(&[op_n, cms], true), n);
            assert_eq!(script_sigop_count(&[op_n, cms], false), 20);
        }
    }
}
