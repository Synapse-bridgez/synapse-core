# Swap Engine Invariant Fuzzing

This directory contains `cargo-fuzz` targets for verifying domain invariants and arithmetic safety bounds in the Synapse Swap Engine.

## Prerequisites

Install `cargo-fuzz` and ensure a nightly Rust toolchain with compiler-rt / libFuzzer support is installed:

```bash
rustup toolchain install nightly
cargo +nightly install cargo-fuzz
```

## Running Fuzz Targets

To run the swap requests fuzz target:

```bash
cargo +nightly fuzz run fuzz_swap_requests
```

To run with sanitizers or timeout constraints:

```bash
cargo +nightly fuzz run fuzz_swap_requests -- -max_total_time=300 -timeout=10
```

## Reproducing a Crash

When `libFuzzer` detects a panic, assertion failure, or invariant violation, it writes the offending input to `fuzz/artifacts/fuzz_swap_requests/crash-<hash>`.

To reproduce the failure locally:

```bash
cargo +nightly fuzz run fuzz_swap_requests fuzz/artifacts/fuzz_swap_requests/crash-<hash>
```

## Corpus Management

- Active seed inputs are committed in `fuzz/corpus/fuzz_swap_requests/`.
- CI runs nightly on scheduled cron and uploads any newly discovered crash or minimized corpus artifacts.
- To minimize the corpus:
  ```bash
  cargo +nightly fuzz cmin fuzz_swap_requests
  ```
