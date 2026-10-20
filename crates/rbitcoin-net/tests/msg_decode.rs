use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message::RawNetworkMessage;
use bitcoin::p2p::Magic;
use bitcoin::Network;
use rbitcoin_net::codec::FramedMessage;
use rbitcoin_net::error::NetError;
use rbitcoin_net::msg_decode::*;
use std::sync::Mutex;
use std::sync::{Arc, OnceLock};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

fn verack_frame() -> FramedMessage {
    FramedMessage {
        magic: Magic::from(Network::Regtest),
        command: *b"verack\0\0\0\0\0\0",
        payload: Vec::new(),
    }
}

#[test]
fn decode_offload_light_and_spawn() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let msg = rt.block_on(decode_framed_offload(verack_frame())).unwrap();
    assert!(matches!(msg.payload(), NetworkMessage::Verack));

    let done = Arc::new(Mutex::new(false));
    let d2 = done.clone();
    rt.block_on(async {
        spawn_decode_then_with_err(
            verack_frame(),
            move |_| {
                *d2.lock().unwrap() = true;
            },
            |_| panic!("should not err"),
        );
        // Yield so the spawned task can finish.
        tokio::task::yield_now().await;
        for _ in 0..50 {
            if *done.lock().unwrap() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    });
    assert!(*done.lock().unwrap());
}

#[test]
fn decode_permit_pools_are_wider_than_one_slot() {
    let blocks = block_decode_permits();
    let ctrl = ctrl_decode_permits();
    assert!(blocks >= 64, "block decodes keep a wide pool, got {blocks}");
    assert!(ctrl >= 8, "header decodes keep their own pool, got {ctrl}");
    assert!(blocks > ctrl);
}
