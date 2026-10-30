# Track C: performance log

Every step from the reference to your final version, each with the live-board run ID that measured it. Judges check that the run IDs exist and that each change moved the way you say it did.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 14.44 | 367 | 0 |
| 1 | C++ multi-threaded (12 threads): mmap I/O, 1 MB direct region table, in-place unescape, lock-free global name dictionary and 20 KB country bitsets | run-765be91f635c | 0.150 | 18.0 | 57.76 |
| 2 | Streamlined source lines, fast 64-bit word hashing, deterministic CRLF chunk splitting | run-323b229fe206 | 0.149 | 21.0 | 57.79 |
| 3 | Zero-allocation mmap region parser (155ms -> 2ms), thread-local L1 cache, inline quote scan removing 6.6M memchr calls, 4-word register packing, embedded BOM support | run-9067f1f6ffd6 | 0.077 | 24.9 | 63.29 |
| 4 | 2-way associative L1 name cache eliminating conflict misses, single-instruction register word packing | run-e74d2569a17c | 0.075 | 24.3 | 63.65 |
| 5 | Compact 16-byte slot hash table (cutting RAM from 24.3 MB to 13.2 MB), 1600-word bitsets, branchless 8-byte masked loads, buffered output | run-e6c5584df3b0 | 0.071 | 13.2 | 66.93 |
| 6 | Linux-only source (dropped all Windows paths), one shared SIMD quote scanner (SSE2 + memchr tail) for name and note fields, deduplicated escaped-quote handling, packed country codes (no std::string table). Source: 552 -> ~433 scc code lines. Correctness guard: names over 25 bytes now hashed full-length (FNV-1a) instead of truncated, and all word loads are end-bounded | run-e35e545b96d9 | 0.062 | 12.6 | 69.17 |
| 7 | Statement-count reduction, same algorithm and same outputs: direct thread entry (no thunk), memmem chunk splitting, separator-folding number writers, static worker array, merge without empty-thread guards. Source: 433 -> 353 scc code lines (346 C++ + 7 Dockerfile). Time and memory unchanged within noise | run-20eef697c04d | 0.055 | 12.4 | 71.29 |
| 8 | Slot-index interning: the hash-table slot number is the name id, halving the intern table to 8-byte slots and dropping the id counter and pending-slot dance (bitset widened to cover the slot space); fprintf output replaces custom number writers; per-thread name cache halved; statement golf. Source: 353 -> 294 scc code lines (287 C++ + 7 Dockerfile). Memory: 12.4 -> 10.1 MB | run-5bee3add15d8 | 0.051 | 10.1 | 73.65 |
| 9 | AVX2 (32-byte) quote scanning, 16 worker threads. Regression on the 12-core runner (oversubscription); reverted to 12 threads | run-e47aa98dc22a | 0.067 | 10.4 | 70.51 |
| 10 | Dropped the per-thread intern cache (2% measured hit rate: names have no temporal locality), keeping AVX2 scanning and 12 threads. Source: 294 -> 284 scc code lines | run-ee72935ed30f | 0.0497 | 9.9 | 73.91 |
| 11 | Experiment: 6 worker threads (runner: 6 P-cores x 2 SMT, cpuset 0-11). Regressed - hyperthread pairs help this memory-latency-bound parse; reverted to 12 | run-f1f3a1f19375 | 0.065 | 9.0 | 71.62 |
| 12 | Prefetch the intern-table slot as soon as the name hash exists (hides L2 latency behind the note scan), force-inline the quoted-field scanner | pending | | | |

## Step 13: SWAR regions.csv parse + relaxed-atomic bitset pre-test
- regions.csv rid fields: SWAR comma-index + branch-free digit convert (removes
  a data-dependent branch per lookup row); scalar fallback for the tail and
  over-long ids. Bitset pre-test load made atomic-relaxed (TSAN-clean UB fix,
  identical x86 codegen).
- Local: ~1% wall on full (noisy sandbox); expected larger on the runner where
  single-core speed is lower (load_regions is serial). S 299, M unchanged.
- Run: PENDING
