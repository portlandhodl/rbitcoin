# Crate orientation

Use this page to find the crate that owns a behavior. The list moves from
shared libraries through storage and runtime crates to composition and tools;
`rbitcoin-node` wires the product crates into one process.

| Crate | Owns |
|-------|------|
| `rbitcoin-primitives` | Shared Bitcoin types and newtypes used across the workspace |
| `rbitcoin-log` | Leveled stderr logging; live progress of long stages (`progress`) |
| `rbitcoin-store` | Relational archive and Class A/B/C on-disk tables |
| `rbitcoin-query` | Archive, confirm pipeline, reconstruction, and query APIs over the store. Loads rows, writes Class A / Class C / spend annotations, and calls `rbitcoin-consensus` for rule decisions |
| `rbitcoin-consensus` | Header, block, and script rules from bytes and caller facts. Does not open a store |
| `rbitcoin-mempool` | Live transaction graph and admission policy |
| `rbitcoin-net` | P2P, tip follow, and transaction relay |
| `rbitcoin-ibd` | Multi-peer initial block download and the confirm pipeline. Depends on `rbitcoin-net`; RPC and Electrum do not |
| `rbitcoin-rpc` | Core-class JSON-RPC subset |
| `rbitcoin-electrum` | Electrum TCP server |
| `rbitcoin-esplora` | Esplora REST server |
| `rbitcoin-sv2` | Stratum v2 Template Distribution Protocol server (Noise over TCP) |
| `rbitcoin-node` | Product binary and process composition |
| `rbitcoin-cli` | RPC client binary |
| `rbitcoin-test` | High-level scenario and integration-test harness |
| `rbitcoin-bench` | Optional Electrum/Esplora client benchmark |

For crate neighbors and read-first links, see
`crates/<name>/AGENTS.md` when present. Those files own no facts. Test locations and suite selection are
in [`TESTING.md`](../TESTING.md). Agent task routing is in
[`ORIENT.md`](./ORIENT.md).
