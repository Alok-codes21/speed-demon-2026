# Track C: Join

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md).

## Approach

A high-performance C++20 engine built for maximum throughput, low memory footprint, and zero external dependencies:

1. **Zero-copy memory-mapped I/O**: Both `regions.csv` and `records.csv` are mapped with `mmap` and `MADV_SEQUENTIAL`.
2. **Parallel Table Pre-Warming**: `MADV_POPULATE_WRITE` is dispatched across 12 worker threads up front to fault in anonymous lookup tables concurrently, eliminating kernel page-table lock contention.
3. **Exact CRLF chunk partitioning**: 8 pieces per thread are claimed dynamically through atomic work sharing, avoiding tail imbalance.
4. **SWAR 8-digit Integer & Currency Decoders**: IDs, region codes, and currency cents are parsed 8 bytes at a time using bitwise masking and arithmetic folding (`load8` + `fold`), completely bypassing string scanning and floating point operations.
5. **Lock-Free 4-Lane AVX2 Name Table**: Names intern to small IDs in a 1 MB table where 4 slots per cache line are probed simultaneously with 256-bit SIMD comparison (`_mm256_cmpeq_epi64`).
6. **In-Scan Distinct Counting**: Country bitsets are updated on the fly using `fetch_or`. A successful bit toggle directly increments the country's distinct counter, completely eliminating any post-scan popcount over bitsets.
7. **16-Record Lead Prefetch Pipeline**: A sliding ring buffer prefetches region entries, name table slots, and country bitset lines 16 records ahead, completely overlapping memory latency.
8. **Direct Output Buffer Formatting**: Output strings are generated without allocations and written in a single streaming write.

## Dependencies

None. Standard C++20, POSIX threads and glibc only (`dependency_count = 0`).
Built offline with pinned GCC 16 and Debian 13 runtime.
