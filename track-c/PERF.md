# Track C: performance log

Every step from the reference to your final version, each with the live-board run ID that measured it. Judges check that the run IDs exist and that each change moved the way you say it did.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 14.44 | 367 | 0 |
| 1 | C++ single-threaded: mmap I/O, 1 MB direct region table, in-place unescape, global name dictionary and 20 KB country bitsets | | 0.88 | 10 | |
| 2 | C++ multi-threaded: 12 threads with CRLF boundary partitioning, lock-free atomic name interning, atomic bitset updates, thread-local stats merging | | 0.25 | 10 | |
