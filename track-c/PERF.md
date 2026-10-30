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
| 6 | Linux-only source (dropped all Windows paths), one shared SIMD quote scanner (SSE2 + memchr tail) for name and note fields, deduplicated escaped-quote handling, packed country codes (no std::string table). Source: 552 -> ~433 scc code lines. Correctness guard: names over 25 bytes now hashed full-length (FNV-1a) instead of truncated, and all word loads are end-bounded | pending | | | |
