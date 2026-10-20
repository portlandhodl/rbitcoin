# rbitcoin-esplora

Esplora-compatible REST. Depends on query, store, electrum,
net, and consensus. Electrum is a sibling crate (`rbitcoin-electrum`); both
need `--sh-index` for address history.

## Read first

Open the row that matches the change. Leave the other owners closed.

| Change | Read |
|--------|------|
| Shipped HTTP surface | [`COMPAT.md`](../../COMPAT.md) |
| Flags, listen, SH tradeoffs | [`OPERATOR.md`](../../OPERATOR.md) |
| `/internal/*` and unix listen | [`COMPAT.md`](../../COMPAT.md), [`OPERATOR.md`](../../OPERATOR.md) |
| JSON-RPC overlap (broadcast, unix `rpc.sock`) | [`docs/rpc.md`](../../docs/rpc.md) |

## Where

- Router and listen: `src/server.rs`
- Handlers: `src/handlers.rs`
- electrs `/internal/*`: `src/internal.rs`
- Tx JSON: `src/tx_json.rs`

## Verify

`cargo test -p rbitcoin-esplora --test suite`
