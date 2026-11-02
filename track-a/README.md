# Track A: Ingest

The retained implementation is C++20. `Dockerfile` builds `main.cpp` with pinned GCC 15 and runs the binary in a small Debian 13 image. It uses only the C++ standard library and Linux system calls.

## Approach

The input is memory-mapped with a zero guard page and split at line boundaries. Up to 12 workers parse records into packed per-hour blocks. A shared endpoint table checks the full endpoint name, rather than treating a hash as an identity. Large latency values use an escape record.

Each hour is sorted by endpoint rank and latency with three radix passes. The sorted records give exact nearest-rank p50, p95 and p99 values. Error counts and integer parts-per-million rates are written in hour and endpoint order.

Parsing, completed-hour processing and output overlap. Hours may close after a 90-minute input horizon; if late or reordered data invalidates that guess, a barrier resets the partial state and the entire input is processed again without early closing. Output offsets are computed from exact byte counts, and the output is truncated to its final length.

## Validation and measurement

The retained engine passed byte-for-byte comparisons against the reference on tiny, sample, full, shuffled and reversed inputs at 1, 2, 4, 7 and 12 workers. Further integer-oracle comparisons covered unseen seeds and input orders. These tests support exactness; they are not a proof over every possible input.

The clean A1 restore passed all five official board gates. One source-verified receipt, run `run-39e2080d9c66`, measured 19.856 ms, 15,142,912 bytes, S411 and 85.482018 points. Public-board timings vary; final scoring uses the frozen commit and a hidden seed.

An exact-name Rust adaptation was also tested and rejected. Its captured official receipt was 31.018 ms, 16,277,504 bytes and 78.310063 points. That receipt counted an externally re-added, inactive C++ source as well as Rust (S763), so it is not a clean source-size comparison. The slower runtime was enough to prefer the C++ restore. The Rust source and build switch were removed.

Required license notices are retained in the repository-level `THIRD_PARTY_NOTICES.md`.
