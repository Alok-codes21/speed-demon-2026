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
| (peak-trim v7) | run-61fcc3fc98c9 | PASS: T=0.989 s, M=265 MB, K=8.97, 46.74 pts |


## Step 7: bigram phrase index + query-only postings rewrite (v10)

Full rewrite of the indexing half, keeping the Docker/cargo scaffold:

- PHRASE no longer stores positions at all. A **bigram inverted index**
  (consecutive query-term pairs, ~40k distinct) answers 2-term phrases with
  one lookup; longer phrases intersect their bigram lists and re-tokenize
  only the surviving documents (median 1) to verify the exact sequence.
  This removes the (doc<<13)|pos positional lists that dominated memory.
- BOOL postings are collected only for the 53k terms that need them
  (21.1M doc ids) and stored delta-varint encoded (~27 MB).
- docs.txt is mmap'd (the harness pre-faults /data before the run's cgroup
  exists, so mapped pages do not count toward M) instead of streamed.
- Verification: spec worked example exact; full/sample/tiny seed-1 outputs
  match the published sha256 (full f86973c2...80927); 1-vCPU dev wall
  3.1 s full dataset, peak RSS 383 MB local (includes the 254 MB corpus
  pages, which the board does not charge).
- Run: run-8919acd63605 - PASS: T=0.868 s, M=228 MB, S=822, K=9.11,
  **47.98 pts** (was 46.74 with v7; rank 43/62 at the time)

## Step 8: bloom-gated scan + flat u32 postings arena (v11)

- A 2 Mbit bloom filter over the query-term hash now gates the hash-table
  probe in the corpus scan: the ~85% of corpus tokens that are not query
  terms are rejected with one L2-resident bit test instead of a 2 MB table
  walk. The same gate fronts phrase-verification lookups.
- The merged postings store changed from delta-varint to a flat u32 arena
  (84 MB), so query evaluation reads doc-id slices directly instead of
  re-decoding: measured 417 MB of varint bytes were being decoded per
  50k-query evaluation (~15x re-decode of the same lists).
- Deliberate trade: board memory rose 228 -> 280 MB to buy wall time.
- Board: T 0.868 -> 0.678 s. Run: run-53dc23e73e19 - PASS: T=0.678 s,
  M=280 MB, S=851, K=9.85, **49.66 pts** (rank 43/63 at the time)

## Step 9: segmented scan + varint streams (v12, this push)

- Gets v11's wall time back at v10's memory: the corpus is scanned in 8
  sequential segments (12 line-aligned stripes each); each segment's
  per-thread postings are merged into per-term delta-varint streams
  immediately and their vectors released (capacity is reused across
  segments, so there is no reallocation churn).
- The merge itself is parallel: each merge thread owns a term range and a
  bigram range, reading every scan thread's stripe lists in order (stripes
  are contiguous doc ranges, so concatenation is globally sorted without a
  sort pass).
- Evaluation decodes term/bigram streams into reused scratch vectors, as in
  v10; the flat u32 arena (and its 84 MB peak) is gone.
- Dev sandbox (1 vCPU): wall 2.4 s full dataset (scan+merge 1.45 s,
  eval 0.95 s), peak RSS 325 MB local including the 254 MB corpus pages the
  board does not charge (v11 was 436 MB).
- Verification: spec worked example exact; tiny/sample byte-diff vs Python
  reference; full sha256 f86973c2...80927 match.
- Run: run-a5a8728c736a - PASS: T=0.793 s, M=149.5 MB, S=892, K=10.25,
  **50.54 pts** (rank 42/63 at the time). Memory 280 -> 149.5 MB as designed.
  A same-code re-run (run-c3ce03fe455f, triggered by a teammate pushing
  another track) measured T=0.857 s / 49.64 pts, so board wall time carries
  roughly +/-8% run-to-run noise.

## Step 10: PGO build on the allowlisted llvm-tools image (v13, this push)

- Dockerfile gains a generator stage (published b_index.py, byte-identical,
  free on the S axis) producing seed-1 sample, seed-7 sample and seed-1 tiny
  datasets, then builds twice on halloumee/speed-demon-rust
  (rust:1-trixie + llvm-tools): -Cprofile-generate, three instrumented runs,
  llvm-profdata merge, -Cprofile-use. Build-time training on generator
  output is explicitly allowed; /data is not mounted at build time.
- No source changes vs v12.
- Run: PENDING
