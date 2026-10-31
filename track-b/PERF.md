# PERF.md - Track B optimisation log

Numbers are wall time for the whole run (read corpus + build index + answer
50,000 queries), the way the harness measures it.

Baseline: the organisers' naive Python reference (`b_index.py`).
Published measured reference on the scoring box: **56.35 s** (seed-1 full).
On an Apple M4 Pro the same reference is 27.2 s with 2.29 GB peak RSS.

## Steps

1. **Baseline reproduced locally** on the seed-1 `sample` dataset to confirm
   the harness contract and the expected output hash.
2. **Rewrite in Rust, query-driven index.** Instead of indexing all 609,833
   distinct tokens, index only terms that appear in the 50,000 queries
   (75,747 distinct). Tokenization: byte-class table, no regex, no UTF-8
   decoding. Postings appended in scan order - no sorting anywhere.
3. **Parallel corpus scan (std::thread).** Line-aligned chunks, per-thread
   posting lists concatenated in chunk order (globally sorted without a sort
   pass). Parallel query evaluation on top.
4. **Streaming + selective postings (v2).** docs.txt streamed in blocks
   instead of held resident; positional index dropped for doc-id lists only.
   Memory fell 649 MB -> 253 MB on the board, but phrase verification by
   re-reading candidate docs cost wall time (0.882 s -> 0.992 s).
5. **Runtime-first redesign (v4).** Per team priority, wall time comes
   first, so phrase verification is back on stored positions - but only for
   PHRASE terms, packed as (doc << 13) | pos u32s, verified by binary
   search; BOOL terms keep deduplicated doc-id lists only. The scan keeps
   streaming (8 MB blocks), with a 4 Mbit summary filter and a
   single-u64-slot term table so most corpus tokens never touch the hash
   table, plus a per-thread (term, doc) recency cache for hot vocabulary.
   Dev sandbox (1 vCPU): **2.55 s** wall (vs 4.4 s for the v1 positional
   build), 385 MB peak RSS.

6. **Quota-aware threads + 4 MB blocks (v5).** Thread count now reads the
   cgroup CPU quota (cpu.max / cfs_quota) instead of the host core count the
   container reports (capped at the quota, else host, max 16). Board memory
   did not move (501 MB) - the bloat is not thread buffers; still hunting it.
8. **Peak-memory trim (v7).** Per-thread posting vectors are shrunk to fit
   before the merge, so the build peak is exact parts + index instead of
   parts with Vec growth slack (~1.7x) + index. Merge now precomputes exact
   spans with a prefix sum. Env-gated stage/IO statistics (TRACKB_STATS)
   added for profiling; no effect when off. Local wall unchanged
   (3.2-3.8 s on 1 vCPU), all four correctness gates byte-identical.

7. **AVX2 tokenizer (v6, current).** The corpus scan classifies 32 bytes per
   instruction into token/separator bitmasks (unsigned range tests via the
   xor-0x80 trick, case folded with | 0x20, exact on [a-z0-9A-Z]) and walks
   token runs over the masks. Tokens are hashed chunk-wise straight from the
   scan buffer (8-byte unaligned reads, overlapping tail) with no per-byte
   table lookups; term-table probes compare case-folded in place. Runtime
   detected (is_x86_feature_detected!) with the scalar loop as fallback.
   Dev sandbox (1 vCPU): 3.67 s wall vs 5.1-5.75 s scalar on the full
   dataset; identical output on all four verification datasets.

Note: the board re-runs every track on each push to the branch, so the v5
numbers were recorded on a teammate's later commit whose track-b tree is
identical to 4d9f7f3.

## Wrong turns

- Storing positions for every corpus token (full positional index) was the
  first design; 85% of distinct tokens never appear in a query.
- Dropping positions entirely (v2) minimised memory but the doc re-read
  phrase checks cost more wall time than they saved; positions for PHRASE
  terms only is the better trade under the score formula.

## Verification

| dataset | check | result |
| --- | --- | --- |
| full, seed 1 (the board's data) | sha256 = f86973c2…80927 | match |
| sample, seed 1 | sha256 = eeeec1e9…8a8f | match |
| tiny/sample, seeds 7 and 123 | byte-diff vs Python reference | identical |

## Live board runs

| commit | run | result |
| --- | --- | --- |
| 72d27fd (positional v1) | run-cb0c84fd5f0f | PASS: T=0.882 s, M=649 MB, K=7.96, 45.05 pts (rank 44/58 at the time) |
| 2071a82 (streaming v2) | run-8688a566173e | PASS: T=0.992 s, M=253 MB, K=8.60, 46.71 pts (rank 43/58 at the time) |
| 66d14cf (runtime-first v4) | run-06ed87c66719 | PASS: T=0.886 s, M=477 MB, K=7.96, 45.06 pts |
| 4d9f7f3 (quota threads v5) | run-8b18b13d78cd (measured at d2793ac, same track-b code) | PASS: T=0.863 s, M=501 MB, K=8.02, 45.22 pts |
| 7fa17fc (AVX2 tokenizer v6) | run-ddbd7deaacbb | PASS: T=0.723 s, M=501 MB, K=8.46, 46.36 pts |
| peak-trim v7 (this push) | pending | awaiting board run |
