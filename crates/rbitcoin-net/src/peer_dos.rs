//! Per-peer DoS controls: message/byte rate windows and inbound connection caps.
//!
//! These are **not** Bitcoin Core banlist parity — they bound cheap resource abuse
//! (flooding messages or multi-MB frames) with disconnect when thresholds trip.

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

/// Default max concurrent **inbound** P2P sessions (post-handshake work).
pub const DEFAULT_MAX_INBOUND: usize = 125;
/// One-second rate window (current second plus the previous second).
pub const RATE_WINDOW: Duration = Duration::from_secs(1);
/// Max application messages per peer per window (after decrypt/frame).
///
/// Tip mempool sync and compact-block reconstruction can burst many small inv /
/// getdata / tx messages; 200/s was disconnecting useful peers. 4k/s matches a
/// healthy peer under load without inviting pure message-spam (byte budget
/// still bounds bulk).
pub const DEFAULT_MAX_MSGS_PER_SEC: u32 = 4_000;
/// Max framed payload bytes per peer per window (BIP324 contents size).
/// ~16 MiB/s: enough for concurrent block + tx relay; still caps multi-peer floods.
pub const DEFAULT_MAX_BYTES_PER_SEC: u64 = 16_000_000;
/// Misbehavior score added when a peer exceeds rate limits. Decoy and
/// unknown-type frames disconnect at 100. An application message disconnects
/// on its first overflow; a repeat from that address within an hour is a
/// threshold exit.
pub const RATE_LIMIT_BAN_SCORE: u32 = 50;
/// Disconnect score for oversized protocol messages already rejected as MessageTooLarge.
pub const OVERSIZE_BAN_SCORE: u32 = 100;

/// Process-wide inbound session slots.
pub fn inbound_semaphore(max: usize) -> Arc<Semaphore> {
    Arc::new(Semaphore::new(max.max(1)))
}

/// Per-session message and byte counters.
///
/// Two buckets (the current second and the previous one). A note weighs the
/// previous bucket by how much of it is still inside the one-second window.
/// No per-message allocation. A tumbling reset granted a second full budget
/// at the boundary.
#[derive(Debug, Clone)]
pub struct PeerRateLimiter {
    pub current_start: Instant,
    pub cur_msgs: u32,
    pub cur_bytes: u64,
    pub prev_msgs: u32,
    pub prev_bytes: u64,
    pub max_msgs: u32,
    pub max_bytes: u64,
}

impl PeerRateLimiter {
    pub fn new(max_msgs: u32, max_bytes: u64) -> Self {
        Self {
            current_start: Instant::now(),
            cur_msgs: 0,
            cur_bytes: 0,
            prev_msgs: 0,
            prev_bytes: 0,
            max_msgs: max_msgs.max(1),
            max_bytes: max_bytes.max(1),
        }
    }

    pub fn default_limits() -> Self {
        Self::new(DEFAULT_MAX_MSGS_PER_SEC, DEFAULT_MAX_BYTES_PER_SEC)
    }

    /// Record one framed message of `payload_len` bytes.
    /// Returns `false` if this message would exceed the window budget.
    pub fn note(&mut self, payload_len: usize) -> bool {
        self.note_at(payload_len, Instant::now())
    }

    /// Same as [`Self::note`] at a chosen instant. Tests pin the window edge.
    pub fn note_at(&mut self, payload_len: usize, now: Instant) -> bool {
        self.roll(now);
        let elapsed_ms = now
            .saturating_duration_since(self.current_start)
            .as_millis()
            .min(1_000) as u64;
        let prev_weight = 1_000 - elapsed_ms;
        let eff_msgs = u64::from(self.cur_msgs) + (u64::from(self.prev_msgs) * prev_weight) / 1_000;
        let eff_bytes = self.cur_bytes + (self.prev_bytes * prev_weight) / 1_000;
        let next_msgs = eff_msgs.saturating_add(1);
        let next_bytes = eff_bytes.saturating_add(payload_len as u64);
        if next_msgs > u64::from(self.max_msgs) || next_bytes > self.max_bytes {
            return false;
        }
        self.cur_msgs = self.cur_msgs.saturating_add(1);
        self.cur_bytes = self.cur_bytes.saturating_add(payload_len as u64);
        true
    }

    #[doc(hidden)]
    pub fn roll(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.current_start);
        if elapsed < RATE_WINDOW {
            return;
        }
        if elapsed >= RATE_WINDOW + RATE_WINDOW {
            self.prev_msgs = 0;
            self.prev_bytes = 0;
            self.cur_msgs = 0;
            self.cur_bytes = 0;
            self.current_start = now;
            return;
        }
        self.prev_msgs = self.cur_msgs;
        self.prev_bytes = self.cur_bytes;
        self.cur_msgs = 0;
        self.cur_bytes = 0;
        self.current_start += RATE_WINDOW;
    }
}

/// Count one decoy (or other unsolicited frame) in `rate`.
///
/// A frame that does not fit adds [`RATE_LIMIT_BAN_SCORE`] and stays
/// connected until `disconnect_at`. Callers use the same threshold as an
/// unknown message type.
pub fn decoy_stays(
    rate: &mut PeerRateLimiter,
    ban_score: &mut u32,
    payload_len: usize,
    disconnect_at: u32,
) -> bool {
    if rate.note(payload_len) {
        return true;
    }
    *ban_score = ban_score.saturating_add(RATE_LIMIT_BAN_SCORE);
    *ban_score < disconnect_at
}
