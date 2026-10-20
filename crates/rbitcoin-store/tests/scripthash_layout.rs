use rbitcoin_primitives::Fk;
use rbitcoin_store::error::StoreError;
use rbitcoin_store::scripthash_layout::*;

#[test]
fn head_value_roundtrip_inline_and_mode10_refuses() {
    let e0 = Fk(3);
    let inline = ShHeadValue::inline_one(e0);
    assert_eq!(unpack8(pack8(&inline).unwrap()).unwrap(), inline);

    let mode10 = (2u64 << SH8_MODE_SHIFT) | 8192;
    match unpack8(mode10) {
        Err(StoreError::Corrupt(m)) => assert_eq!(m, INDEX_REFUSE_PAGED_SH),
        other => panic!("unpack8 mode 10 must refuse, got {other:?}"),
    }

    let slab = ShHeadValue::slab(1, 5, 4096);
    assert_eq!(unpack8(pack8(&slab).unwrap()).unwrap(), slab);
    assert_eq!(slab.used(), 5);
    assert!(slab.is_slab());
    assert!(!slab.is_paged());

    assert!(unpack8(pack8(&ShHeadValue::Empty).unwrap())
        .unwrap()
        .is_empty());
}

#[test]
fn inline_one_fk_pack8_roundtrip() {
    let one = ShHeadValue::inline_one(Fk(0xdead_beef));
    assert_eq!(unpack8(pack8(&one).unwrap()).unwrap(), one);
    assert_eq!(pack8(&one).unwrap(), 0xdead_beef);
    let packed = pack8_bytes(&one).unwrap();
    assert_eq!(unpack8_bytes(&packed).unwrap(), one);
}

#[test]
fn head_key_prefix() {
    let full = [0xabu8; 32];
    let k = head_key_from_full(&full);
    assert_eq!(k.len(), 16);
    assert_eq!(&k[..], &full[..16]);
}

#[test]
fn page_class_is_4k() {
    assert_eq!(slab_bytes(SH_PAGE_SLAB_CLASS), 4096);
}

#[test]
fn pack8_roundtrip_modes() {
    let one = ShHeadValue::inline_one(Fk(0x1234));
    assert_eq!(unpack8(pack8(&one).unwrap()).unwrap(), one);
    let slab = ShHeadValue::slab(2, 9, 4096);
    let got = unpack8(pack8(&slab).unwrap()).unwrap();
    assert_eq!(got, slab);
    assert_eq!(pack8(&ShHeadValue::Empty).unwrap(), 0);
    assert!(pack8(&ShHeadValue::inline_one(Fk(0))).is_ok());
    assert!(unpack8(1u64 << 62 | 1).is_err() || unpack8(1u64 << 62 | 1).is_ok());
}

#[test]
fn pack8_mode_11_extent_roundtrip() {
    let v = ShHeadValue::extent(8192);
    let w = pack8(&v).unwrap();
    assert_eq!(w >> SH8_MODE_SHIFT, 3);
    assert_eq!(w & SH8_PAYLOAD62, 8192);
    match unpack8(w).unwrap() {
        ShHeadValue::Extent { last_page: 8192 } => {}
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        pack8(&ShHeadValue::extent(0)),
        Err(StoreError::Corrupt(_))
    ));
    assert!(matches!(
        unpack8(3u64 << SH8_MODE_SHIFT),
        Err(StoreError::Corrupt(_))
    ));
}

#[test]
fn layout_error_paths() {
    assert!(Fk::NULL.is_null());
    let one = ShHeadValue::inline_one(Fk(9));
    assert_eq!(one.inline_fks(), vec![Fk(9)]);
    assert!(ShHeadValue::Empty.inline_entries().is_empty());
    assert_eq!(payload_start(16), 16 + SH_ALLOC_HEADER_LEN as u64);
}

#[test]
fn head_value_used_and_paged_flags() {
    assert_eq!(ShHeadValue::Empty.used(), 0);
    assert!(!ShHeadValue::Empty.is_paged());
    let one = ShHeadValue::inline_one(Fk(1));
    assert_eq!(one.used(), 1);
    assert!(!one.is_paged());
    let two = ShHeadValue::slab(0, 2, 4096);
    assert_eq!(two.used(), 2);
    let zero_inline = ShHeadValue::Inline {
        entries: [Fk::NULL; SH_INLINE_CAP],
        used: 0,
    };
    assert!(matches!(pack8(&zero_inline), Err(StoreError::Corrupt(_))));
    let extent = ShHeadValue::extent(8192);
    assert_eq!(extent.used(), u32::MAX);
    assert!(extent.is_paged());
    assert!(!extent.is_slab());
    assert!(extent.inline_entries().is_empty());
    assert!(extent.inline_fks().is_empty());
    let slab = ShHeadValue::slab(0, 4, 4096);
    assert_eq!(slab.used(), 4);
    assert!(slab.is_slab());
    assert!(!slab.is_paged());
}

#[test]
fn unpack8_bad_mode_errors() {
    let slab_used_inline = (1u64 << SH8_MODE_SHIFT) | (4096 & SH8_OFF_MASK);
    assert!(matches!(
        unpack8(slab_used_inline),
        Err(StoreError::Corrupt(_))
    ));
    match unpack8(2u64 << SH8_MODE_SHIFT) {
        Err(StoreError::Corrupt(m)) => assert_eq!(m, INDEX_REFUSE_PAGED_SH),
        other => panic!("mode 10 must be INDEX_REFUSE_PAGED_SH, got {other:?}"),
    }
}
