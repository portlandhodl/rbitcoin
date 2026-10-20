fn pin_empty_store_has_no_view(q: &Query) {
    assert!(q.pin_chain_view().unwrap().is_none());
    assert!(q.pin_view(ChainViewKind::Tip, None).unwrap().is_none());
    assert!(q
        .pin_view(ChainViewKind::ScriptHash, None)
        .unwrap()
        .is_none());
    let err = q.run_at_chain_view(|_v| Ok(())).unwrap_err();
    assert!(matches!(err, StoreError::NotFound));
}

/// Height 1 spends the height-0 create. A pin at either height answers the
/// scripthash and outpoint surfaces as of that height.
fn pin_asof_spend(q: &Query, view0: &ChainView, create_txid: [u8; 32]) -> ChainView {
    let sh = script_hash(&[0x51]);
    let view1 = q.pin_chain_view().unwrap().expect("height 1");
    assert_eq!(view1.height, Height(1));
    assert_eq!(q.pin_view(ChainViewKind::Tip, None).unwrap(), Some(view1));
    assert!(
        view0.still_live(q).unwrap(),
        "a prefix pin stays live across tip extension"
    );
    assert!(!q.is_outpoint_spent_at(&create_txid, 0, Some(0)).unwrap());
    assert!(q.is_outpoint_spent_at(&create_txid, 0, Some(1)).unwrap());
    assert!(q.is_outpoint_spent(&create_txid, 0).unwrap());

    let utxo0 = q.scripthash_listunspent_in(&sh, view0).unwrap();
    assert_eq!(utxo0.len(), 1);
    assert_eq!(utxo0[0].tx_hash, create_txid);
    assert_eq!(utxo0[0].value, 10_0000_0000);
    assert_eq!(
        q.scripthash_balance_in(&sh, view0).unwrap().confirmed,
        10_0000_0000
    );
    let hist0 = q.scripthash_history_in(&sh, view0).unwrap();
    assert_eq!(hist0.iter().map(|i| i.txid).collect::<Vec<_>>(), [create_txid]);

    assert!(
        q.scripthash_listunspent_in(&sh, &view1).unwrap().is_empty(),
        "the spend at height 1 is visible as of 1"
    );
    assert_eq!(q.scripthash_balance_in(&sh, &view1).unwrap().confirmed, 0);
    assert_eq!(q.scripthash_history_in(&sh, &view1).unwrap().len(), 2);
    view1
}

/// A same-height replace at 1 kills the old tip pin and the join slot built
/// under it, and brings the create back unspent.
fn pin_same_height_replace_kills_tip_pin(
    q: &Query,
    view0: &ChainView,
    view1: &ChainView,
    create_txid: [u8; 32],
) -> HeaderRecord {
    let sh = script_hash(&[0x51]);
    let mut slot = None;
    let hist_a = q.scripthash_history_slot(&sh, &mut slot).unwrap();
    assert_eq!(hist_a.len(), 2);

    q.disconnect_tip().unwrap();
    assert!(
        !view1.still_live(q).unwrap(),
        "disconnect of the pinned height kills the view"
    );
    assert!(view0.still_live(q).unwrap());
    let (mut h1b, mut t1b) = coinbase_block(1, view0.header_fk, Some(view0.hash));
    h1b.nonce = h1b.nonce.wrapping_add(1);
    rehash_header(&mut h1b, &view0.hash);
    t1b.tx.txid[5] = 0xbb;
    let txid_b = t1b.tx.txid;
    q.connect_block(Height(1), &h1b, &[t1b]).unwrap();
    assert!(
        !view1.still_live(q).unwrap(),
        "a same-height replace does not revive the old pin"
    );
    let view1b = q.pin_chain_view().unwrap().expect("replacement tip");
    assert_eq!((view1b.height, view1b.hash), (Height(1), h1b.hash));
    assert!(view1b.still_live(q).unwrap());

    let ids_b: Vec<_> = q
        .scripthash_history_slot(&sh, &mut slot)
        .unwrap()
        .iter()
        .map(|i| i.txid)
        .collect();
    assert_eq!(
        ids_b,
        [create_txid, txid_b],
        "the replace misses the slot: the spend is gone, B's coinbase is in"
    );
    assert!(!q.is_outpoint_spent(&create_txid, 0).unwrap());
    h1b
}

