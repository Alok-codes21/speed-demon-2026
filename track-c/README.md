# Track C: Join

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md).

## Approach

Our solution is a high-performance, multi-threaded C++ engine designed for maximum throughput, minimal memory usage, and zero external dependencies:

1. **Zero-Copy Memory-Mapped I/O**: Maps `records.csv` into memory using `mmap` with `MADV_SEQUENTIAL`, avoiding user/kernel copy overhead.
2. **Lock-Free Chunk Partitioning**: Leverages the specification's structural guarantee that `\r` (CR) never appears inside quoted fields and only occurs in CRLF terminators. 12 worker threads independently scan forward from their assigned file offsets to locate `\r\n`, enabling 100% exact, non-overlapping partition boundaries without cross-thread coordination.
3. **Flat O(1) Cache-Resident Region Lookup**: Instead of dynamic hash maps, we parse `regions.csv` into a 1 MB flat array (`uint8_t region_to_country[1000000]`). Lookups take a single O(1) index operation with zero branch mispredictions, residing entirely in L2/L3 cache. Missing regions naturally default to `_UNMATCHED`.
4. **Lock-Free Concurrent Name Interning**: Distinct name byte strings (~100k distinct values, max 25 bytes each) are interned to compact 32-bit IDs via a lock-free open-addressing hash table with atomic CAS slot reservation.
5. **Dense Country Bitsets**: Each country tracks distinct name IDs using a compact 2,500-word bitset (20 KB per country, ~3 MB total memory). Setting a bit uses relaxed check-and-OR, which avoids atomic bus contention on already-seen names (>97% of accesses). At the end of the run, distinct name counts are computed via hardware `popcount` (`__builtin_popcountll`).
6. **Streaming Length Calculation**: Note byte lengths are computed in-flight by subtracting escape quote count from delimited span length without buffering or heap allocation.
7. **Thread-Local Metric Aggregators**: Numeric totals (`count`, `id_sum`, `amount_sum`, min, max, byte sums) are kept in private per-thread arrays during parsing and merged at termination in microseconds.

## Dependencies

None. Standard C++20 and POSIX threads / standard glibc only (`dependency_count = 0`).

## With another 24 hours

- Hand-tuned AVX2 SIMD vectorization for scanning field commas and unescaped quote pairs.
- io_uring asynchronous parallel reading for cold file systems where files are not yet in page cache.
- Dynamic work-stealing job queue if row length variance causes chunk imbalance across threads.
