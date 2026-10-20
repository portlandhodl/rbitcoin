use crate::ConfirmError;
use crate::{Query, TxApply};
use bitcoin::block::Header;
use bitcoin::hashes::Hash;
use bitcoin::Transaction;
use rbitcoin_primitives::Fk;
use rbitcoin_store::{HeaderRecord, InputRecord, OutputRecord, TxRecord};

/// Header row from wire fields. `hash` is the caller-computed `header.block_hash()`.
pub fn header_to_record(prev_fk: Fk, header: &Header, hash: [u8; 32]) -> HeaderRecord {
    HeaderRecord {
        prev_fk,
        version: header.version.to_consensus(),
        timestamp: header.time,
        bits: header.bits.to_consensus(),
        nonce: header.nonce,
        merkle_root: header.merkle_root.to_byte_array(),
        hash,
        size: 0,
        weight: 0,
    }
}

pub fn block_to_apply(
    query: &Query,
    header: &Header,
    txs: &[Transaction],
) -> Result<(HeaderRecord, Vec<TxApply>), ConfirmError> {
    let txids: Vec<[u8; 32]> = txs
        .iter()
        .map(|t| t.compute_txid().to_byte_array())
        .collect();
    block_to_apply_with_txids(query, header, txs, &txids)
}

/// Build archive records using **precomputed** txids (from structure validation).
///
/// Avoids a second `compute_txid` pass over every transaction — the hot path for
/// multi-worker IBD prep on large blocks.
pub fn block_to_apply_with_txids(
    query: &Query,
    header: &Header,
    txs: &[Transaction],
    txids: &[[u8; 32]],
) -> Result<(HeaderRecord, Vec<TxApply>), ConfirmError> {
    rbitcoin_consensus::block::reject_txid_count(txs.len(), txids.len())?;
    let prev_fk = if header.prev_blockhash.to_byte_array() == [0u8; 32] {
        Fk::NULL
    } else {
        rbitcoin_consensus::block::require_linked_parent(
            query
                .get_header_by_hash(header.prev_blockhash.as_byte_array())?
                .map(|(fk, _)| fk),
        )?
    };
    block_to_apply_with_txids_prev(prev_fk, header, txs, txids)
}

/// Like [`block_to_apply_with_txids`] but **no store access** — caller supplies
/// `prev_fk` (use [`Fk::NULL`] on the IBD path where the header row already exists).
pub fn block_to_apply_with_txids_prev(
    prev_fk: Fk,
    header: &Header,
    txs: &[Transaction],
    txids: &[[u8; 32]],
) -> Result<(HeaderRecord, Vec<TxApply>), ConfirmError> {
    rbitcoin_consensus::block::reject_txid_count(txs.len(), txids.len())?;
    let hash = header.block_hash().to_byte_array();
    let header_rec = header_to_record(prev_fk, header, hash);
    let mut out = Vec::with_capacity(txs.len());
    for (tx, txid) in txs.iter().zip(txids.iter()) {
        out.push(tx_to_apply(tx, *txid)?);
    }
    Ok((header_rec, out))
}

#[doc(hidden)]
pub fn tx_to_apply(tx: &Transaction, txid: [u8; 32]) -> Result<TxApply, ConfirmError> {
    let inputs: Vec<InputRecord> = tx
        .input
        .iter()
        .map(|inp| {
            let parent = if inp.previous_output.is_null() {
                rbitcoin_store::InputParent::Coinbase
            } else {
                rbitcoin_store::InputParent::Unlinked {
                    vout: inp.previous_output.vout,
                }
            };
            InputRecord {
                prev_txid: inp.previous_output.txid.to_byte_array(),
                parent,
                sequence: inp.sequence.to_consensus_u32(),
                script_sig: inp.script_sig.to_bytes(),
                witness: inp.witness.to_vec(),
            }
        })
        .collect();

    let outputs: Vec<OutputRecord> = tx
        .output
        .iter()
        .map(|o| OutputRecord::unspent(o.value.to_sat(), o.script_pubkey.to_bytes()))
        .collect();

    Ok(TxApply {
        tx: TxRecord {
            txid,
            version: tx.version.0,
            locktime: tx.lock_time.to_consensus_u32(),
            input_start_fk: Fk::NULL,
            input_count: inputs.len() as u32,
            output_start_fk: Fk::NULL,
            output_count: outputs.len() as u32,
        },
        inputs,
        outputs,
    })
}
