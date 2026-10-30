# Track C: Join

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md).

## Approach

A multi-threaded C++ engine built for throughput, small memory and zero external dependencies:

1. **Zero-copy memory-mapped I/O**: `records.csv` is mapped with `mmap` + `MADV_SEQUENTIAL`; parsing reads the mapping directly.
2. **Exact chunk partitioning**: the spec guarantees `\r` only occurs in CRLF terminators, so 12 worker threads find exact record boundaries by scanning for `\r\n` from their nominal offsets. No coordination, no overlap.
3. **SIMD field scanning**: quoted fields are scanned for their closing quote with an inline SSE2 loop (16 bytes per step) and a `memchr` tail, instead of byte-at-a-time loops.
4. **Flat O(1) region lookup**: `regions.csv` is parsed into a 1 MB `uint8_t` table indexed by region id. Country codes are stored packed as 3 bytes in one sorted integer array, so output order needs no re-sort and no `std::string` table. Missing regions map to `_UNMATCHED`.
5. **Lock-free name interning**: distinct names (~100k) become compact 32-bit ids through an open-addressing table with atomic CAS slot reservation plus a 2-way thread-local L1 cache in front of it.
6. **Correct length and identity for every name**: names up to 25 bytes hash from four 64-bit words with end-bounded loads (never a read past the mapping). Longer names take a full-length FNV-1a path, so distinct long names can never be merged by truncation, and the unescape buffer can never overflow.
7. **Dense country bitsets**: each country tracks distinct name ids in a 1,600-word bitset (12.5 KB per country, ~3 MB total). Setting a bit is a relaxed check-and-OR; distinct counts are a final `popcount` pass.
8. **Thread-local aggregates**: all numeric totals accumulate in per-thread arrays and merge in microseconds at the end. Output is built in one buffer and written with a single `write` loop.

## Dependencies

None. Standard C++20, POSIX threads and glibc only (`dependency_count = 0`).

## With another 24 hours

- Wider SIMD (AVX2) for the quote scan and digit parsing.
- Dynamic work stealing if row-length variance ever unbalances the 12 chunks.
- io_uring for cold page-cache runs.
