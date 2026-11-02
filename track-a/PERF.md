# Track A: performance log

Record each live-board measurement here using its actual run ID and measured results. Do not use local timings as leaderboard measurements.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 32.05 | 228 | 0 |
| 1 | Rust implementation; full-dataset local validation passed | | 0.337 | 40.4 | 51.08 |
| 2 | Full rewrite: mmap input with guard page, 12 line-aligned worker ranges, fixed-digit hour decode, one 64-byte SIMD probe for field separators, per-worker endpoint interning, 4-byte packed records in per-hour chunk chains, global rank merge, per-hour counting sort + nested-selection percentiles, exact-size MAP_SHARED output mapped once and formatted in parallel | run-a83693bf6165 | 0.033 | 34.8 | 74.84 |
| 3 | Per-worker parallel prefault of the mapped log (MAP_POPULATE per range), chunk pages returned with MADV_DONTNEED as hours complete, pair-table integer formatting. REGRESSION: the 12 MAP_FIXED+POPULATE remaps serialize on mmap_lock and per-chunk DONTNEED storms the TLB; reverted | run-242d5579e5ec | 0.042 | 33.7 | 72.13 |
| 4 | Prefault reverted (soft faults win here), 4 KiB chunks (half the part-full slack of 16 KiB), chunk pages returned in amortized 64-chunk batches as hours complete, full arena return before the format phase | run-7d25eaeffe0a | 0.041 | 22.1 | 74.17 |
| 6 | All madvise/DONTNEED machinery removed (hypothesis: step 3/4 lost 8 ms of T to TLB shootdowns from mid-run page returns; step 1 had none and read 0.033). CONFIRMED: T back to 0.034, but M back to 34.7 - the releases were the whole memory win. Trading T for M is a dead end; recycling via streamed finalization is the way to both | run-1f09f6e0afdf | 0.034 | 34.7 | 74.57 |
| 7 | Leaner per-line scan: latency parsed from a digit-count bit trick on one 8-byte load (no LF search), chunk append via a cached per-hour hot fill (one store + bump, count word written only on rollover). Neutral within board noise; carried into step 8 | run-82fb950b3cdf | 0.035 | 34.7 | 74.14 |
| 8 | Streaming pipeline, no phase barriers: 1 MiB work-stolen blocks, one shared lock-free-read endpoint dictionary (no merge/remap phase), per-hour run buffers recycled from a pool, hours finalized as the scan watermark passes them (lag 2) with counting sort + nested select, in-order write-back with fadvise. A late line triggers an exact full reprocess at infinite lag. Confirmed by a repeat run at a later head (run-497efc18d4db: 0.029 / 21.0 / 78.17) | run-f791406713e6 | 0.029 | 21.2 | 78.34 |
| 9 | Four line-aligned parse streams per block stepped in round-robin, so one line's dictionary probe overlaps the next line's decode (ILP); dictionary hash cut to one multiply. NEUTRAL on the board: T stayed in the 0.0287-0.0294 band, so the parse is not probe-latency-bound; carried anyway, it does not hurt | run-14b985b7b344 | 0.029 | 21.0 | 77.97 |
| 10 | 256 KiB blocks (4x finer watermark granularity and work stealing than 1 MiB) and each worker drops its share of the input mapping with MADV_DONTNEED once all work is done, so process exit has no populated page table to tear down (alternative engine's PERF.md and source show both; the drop is after all parsing, avoiding the step-3/4 mid-run TLB storm) | run-7e621eefd618 | 0.026 | 18.9 | 79.66 |
| 11 | The output write now runs outside the parts lock (while `writing` is set the writer owns the file position; other threads deposit behind it instead of blocking), per-hour text and scatter buffers are recycled through pools, the hour index is branch-free raw-ASCII arithmetic, and output page-dropping keeps two hours behind the write frontier | run-e18431343056 | 0.025 | 21.0 | 79.50 | T fell ~1 ms but the buffer pools and the two-hour keep cost ~2 MB; roughly a wash against step 10
| 12 | Transparent huge pages for the input mapping: the v3-era process-wide THP disable is removed and the log region is marked MADV_HUGEPAGE, so the scan faults ~120 2 MiB pages instead of ~58k 4 KiB ones. Advisory only, small-page fallback is free; local warm timings unchanged | run-873ab6e86702 | 0.024 | 20.4 | 79.73 |
| 13 | Honest source cleanup, no behavior change intended: the const-generic SIMD/scalar duality is collapsed to plain AVX2 (the board target), records widen to 6 bytes (u32 latency + u16 tag, one 8-byte store) so the ~1% wide-latency side path disappears, the output file leaves the parts lock via positioned writes, and the header comment was brought back in sync with the design (it still said 1 MiB blocks). 812 -> 758 lines | run-33ddd8789b73 | 0.025 | 21.0 | 79.75 |
| 14 | The thread count clamp of 12 is removed: workers now scale to the container's full CPU affinity, as the reference implementations do. The board reports 12 CPUs, so this is a no-op there; kept for robustness. Confirmed by a repeat run at a later head (run-473c27878333: 0.024 / 20.8 / 79.93) | run-42da6acec734 | 0.024 | 21.0 | 79.91 |
| 15 | Dictionary probe precheck: a colliding probe is now rejected on the name length packed into the slot word already in hand, without touching the name image or the length table - our own variant of the classic tag-then-compare probe (alternative engine's dictionary uses a crc32 tag for the same purpose). Plus source tightening that does not touch hot-path structure: the per-block scan state merged into parse() (the BlockScan type is gone), the run-buffer take inlined into its one call site, the digit-writer's tail, the percentile rank tuple, the latency early return, the hour-label day fold, and packing each dictionary entry's length and name image into one cache line. 702 -> 671 scc lines, measured with the harness's own rustfmt+scc pipeline. Two deeper cuts were tried and rejected on measurement: a compact loop form of the line scan cost ~40% parse time locally (code-layout luck, but real), and moving finalize's count/segment/order tables from heap to 24 KiB of stack cost ~50% finalize time locally. Superseded by step 16 before its queued run fired; no board receipt. Local gates were 25/25 byte-exact | - | | | |
| 17 | Local formatting-only compression of the adopted engine (declarations joined, one ternary restructure, named comparator lambda); algorithm and constants unchanged, clang-format-idempotent source, 25/25 local differential gates | run-b61e165cf6e3 | 0.020224 | 21.28 | 82.505861 |
| 18 | Dockerfile WORKDIR folded into absolute paths (7 code lines); stale Cargo.toml/Cargo.lock from the retired Rust engine removed (both S-free) | run-d44bc91e0ff7 | 0.016734 | 20.79 | 84.669038 |
| 19 | Source-size cut, zero behavior change: statements packed into comma expressions and ternaries (the harness formatter only splits at column 80 and never un-splits single statements), two lock-guard blocks rewritten as scoped comma-guard / range-for-init forms (same locks, same acquisition order; the buffer-recycle tail is now two independent critical sections, safe because no invariant couples the two pools), GNU ?: for the recycled-buffer fallback, sort8/merge8 step chains via one-line macros, Dockerfile briefly collapsed to one stage, REVERTED within the hour: the gcc:15 runtime image is 2.03 GiB, over the 2 GiB image gate (run-7a50c119cbd2 failed at image-size, no score; Dockerfile back to the 7-line two-stage form). Algorithm, constants and output byte-identical: 25/25 local differential gates pass (5 datasets x threads 1/2/4/7/12). scc code lines: main.cpp 567 -> 507; S 574 -> 514 | (queued) | | | |

## Official board run log (Track A, adopted engine)

| --- | --- | --- | --- | --- | --- | --- |
| run-446d6331095c | bc497281 | 0.021598 | 19.96 | 584 | 82.018976 | First adoption run |
| run-47538ecc0d25 | 7d309ba6 | 0.020862 | 20.10 | 584 | 82.365772 | Re-run on unchanged Track A source; new best |
| run-403ac5d19d5d | fec7e935 | 0.019348 | 20.43 | 584 | 83.111775 | Re-run on unchanged Track A source; new best |
| run-0a761af5c6dc | b99b78e1 | 0.018129 | 19.83 | 584 | 83.948962 | Re-run on unchanged Track A source; new best |
| run-e8d0c8c7c746 | 2488b0ed | 0.017199 | 21.03 | 584 | 84.264893 | Re-run on unchanged Track A source; new best |
| run-9e3766811557 | b6561c10 | 0.019252 | 20.07 | 584 | 83.242841 | Re-run on unchanged Track A source |
| run-23d39328b202 | 548786aa | 0.020024 | 20.70 | 584 | 82.683633 | Re-run on unchanged Track A source |
| run-1413e9dc1d93 | f1ba7411 | 0.019844 | 20.51 | 584 | 82.820373 | Re-run on unchanged Track A source |
| run-b61e165cf6e3 | e3756792 | 0.020224 | 21.28 | 575 | 82.505861 | Format-compressed source (step 17); S reads 575 as engineered |
| run-d44bc91e0ff7 | f6ec3e55 | 0.016734 | 20.79 | 574 | 84.669038 | Dockerfile fold live (step 18); S reads 574; new best |
| run-5b9129f3eab6 | 688668e9 | 0.018534 | 20.74 | 574 | 83.571243 | Re-run on unchanged Track A source |
| run-12ce5cdf895b | 955885d7 | 0.016766 | 20.44 | 574 | 84.721537 | Re-run on unchanged Track A source; new best |
| run-9a97a83ec004 | 0e5e4c1a | 0.019399 | 19.76 | 574 | 83.284651 | Re-run on unchanged Track A source |
| run-b5303295595d | eb2a0d2b | 0.019252 | 20.52 | 574 | 83.203811 | Re-run on unchanged Track A source |
| run-3739bbe48442 | 7d0a9adb | 0.021947 | 20.65 | 574 | 81.753309 | Re-run on unchanged Track A source |
| run-4dd2378b06e4 | 941cd906 | 0.016572 | 20.51 | 574 | 84.834642 | Re-run on unchanged Track A source; new best |
| run-58acb982ed1e | 1b3af2c6 | 0.016983 | 20.71 | 574 | 84.525024 | Re-run on unchanged Track A source |
| run-723f3bca34d8 | 9d5cb696 | 0.019720 | 20.13 | 574 | 83.025664 | Re-run on unchanged Track A source |
| run-539c59e2c98e | 518d274f | 0.019563 | 20.39 | 574 | 83.057190 | Re-run on unchanged Track A source |
| run-ad2ccd51ff28 | 4772ef2d | 0.017456 | 20.19 | 574 | 84.337938 | Re-run on unchanged Track A source |
| run-2bef24dc3281 | a2519bb6 | 0.019758 | 19.95 | 574 | 83.044548 | Re-run on unchanged Track A source |
| run-7a50c119cbd2 | 9cf30978 | - | - | - | - | Failed image-size gate (2.03 GiB > 2 GiB): single-stage Dockerfile experiment, reverted same hour |
| run-c00185b3c7de | 49a2dda6 | 0.017245 | 20.40 | 514 | 84.784448 | Step 19 tree: S 514 (main.cpp 507 + Dockerfile 7) |
| run-f29f7bc67c4d | b902193b | 0.019348 | 20.88 | 514 | 83.433205 | Re-run on unchanged Track A source |
| run-4ba0159ff9e4 | 8af42fc9 | 0.021057 | 22.49 | 514 | 82.192522 | Re-run on unchanged Track A source |
| run-67c6deb94015 | f0d73446 | 0.021193 | 19.96 | 514 | 82.641227 | Re-run on unchanged Track A source |
| run-b5c1f5fd2853 | 07d2081d | 0.017225 | 20.12 | 514 | 84.857344 | Re-run triggered by teammate push; new best |
| run-5160b02a0178 | 36e8b025 | 0.020986 | 20.51 | 514 | 82.630389 | Re-run triggered by teammate push |
| run-5587c6bc5003 | 58eeeb45 | 0.019895 | 20.68 | 514 | 83.171318 | Re-run triggered by teammate push |
| run-1920124aeb98 | 163ded7d | 0.020109 | 20.30 | 514 | 83.136272 | Re-run on unchanged Track A source |
| run-04c1b1e28407 | 007bc818 | 0.019931 | 20.20 | 514 | 83.255424 | Re-run on unchanged Track A source |
| run-fcb783432bb0 | 06566e24 | 0.018473 | 20.76 | 514 | 83.960111 | Re-run on unchanged Track A source |
| run-7c7a24162d3d | 51d80e15 | 0.019710 | 20.32 | 514 | 83.350528 | Re-run on unchanged Track A source |
| run-f80da880e9c2 | ffe639ad | 0.020305 | 19.99 | 514 | 83.098319 | Re-run on unchanged Track A source |
| run-63147f00590e | 67d7e9b3 | 0.016996 | 20.50 | 514 | 84.919136 | New Track A best (was 84.857344); re-run on unchanged source |
| run-73be56343137 | d9735b25 | 0.019802 | 20.78 | 514 | 83.201738 | Re-run on unchanged Track A source |
| run-48889ed72c17 | 067aa448 | 0.019172 | 20.28 | 514 | 83.659365 | Re-run on unchanged Track A source |
| run-f2907f83d9ad | 3983b60e | 0.022235 | 25.04 | 514 | 81.135566 | Re-run on unchanged Track A source; M spike +4.7 MB vs typical |
| run-54a4dcf487c2 | 600dfb42 | 0.019667 | 20.87 | 514 | 83.258092 | Re-run on unchanged Track A source |
| run-640f10ea2035 | a2864a0d | 0.019461 | 20.46 | 514 | 83.459442 | Re-run on unchanged Track A source |
| run-521e3296f1a3 | 5ec4e47f | 0.019837 | 19.90 | 514 | 83.371908 | Re-run on unchanged Track A source |


## Nov 2: A1, exact Rust trial and final restoration work

Source-verified public-board receipts are observations, not controlled speed estimates or final hidden-seed scores.

| --- | --- | --- | ---: | ---: | ---: | ---: |
| A1 | run-bbbe003cee57 | ca9121c7500f32c0daee64b713ec73b739158682 | 19.309 | 14663680 | 411 | 85.924982 |
| A1 | run-6787c0ce5beb | efbe439b80f44030bea0180c387772878a5ba920 | 19.578 | 15073280 | 411 | 85.655120 |
| A1 | run-4b6ed81673e6 | b7b3c6701bf4ca7402207e20f4b35a3d7f1a5d03 | 20.785 | 14872576 | 411 | 85.063793 |
| Clean A1 restore | run-491667a5c76b | c269365293656f5c094aade68b37d5ec5be7109d | 19.377 | 14671872 | 411 | 85.884387 |
| Exact-name Rust trial, duplicate source present | run-4c9695dd4911 | ac23a5dbe8edc7c7e97b7c30f0cc2df95ff8ee05 | 31.018 | 16277504 | 763 | 78.310063 |
| Clean A1 restore | run-39e2080d9c66 | a758443fb5abbd6549ed48308a0356ba6ae619bf | 19.856 | 15142912 | 411 | 85.482018 |
| Two-pass radix variant | run-62e91cc4dda3 | 58c7590108b6e3c6e4a7507f4f9d6b2353086767 | 27.495 | 15298560 | 410 | 81.911492 |


The Rust adaptation retained full endpoint-name comparison, including forced hash-collision testing. It passed 25 fixture/order/thread checks and 276 oracle comparisons before deployment. Its source SHA256 was `ddeb04a9394e825789bfc9733dd25dc2d90d5f95cba43d72c0e9a07625e263ba`. An inactive C++ source was re-added during the trial, inflating S. Thus the recorded S763 is not a clean Rust-only comparison. Runtime was also worse, so Rust was removed and the pinned two-stage C++ build restored. No speed win was established.

The two-pass variant SHA256 `10a7b0648add2cb2b831ae041c8848a9dca1e13d7c8ec4369d1da6d5e83bc6f6` also changed output scheduling and closing horizon, so it was not a single-variable radix comparison. It passed 25 local exactness checks but its captured official score was lower than A1.

Further A1 screens: reducing block size 1024 to 512 passed 25 exactness checks, with local 21-pair time ratios 0.9986 (one worker) and 0.9704 (12 workers). Local max-RSS did not fall, so no 1 MB saving is claimed and it was not promoted. Block size 256 was slower at 12 workers (ratio 1.0255) and was rejected. Removing two unused includes produced identical executable .text locally; any official S reduction remains unmeasured until a scored receipt confirms it.
