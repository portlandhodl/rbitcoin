Changed

- **Library tests compile against the normal rlib.** Each library package has one `tests/suite` target (`[lib] test = false`), so `cargo test` no longer rebuilds the crate under `--cfg test`. `test-hooks` is gone; the same hooks are `cfg(debug_assertions)`. Initial block download is `rbitcoin-ibd`, and RPC, Electrum, Esplora, and SV2 do not depend on it. `rbitcoin-test`'s default scenarios are one `suite` binary; `overlay` stays feature-gated. `NODE_COMPACT_FILTERS` is advertised per peer hub, so one process can run more than one node.
