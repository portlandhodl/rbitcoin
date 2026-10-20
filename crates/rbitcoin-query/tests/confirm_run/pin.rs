use rbitcoin_primitives::Fk;
use rbitcoin_query::confirm_run::pin::*;
use rbitcoin_query::confirm_run::*;
use rbitcoin_query::{CreatePin, ParentIdent, U64Map};
use rbitcoin_store::{OutputRecord, TxRecord};

fn pin_with(txid: [u8; 32], inputs: u32) -> CreatePin {
    rbitcoin_query::CreatePinInner::records(
        TxRecord {
            txid,
            version: 1,
            locktime: 0,
            input_start_fk: Fk::NULL,
            input_count: inputs,
            output_start_fk: Fk::NULL,
            output_count: 1,
        },
        vec![OutputRecord::unspent(50, vec![0x51])],
    )
}

#[test]
fn plan_pin_coinbase_flag_follows_input_count() {
    let one = pin_with([0x31; 32], 1);
    let two = pin_with([0x32; 32], 2);
    let mut parents = rbitcoin_query::BatchParents::new();
    parents.insert_owned(
        Fk(1),
        one.tx().clone(),
        vec![(0, OutputRecord::unspent(50, vec![0x51]))],
        vec![0],
        None,
        None,
        Vec::new(),
    );
    let mut by_id: U64Map<CreatePin> = U64Map::default();
    by_id.insert(1, one);
    let mut vouts: U64Map<Vec<u32>> = U64Map::default();
    vouts.insert(1, vec![0]);
    let mut still = U64Map::default();
    apply_plan_pins(
        &vouts,
        &by_id,
        &ParentPinStamp::default(),
        &mut parents,
        &mut still,
    );
    assert_eq!(parents.get_parent_coinbase(Fk(1)), None);

    let mut stamp = ParentPinStamp::default();
    stamp
        .idents
        .insert(1, ParentIdent::with_body([0x31; 32], (4, 8)));
    apply_plan_pins(&vouts, &by_id, &stamp, &mut parents, &mut still);
    assert_eq!(
        parents.get_body_range(Fk(1)),
        Some((4, 8)),
        "a one-input pin still publishes a body range"
    );

    let mut fresh = rbitcoin_query::BatchParents::new();
    let mut by_two: U64Map<CreatePin> = U64Map::default();
    by_two.insert(2, two);
    let mut need: U64Map<Vec<u32>> = U64Map::default();
    need.insert(2, vec![0]);
    let mut still = U64Map::default();
    apply_plan_pins(
        &need,
        &by_two,
        &ParentPinStamp::default(),
        &mut fresh,
        &mut still,
    );
    assert_eq!(fresh.get_parent_coinbase(Fk(2)), Some(false));

    let mut empty_need: U64Map<Vec<u32>> = U64Map::default();
    empty_need.insert(3, Vec::new());
    let mut by_empty: U64Map<CreatePin> = U64Map::default();
    by_empty.insert(3, pin_with([0x33; 32], 1));
    let mut dest = rbitcoin_query::BatchParents::new();
    let mut still = U64Map::default();
    apply_plan_pins(
        &empty_need,
        &by_empty,
        &ParentPinStamp::default(),
        &mut dest,
        &mut still,
    );
    assert!(
        dest.get_parent_tx(Fk(3)).is_some(),
        "an empty need list is not already covered"
    );
}
