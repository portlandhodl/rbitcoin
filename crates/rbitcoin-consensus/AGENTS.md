# rbitcoin-consensus

Header, block, and script rules from bytes and caller facts. Depends on
primitives. It does not open a store or write Class A. Peer and RPC script
checks use the detached worker. The thread rule is
[`docs/concurrency.md`](../../docs/concurrency.md).

## Read first

Open the row that matches the change. Leave the other owners closed.

| Change | Read |
|--------|------|
| Rules we own vs Core corpora | [`docs/consensus-tests.md`](../../docs/consensus-tests.md) |
| rust-bitcoin gaps | [`docs/rust-bitcoin-limitations.md`](../../docs/rust-bitcoin-limitations.md) |
| Do not split an opcode match for line count | [`docs/code-shape.md`](../../docs/code-shape.md) |

## Where

- Scripts: `src/script/` (`interpreter.rs` is the opcode match)
- Headers and blocks: `src/header.rs`, `src/block/` (`facts.rs` is the spend and assemble decisions)
- Worker pool: `src/script_pool.rs`
- Confirm pipeline (lookup, load, pin, write, index): `rbitcoin-query` `src/confirm_run/` and `src/block_pipeline.rs`

## Verify

`cargo test -p rbitcoin-consensus --test suite`
