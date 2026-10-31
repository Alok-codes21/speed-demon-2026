# Track A: performance log

Record each live-board measurement here using its actual run ID and measured results. Do not use local timings as leaderboard measurements.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 32.05 | 228 | 0 |
| 1 | Rust implementation; full-dataset local validation passed | | 0.337 | 40.4 | 51.08 |
| 2 | Full rewrite: mmap input with guard page, 12 line-aligned worker ranges, fixed-digit hour decode, one 64-byte SIMD probe for field separators, per-worker endpoint interning, 4-byte packed records in per-hour chunk chains, global rank merge, per-hour counting sort + nested-selection percentiles, exact-size MAP_SHARED output mapped once and formatted in parallel | run-a83693bf6165 | 0.033 | 34.8 | 74.84 |
| 3 | Per-worker parallel prefault of the mapped log (MAP_POPULATE per range), chunk pages returned with MADV_DONTNEED as hours complete, pair-table integer formatting | pending | | | |
