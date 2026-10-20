# rbitcoin-net

P2P, tip follow, and transaction relay. Depends on query, store, consensus, and mempool.
Initial block download lives in `rbitcoin-ibd`, which depends on this crate.
`rbitcoin-node` owns the process.

## Read first

Open the row that matches the change. Leave the other owners closed.

| Change | Read |
|--------|------|
| Roles, body queue, publish order | [`docs/concurrency.md`](../../docs/concurrency.md) |
| Stage IO | [`docs/invariants.md`](../../docs/invariants.md) |
| Process RAM, caps, evict | [`docs/ibd-memory.md`](../../docs/ibd-memory.md) |
| IO machines | [`docs/io-modality.md`](../../docs/io-modality.md) |
| Published mempool snapshots (fees; unix `/internal` lazy tx JSON) | [`docs/mempool-fee-estimation.md`](../../docs/mempool-fee-estimation.md), [`docs/ibd-memory.md`](../../docs/ibd-memory.md), [`COMPAT.md`](../../COMPAT.md) |

## Where

- IBD: `crates/rbitcoin-ibd` (`assign`, `body`, `path`, `perf_log`)
- Tip: `src/chain.rs`, `src/tip_accept.rs`
- Relay: `src/tx_relay.rs`, `src/peers.rs`

## Verify

`cargo test -p rbitcoin-net --test suite`

Core functional (labeled job only): [`.agents/skills/core-functional/SKILL.md`](../../.agents/skills/core-functional/SKILL.md).
