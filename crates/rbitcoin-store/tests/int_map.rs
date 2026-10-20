use rbitcoin_primitives::Fk;
use rbitcoin_store::int_map::*;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

#[test]
fn u64_identity_hasher_is_raw_key_and_map_roundtrips_pack_scale() {
    let mut h = U64IdentityHasher::default();
    h.write_u64(0xdead_beef_cafe_u64);
    assert_eq!(h.finish(), 0xdead_beef_cafe_u64);
    // Byte-path mix (Hash trait fallback for non-u64 keys).
    let mut h2 = U64IdentityHasher::default();
    h2.write(&[0x01, 0x02, 0x03, 0x04]);
    assert_ne!(h2.finish(), 0);
    let mut h3 = U64IdentityHasher::default();
    h3.write_u32(0xabcd_u32);
    assert_eq!(h3.finish(), 0xabcd_u64);

    let n = 8_000u64;
    let mut m: U64Map<u32> = U64Map::with_capacity_and_hasher(n as usize, Default::default());
    for i in 1..=n {
        m.insert(i, (i % 1_000_000) as u32);
    }
    assert_eq!(m.len(), n as usize);
    for i in 1..=n {
        assert_eq!(m.get(&i).copied(), Some((i % 1_000_000) as u32));
    }
    assert_eq!(m.get(&0), None);
    assert_eq!(m.get(&(n + 1)), None);

    let mut s: U64Set = U64Set::with_capacity_and_hasher(n as usize, Default::default());
    for i in 1..=n {
        s.insert(i);
    }
    assert_eq!(s.len(), n as usize);
    assert!(s.contains(&1));
    assert!(!s.contains(&(n + 1)));

    let mut fm: FkMap<u8> = FkMap::default();
    fm.insert(Fk(42), 7);
    assert_eq!(fm.get(&Fk(42)).copied(), Some(7));

    let mut fs: FkSet = FkSet::default();
    assert!(fs.insert(Fk(1)));
    assert!(!fs.insert(Fk(1)));
    assert!(fs.contains(&Fk(1)));
}
