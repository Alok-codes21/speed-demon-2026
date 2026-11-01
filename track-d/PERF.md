# Track D: performance log

Every step from the reference to your final version, each with the live-board run ID that measured it. Judges check that the run IDs exist and that each change moved the way you say it did.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 72.69 | 893 | 0 |
| 1 | Restored Rust implementation (teammate's step-15 design, adapted from MIT-licensed manusingh090/speed-demon-2026-entry) | run-98555c37390f | 0.176 | 66.6 | 68.64 |
| 2 | Crash fix: BFS-ahead helper prefetch read link[first[u]] with first[u]==m for zero-outdegree nodes; now computes the address with wrapping_add. Output unchanged (byte-identical tiny/sample/full seed-1 gates). Board run pending. | | | | |