/// The write-behind job taken for a height that is then replaced must not
/// seed the durable index with the old branch's creates.
fn pin_stale_sh_job_after_replace(q: &Query, h1b: &HeaderRecord) {
    let prev = q.tip_header_fk().unwrap().unwrap();
    let (h2a, mut t2a) = coinbase_block(2, prev, Some(h1b.hash));
    t2a.outputs = vec![OutputRecord::unspent(50_0000_0000, vec![0xaa])];
    q.commit_class_a_only(&h2a, &[t2a]).unwrap();
    q.confirm_block(Height(2), &h2a.hash).unwrap();
    q.release_index_writebehind(Height(2));
    let stolen = q.take_sh_job_for_apply().expect("old branch job");

    q.disconnect_tip().unwrap();
    let (mut h2b, mut t2b) = coinbase_block(2, prev, Some(h1b.hash));
    h2b.nonce = h2a.nonce.wrapping_add(1);
    rehash_header(&mut h2b, &h1b.hash);
    t2b.outputs = vec![OutputRecord::unspent(50_0000_0000, vec![0xbb])];
    t2b.tx.txid[30] = 0xbb;
    q.commit_class_a_only(&h2b, &[t2b]).unwrap();
    q.confirm_block(Height(2), &h2b.hash).unwrap();

    q.apply_sh_job(stolen).unwrap();
    q.finish_sh_job(Height(2));
    q.apply_sh_pending().unwrap();
    assert!(
        q.scripthash_history(&script_hash(&[0xaa])).unwrap().is_empty(),
        "stale branch creates must not seed the durable index"
    );
    assert_eq!(
        q.scripthash_history(&script_hash(&[0xbb])).unwrap().len(),
        1
    );
    assert_eq!(q.sh_indexed_through_height(), Some(2));
}

/// `run_at_chain_view` retries once its pin dies under it, and gives up
/// with a named error when every attempt sees the tip move.
fn pin_run_at_chain_view_retries(q: &Query, h1b: &HeaderRecord) {
    let prev1 = q.header_at_height(Height(1)).unwrap().unwrap().0;
    let mut calls = 0u32;
    let (view, n) = q
        .run_at_chain_view(|view| {
            calls += 1;
            if calls == 1 {
                replace_tip_same_height(q, 2, prev1, h1b.hash, 7);
                assert!(!view.still_live(q).unwrap());
            }
            Ok(calls)
        })
        .unwrap();
    assert!(calls >= 2, "must retry after the pin died, calls={calls}");
    assert_eq!(n, calls);
    assert!(view.still_live(q).unwrap());
    assert_eq!(view.hash, q.pin_chain_view().unwrap().unwrap().hash);

    let mut delta = 7u32;
    let err = q
        .run_at_chain_view(|_view| {
            delta += 1;
            replace_tip_same_height(q, 2, prev1, h1b.hash, delta);
            Ok(())
        })
        .unwrap_err();
    assert!(err.to_string().contains("chain view moved"), "{err}");
    assert!(
        !err.to_string().contains("corrupt"),
        "a moved view is not corruption: {err}"
    );
}

/// One pad of pins over a spend, two same-height replaces, and a disconnect
/// to empty: tip and buried pins, as-of scripthash answers, the join slot,
/// a stale write-behind job, and the pinned-run retry.
#[test]
fn chain_view_pin_asof_reorg() {
    let (dir, q) = temp_query("chain-view-journey");
    pin_empty_store_has_no_view(&q);

    let (h0, ta0) = funded_op_true_coinbase(0, Fk::NULL, None);
    let create_txid = ta0.tx.txid;
    let hfk0 = q.connect_block(Height(0), &h0, &[ta0]).unwrap();
    let create_fk = q.block_tx_fks(Height(0)).unwrap()[0];
    assert!(q.pin_chain_view_at(&[0xee; 32]).unwrap().is_none());
    let view0 = q.pin_chain_view_at(&h0.hash).unwrap().expect("genesis");
    assert_eq!((view0.height, view0.hash), (Height(0), h0.hash));
    assert_eq!(q.pin_chain_view().unwrap(), Some(view0));

    let (h1, spend, _) = spend_op_true(hfk0, h0.hash, create_fk, create_txid);
    q.connect_block(Height(1), &h1, &[spend]).unwrap();
    let view1 = pin_asof_spend(&q, &view0, create_txid);
    let h1b = pin_same_height_replace_kills_tip_pin(&q, &view0, &view1, create_txid);
    pin_stale_sh_job_after_replace(&q, &h1b);
    assert!(
        view0.still_live(&q).unwrap(),
        "a buried pin survives extension and a higher replace"
    );
    pin_run_at_chain_view_retries(&q, &h1b);

    q.disconnect_tip().unwrap();
    q.disconnect_tip().unwrap();
    assert_eq!(q.tip_height(), Some(Height(0)));
    assert!(view0.still_live(&q).unwrap());
    assert_eq!(
        q.pin_chain_view_at(&h0.hash).unwrap().unwrap().header_fk,
        view0.header_fk
    );
    q.disconnect_tip().unwrap();
    assert!(!view0.still_live(&q).unwrap());
    assert!(q.pin_chain_view_at(&h0.hash).unwrap().is_none());
    assert!(q.pin_chain_view().unwrap().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}
