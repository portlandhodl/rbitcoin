use bitcoin::hashes::Hash;
use bitcoin::{OutPoint, Transaction, TxOut, Witness};
use rbitcoin_consensus::silent_payments::{tweak_from_tx, TaprootOut, TxTweak};
use rbitcoin_consensus::ChainParams;
use rbitcoin_primitives::{Fk, Height};
use rbitcoin_query::{ConfirmError, Query};
use rbitcoin_store::{IndexWindow, InputRecord, LoadedTweakTx, OutputRecord, StoreError};
use std::collections::{BTreeMap, HashMap};
mod classifier_record_tests;
