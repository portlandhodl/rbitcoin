Added

- **Synthetic IBD replay bench.** `rbitcoin-bench ibd` generates valid regtest block corpora for legacy, multi-input, wide, witness, and mixed transaction shapes, then replays them through the production confirm pipeline (lookup → load/pin → scripts → write) with per-stage timing. `--corpus` replays raw block files, and `--case real-866342` runs the vendored mainnet block through structure and script verification. Optional, not in the default build or CI.
