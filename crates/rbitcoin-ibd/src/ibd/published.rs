//! Header thread. It persists batches, loads the walk origin, and reseeds.
//! The coordination loop only queues those commands.

use super::events::run_header_batch;
use super::header_walk::{install_origin, load_origin, origin_input, send_getheaders};
use super::path::{load_resume_entries, stage_resumed_path};
use super::state::{HeaderCmd, IbdWorkState};
use bitcoin::hashes::Hash;
use bitcoin::BlockHash;
use rbitcoin_net::chain::ChainHub;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tokio::sync::mpsc::Receiver;

pub fn spawn_header_thread(
    shared: Arc<Mutex<IbdWorkState>>,
    hub: Arc<ChainHub>,
    mut rx: Receiver<HeaderCmd>,
    header_fks: super::peer_io::HeaderFkBus,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        while let Some(cmd) = rx.blocking_recv() {
            match cmd {
                HeaderCmd::Batch { peer, headers } => {
                    run_header_batch(&shared, hub.as_ref(), peer, headers, &header_fks);
                }
                HeaderCmd::Ask => {
                    let input = {
                        let st = shared.lock().unwrap_or_else(|err| err.into_inner());
                        origin_input(&st, hub.as_ref())
                    };
                    let loaded = if input.already {
                        None
                    } else {
                        Some(load_origin(hub.as_ref(), &input))
                    };
                    {
                        let mut st = shared.lock().unwrap_or_else(|err| err.into_inner());
                        if let Some(loaded) = loaded {
                            install_origin(&mut st, hub.as_ref(), loaded);
                        }
                        let _ = send_getheaders(&mut st, hub.as_ref());
                    }
                }
                HeaderCmd::Reseed => {
                    let exclude: Vec<[u8; 32]> = {
                        let st = shared.lock().unwrap_or_else(|err| err.into_inner());
                        st.reorg.invalid.iter().collect()
                    };
                    let path = load_resume_entries(hub.as_ref(), &exclude);
                    let pairs: Vec<(BlockHash, rbitcoin_primitives::Fk)> = path
                        .iter()
                        .map(|e| (BlockHash::from_byte_array(e.hash), e.header_fk))
                        .collect();
                    {
                        let mut st = shared.lock().unwrap_or_else(|err| err.into_inner());
                        stage_resumed_path(&mut st, hub.as_ref(), &path);
                    }
                    super::peer_io::publish_header_fks(&header_fks, &pairs);
                    if let Err(e) = super::reorg::rewind_off_coord_lock(
                        &shared,
                        hub.as_ref(),
                        Some(&header_fks),
                    ) {
                        rbitcoin_log::warn!("ibd: header-thread rewind failed: {e}");
                    }
                }
            }
        }
    })
}
