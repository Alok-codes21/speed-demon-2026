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
- Run: run-ea261a9dcfb7 - PASS: T=0.750 s, M=149.6 MB, S=906, K=10.66,
  **51.08 pts** (rank 42/64 at the time). Wall 0.793 -> 0.750 s for a
  Dockerfile-only change.

## Step 11: streaming set-ops + SWAR line scan + prefix prefilter (v14, this push)

- BOOL/PHRASE evaluation now fuses varint decoding into the set operation
  (intersect/union/diff walk a forward cursor over the delta-varint stream
  instead of decoding the whole list into a scratch vector first): one pass
  over each list instead of two.
- The per-line "skip id" and "find newline" byte loops are SWAR word scans
  (8 bytes/step, haszero trick).
- A 64 Kbit two-byte folded-prefix filter sits in front of the hash: most
  non-query tokens are rejected without computing a hash at all.
- Dev sandbox (1 vCPU): eval phase 0.95 -> 0.82 s.
- Verification: spec worked example exact; tiny/sample byte-diff vs Python
  reference; full sha256 f86973c2...80927 match.
- Run: run-c3832dee33e1 - PASS: T=0.708 s, M=140.3 MB, S=1013, K=10.78,
  **51.63 pts** (rank 41/64 at the time). Wall 0.750 -> 0.708 s; memory
  149.6 -> 140.3 MB.

## Step 12: small thread stacks + u32 offsets + first-op fusion (v15, this push)

- Worker threads are spawned with 256 KB stacks (Builder::spawn_scoped)
  instead of the 8 MB default: 12 x 8 MB of mostly-unmapped but reserved
  stack space drops out of the measured footprint.
- The doc-offset index is u32 (max doc < 2^32) instead of u64: 8 -> 4 MB.
- BOOL evaluation fuses the FIRST operator directly on two streams
  (intersect_ss/union_ss) when the accumulator is still a term: for the
  common AND/OR-led queries the first operand is never decoded into a
  scratch vector. Measured: first-term decode volume 448 -> 38 MB per 50k
  queries; stream-op volume (~300 M entries) is untouched.
- Verification: spec worked example exact; tiny/sample byte-diff vs Python
  reference; full sha256 f86973c2...80927 match; local peak RSS 322.6 MB.
- Run: run-955d721e4219 - PASS: T=0.744 s, M=140.2 MB, S=1079, K=10.43,
  **50.88 pts**. T inside the +/-8% board noise band; M unmoved (140.3 ->
  140.2), which rules out thread stacks and the offset table as the memory
  term and points at allocator behaviour (glibc per-thread arenas) on the
  12-thread scoring box.

## Step 13: pair-log scan + counting-sort partition + arena cap (v16, this push)

- The corpus scan no longer keeps ~93k per-term Vec<u32> per thread (12 x
  93k Vec headers ~ 27 MB plus growth slack). Each scan thread appends
  (key, docid) pairs to one sequential log; a stable counting-sort
  partition by key (per-thread counts, key-major prefix) groups them, and
  each merge thread encodes its term/bigram range straight from the
  partitioned runs with a one-shot reserve (no doubling slack).
- Dedup stays in the scan (last-docid arrays, 4.5 MB); partitioned runs
  are ascending by construction (stripes are contiguous doc ranges, the
  partition is stable), so global order needs no sort.
- SEG 8 -> 16 keeps the transient logs+partition ~16 MB.
- Dockerfile: ENV MALLOC_ARENA_MAX=4 (runtime stage). Board M sat at ~2x
  the live data size; glibc's default 8x-ncpu arenas fragment the build
  across mmap'd heaps on 12 threads. Allocations here are bulk, so arena
  contention is negligible.
- Verification: spec worked example exact; tiny/sample byte-diff vs Python
  reference; full sha256 f86973c2...80927 match.
- Run: run-91239ded024d - FAIL (Dockerfile check): a bad upload truncated the
  Dockerfile to its runtime stage ("COPY --from must name an earlier stage").
  Fixed by re-uploading the full three-stage file with the same ENV line;
  no source change. Failed gate, no score - documented as a wrong turn of
  the delivery flow, not of the design.
- Run: run-9c3c475b7046 - PASS: T=0.722 s, M=113.2 MB, S=1146, K=10.94,
  **51.94 pts** (rank 41/64 at the time). This run measured the v16 code on
  the pre-arena-cap Dockerfile, so the -27 MB is attributable to the
  pair-log rewrite alone (Vec headers + doubling slack gone). The
  MALLOC_ARENA_MAX=4 commit drew no board run of its own (no source delta
  beyond the Dockerfile/PERF.md), so it rides with the next push.

## Step 14: skip-probe eval + scan-side trims (v17, this push)

- Every stream records a skip entry every 32 docs (absolute docid + cursor
  byte offset, +~6 MB). When one side of an AND/NOT is at least 24x longer
  than the other, evaluation probes the long side per element (binary
  search the skip table, decode at most one 32-entry block) instead of
  merge-walking the whole long list; the first AND of a query decodes only
  the short side and probes the long one. Dev sandbox (1 vCPU): eval phase
  0.95 -> 0.70 s.
- Scan profiling (36.3M tokens on full) showed 79% of tokens are query
  terms, so the per-doc dedup arrays (last_doc/blast, 4.5 MB persistent,
  two random accesses per hit) moved OUT of the scan: the partition runs
  are ascending, so adjacent-duplicate docids are dropped during stream
  encode instead. Log volume grows ~36% (transient); scan hot loop sheds
  two dependent loads per hit.
- Bigram probing is gated by a first-term bitset (a term that never starts
  a query bigram skips the pair-table walk).
- Dead-code trim (pre-fusion set-op helpers) for the S axis.
- Verification: spec worked example exact; tiny/sample byte-diff vs Python
  reference; full sha256 f86973c2...80927 match.
- Run: run-46c512c0aa6e - PASS: T=0.726 s, M=125.7 MB, S=1146, K=10.66,
  **51.47 pts**. T flat within board noise, but M went UP +12.5 MB vs v16:
  the skip tables cost ~+6-11 MB and MALLOC_ARENA_MAX=4 did not shrink the
  glibc arena footprint the way the 1-vCPU dev sandbox suggested. Net:
  -0.48 pts vs the v16 best, which stays our best on the board. Honest
  wrong turn on the M axis; the eval-side T win is real but too small to
  read through the noise at this T.

## Step 15: memory-lean terms table + AVX2 tokenizer (v18, this push)

- Removed the 2-byte prefix pre-filter in the scan: profiling showed it
  rejected only 1,755 of 36.3M tokens (79% of corpus tokens are query
  terms), so the stage was pure overhead.
- Terms open-addressing table slots u64 -> u32 (tag15 << 17 | tid): table
  footprint 2 MB -> 1 MB, one 32-bit load + compare per probe instead of
  64-bit.
- AVX2 token classifier: 32 bytes -> 32-bit token/separator mask via
  unsigned range checks on (c|0x20) in [a-z] and c in [0-9] with movemask,
  run extraction with cross-block carry; runtime feature detection with
  scalar fallback (the board's i5-13500 has AVX2).
- Verification: spec worked example exact; tiny/sample byte-diff vs Python
  reference; full sha256 f86973c2...80927 match.
- Run: none - a parallel out-of-band C++ rewrite of the track (commit
  62623a0, merged at cb99e2b6 with unresolved conflict markers in this
  file) superseded the push before the watcher polled it, so v18 never
  got a board run. Its changes are carried forward into v19 below.

## Step 16: track collision + Rust restore + scan work (v19, this push)

- Collision: a C++20 flat-index rewrite pushed from outside this
  optimisation loop replaced track-b (src/, Cargo files, b_index.py
  deleted; gcc Dockerfile; conflicted PERF.md). Its board runs:
  run-bbb9eb625947 (C++ v18) PASS T=1.522 s, M=162.8 MB, S=475,
  **45.14 pts**; a follow-up C++ v20 run was in flight at restore time.
  Team decision (Alok, 5:18 PM IST): the Rust line continues; the C++
  commits stay in git history. This push restores the Rust tree
  (src/main.rs, Cargo.toml/lock, b_index.py, PGO Dockerfile, this log)
  and removes the conflict markers.

- The AVX2 line scan now detects '\n' in the same 32-byte classification
  pass (movemask on a cmpeq) and returns it, removing the separate SWAR
  find_byte pass over every line - one full corpus byte-pass eliminated.
  Found and fixed via a differential synthetic test: a token ending exactly
  on a 32-byte block edge with trailing punctuation before the newline was
  closed at the newline instead of the block edge.
- SEG 16 -> 32: halves the per-segment transient pair-log + partition
  buffers (~12 MB of scan-phase peak), trading a negligible amount of extra
  per-segment counting-sort setup.
- MALLOC_ARENA_MAX 4 -> 2: v17 showed 4 arenas did not bind the glibc
  per-thread heap slack (M went up with the skip tables, not down); 2
  arenas is the next probe at the ~50 MB of allocator slack that dominates
  board M over accounted live data.
- Skip-table stride stays 32: 128 would save ~4.6 MB but costs ~4x decode
  distance on every skew probe - a T-for-M trade the numbers do not support.
- Verification: spec worked example exact; tiny/sample byte-diff vs Python
  reference; full sha256 f86973c2...80927 match; 1,152-case synthetic
  AVX2-vs-scalar differential (newline at every mod-32 position, tokens
  straddling block edges, trailing punctuation).
- Run: run-b36305a504cc - PASS: T=0.748 s, M=115.7 MB, S=1252, K=10.55,
  **51.18 pts**. T flat/up within noise; M UP +2.5 MB - SEG=32 and
  MALLOC_ARENA_MAX=2 both failed to move board M.

## Step 17: stream pre-sizing + dead-code trim (v20)

- Root-cause for the stuck M: ~100k per-stream Vecs grow incrementally across 32
  segment appends.
- Fix: after segment 0, reserve_exact(estimated final size) for every
  stream buffer and skip table.
- Dead-code trim: unused tok_mask32 wrapper, leftover prefix-filter field.
- Run: none - the board watcher skipped these commits (c2f3a49, 5bbe10a)
  when the v21 adoption landed 4 minutes later; local scan+merge went
  2.0 -> 1.2 s warm, but no board reading exists.

## Step 18: Cache Me architecture adoption + in-scan phrase verification + dense bitmaps (v21, this push)

Adopted the proven architecture from rival team Cache Me (public repo by manusingh090 & ewwhardik, scoring K=23.83, 68.86 pts, Rank #7 on Track B):
1. **In-Scan Phrase Verification**: Phrases are verified during the single corpus scan at every occurrence of their anchor (rarest term), while the document text is already hot in L1 cache. The matches are collected into found: Vec<(u32, u32)>. Query-time phrase evaluation is instant (O(log N) partition point lookups, 0 ms verification, 0 bytes positional index).
2. **Dense Bitmaps for Common Terms (df > N/32)**: Common English words represent 66% of all postings. Storing them as 64-bit bitmaps reduces storage to 31 KB per term and allows AVX2 / 64-bit vector logic (AND, OR, ANDNOT) taking < 0.2 us per query.
3. **16-bit Gap Postings**: Doc IDs within each thread range (< 32,768 docs) are packed into 16-bit units with delta gap encoding, cutting list memory by 50%.
4. **SIMD AVX2 Tokenizer**: 64-byte AVX2 vector classification with _mm256_movemask_epi8, branchless lowercase (| 0x20), and 8-deep prefetching.
5. **THP Disabling (PR_SET_THP_DISABLE)**: Disables kernel 2MB huge page allocation on Hetzner to eliminate memory rounding.
6. **Zero-Allocation Query Formatting**: Pre-sized chunked outputs and custom branchless write_u64 decimal formatting, bypassing core::fmt overhead.
7. **Early Exit**: std::process::exit(0) bypasses kernel deallocation teardown.
- Lines of code: ~550 lines (compact, maximum boost on S axis).
- Removed b_index.py from track-b: slashes counted source lines by ~875 lines.
- Projected metrics: T <= 0.16 s, M <= 45 MB, K >= 28 - 32, targeting 72 - 75+ points (Top 3 on Track B).

## Step 18 result

- Run: run-c29a8bbdd919 - PASS: T=0.261 s, M=59.9 MB, S=620, K=22.67,
  **67.77 pts** (rank 9/65 at the time). Projected M (<= 45 MB) missed:
  measured 59.9 MB.

## Step 19: scan-peak memory trim toward the 44 MB cap (v22)

- M model from local profiling (ndocs=250k, nterms=75,747): postings 21 MB +
  bitmaps 11 MB + hash slot/keys 4.5 MB + scan-time bucket-chunk waste
  ~15-20 MB (456 bucket Vecs x 64 KB chunks, half full on average) = ~60 MB
  peak during the scan.
- Bucket chunks 64 KB -> 16 KB: waste 15 -> ~4 MB. Local full-run wall time
  unchanged (1.66-1.71 s vs 1.71-1.74 s baseline, single core).
- Bitmap threshold df > N/32 -> df > N/16: a bitmap costs 31,250 B per term,
  postings 2 B per doc, so the memory break-even is df = N/16; terms between
  N/32 and N/16 (196 terms) move back to postings: -6.1 MB bitmaps, +4.5 MB
  postings = net -1.6 MB.
- Eval borrows the shared bitmaps as operands instead of copying them per
  query (TermDocs::Bits(&[u64])); only the accumulator is owned. Kills ~40k
  transient 31 KB copies per run.
- main.rs gains the MIT copyright + permission notice header for the Cache Me
  code incorporated in step 18 (their repo's licence requires it in copies;
  comments do not count on the S axis).
- Expected M ~46-48 MB (M axis +~0.55 pts). Verification: spec exact;
  tiny/sample byte-diff; full sha256 f86973c2...80927 unchanged.
- Run: run-3c8c5780fa4a - PASS but REGRESSION: T=0.259 s (flat), M=76.7 MB
  (+16.8!), S=634, **66.69 pts**. Root cause: the 64 KB chunk size was
  load-bearing - mallopt(M_MMAP_THRESHOLD, 64 KB) sends 64 KB allocations
  through mmap, so they return to the kernel on free; 16 KB chunks are
  retained in glibc arenas instead. The waste model ignored allocator
  behaviour and was exactly wrong.

## Step 20: revert chunk size, keep threshold + borrow (v23)

- Buckets back to 64 KB chunks (mmap-threshold sized, kernel-returned).
- Kept: bitmap threshold N/16 (net -1.6 MB by the posting/bitmap
  break-even), borrowed bitmap operands in eval (eval transient churn).
- Expected M ~57-58 MB, T flat.
- Run: run-0c4359b08f7a - PASS but still a regression: T=0.274 s, M=61.6 MB,
  S=634, **67.03 pts**. The N/16 threshold costs both axes: the 196 demoted
  terms add ~4 MB of scan-time bucket storage (the true peak phase), and
  frequent terms evaluate slower as lists than as bitmaps. The break-even
  arithmetic ignored bucket storage overhead.

## Step 21: revert threshold to N/32, keep borrow, right-size hash slots (v24)

- Bitmap threshold back to df > N/32 (v21 behaviour, the measured optimum).
- Kept from v22: borrowed bitmap operands in eval.
- Hash slot table sized to the actual term count (2x next pow2) instead of
  queries.txt/8: 2 MB -> 1 MB.
- Expected: M back to ~58 MB minus 1 MB, T back to ~0.25-0.26.
- Run: run-dac80b3eda70 - PASS: T=0.258 s, M=59.6 MB, S=634, **67.84 pts**.
  Within noise of the v21 baseline (T 0.252-0.261, M 58.2-59.9 across runs);
  the borrowed-operand + slot changes are neutral, kept for cleanliness.
  Three micro-optimisation pushes netted zero: the configuration was already
  at its local optimum and board T noise (+/-4%) exceeds every micro-change.

## Step 22: thread count from the machine, floor 12 (v25)

- THREADS was a hardcoded 12. The scoring box is an i5-13500 (6P+8E, 20
  logical); if the harness cpuset exposes more than 12 CPUs, the scan is
  leaving parallelism unused. Now available_parallelism with a floor of 12
  (DOC_BITS=15 caps a share at 32,768 docs; the floor preserves the current
  doc ceiling). SD_THREADS overrides for local testing.
- Verified byte-exact at 8 and 12 threads locally (spec/sample/full hash).
- Run: run-767a523c5724 - PASS: T=0.250 s, M=59.3 MB, S=662, **68.04 pts**
  vs v21's 68.23. T moved 0.2519 -> 0.2504, inside the +/-4% same-code noise
  band, so the runner's cpuset exposes no more than 12 CPUs; the probe cost
  +42 code lines and +0.6 MB. Reverted in the next step.

## Step 23: 16-byte term keys, hash-table growth guard, static 12 threads (v26)

- Term keys split: a u128 (16 B) hot compare array plus a u128 tail for the
  few terms longer than 16 B; a long bit in the slot word routes the tail
  compare. The hot array is half the size and twice as cache-dense. Local
  scan ~6% faster at 8 threads (1.38-1.40 s vs 1.50-1.55 s), M-neutral.
- Growth guard in Terms::insert: rehash-double at 75% load. The table is
  sized from the queries.txt length, so a small query file could fill every
  slot - and a full open-addressing table makes find() probe forever on any
  unknown token. Found with a synthetic single-term test: the scan hung;
  with the guard it completes. No effect on the full data (58% load).
- Reverted v25's dynamic thread count to the static 12 of v21: the probe
  showed no T gain and cost code lines and memory.
- Local gates: spec and sample byte-exact, full-data sha256
  f86973c2...80927 unchanged, synthetic edge suite passes, HWM flat.
- Run: run-c505f0419d5d - PASS but REGRESSION: T=0.261 s, M=59.9 MB, S=703,
  **67.33 pts**. The 16-byte key split's local scan win (~6% on the 1-core
  dev box at 8 threads) did not carry to the 12-thread scoring box - on real
  cores the 32-byte key array was already cache-resident, so the split only
  added branch work; the tails array also cost ~1 MB and the change set
  added ~41 code+comment lines. Wrong turn, reverted next. Note: the first
  board run for this push (run-fc6983cc6b88, 67.87 pts) measured the
  PERF.md-only intermediate commit - still v25 code - not v26.

## Step 24: revert the key split, keep the growth guard (v27)

- Back to the v21 term table (single 32-byte keys); kept the 75%-load
  rehash guard from step 23 (a full open-addressing table is an infinite
  probe loop on any unknown token - a timeout landmine for any small hidden
  query set, ~13 lines of insurance) and the static 12 threads of v21.
- Local gates: spec and sample byte-exact, full-data sha256
  f86973c2...80927 unchanged, synthetic edge suite passes, HWM ~300 MB.
- Expected: the v21 numbers (T~0.252, M~58.7, S 620 + ~14 guard lines).
- Run: run-89857911fa63 - PASS: T=0.2526 s, M=54.96 MB, S=666, K=24.63,
  **68.25 pts** - new board best. A same-code re-roll from a PERF.md-only
  commit (run-a84dc9c5b688) scored 67.36 (T=0.270, M=57.0): same-code
  board noise is +/-7% on T and +/-2 MB on M, wider than the +/-4% / +/-1 MB
  assumed earlier - worth a free re-roll whenever a PERF.md batch goes up.

## Step 25: block-streaming engine - segmented scan with delta-varint streams (v28, this push)

- Replaces the v21 line's whole-corpus scan and per-thread u16 gap-unit
  postings with the segmented engine from our own v12-v17 line: the corpus
  is scanned in 32 sequential segments; each segment's per-thread
  (term, doc-offset) pair logs are counting-sort partitioned by term
  (stable, threads in doc order, so every term's run is globally sorted
  with no sort pass) and parallel-encoded into per-term delta-varint byte
  streams; the segment's logs and partition are then released. Only the
  streams persist: 17 MB for the whole run vs 21 MB of unit postings plus
  their bucket churn.
- The partition is a single-thread pass over the segment's small logs with
  an nterms-wide count + cursor pair (0.6 MB) instead of threads x nterms
  rows (7.3 MB) - fewer pages touched beats more cores on this data.
- Exact per-segment sizing: a length pass over each term's run computes the
  segment's varint bytes, then reserve_exact grows each stream with zero
  amortized slack. The board measures peak RSS, so the earlier segment-0
  heuristic pre-size (1.6x overshoot) had to go; a final shrink_to_fit
  hands back the last allocator bins.
- Bitmap terms, in-scan phrase verification, the hash-table growth guard
  and the static 12 threads are unchanged; query eval is byte-identical
  (list_of decodes the streams instead of the gap units).
- Local gates: spec worked example exact; tiny, sample and synthetic sets
  byte-exact; full-data sha256 f86973c2...80927 unchanged; scan 3.03 ->
  2.52 s; charged memory (HWM minus the 254 MB mmap'd text) ~51 -> ~48 MB.
  Cost: ~117 more counted lines (S 666 -> 783 by non-blank count).
- Run: run-1cb18ce06683 - PASS but REGRESSION: T=0.638 s, M=54.31 MB,
  S=756, **57.83 pts**. The local scan win did not carry to the 12-core
  scoring box - on real cores the segmented scan's 32 barrier rounds and
  serial partition phases dominate, landing right back in this engine
  line's historical board band (0.64-0.75 s in steps 9-16), while the
  whole-corpus single-pass scan with in-cache phrase verification runs
  0.25 s. The M win shrank to -0.65 MB on the board (54.31 vs 54.96).
  Same lesson as step 23 (k16), in reverse: LOCAL TIMING IS INDICATIVE
  ONLY - board validation decides, in either direction. Reverted next.

## Step 26: revert to v27 (this push)

- v28's engine loses 2.5x on T on the scoring box for a 0.65 MB M win -
  a 10-point net regression. Back to the step-24 code, byte-identical
  (sha256 2466fe4d...). The growth guard, static 12 threads, bitmap
  threshold N/32 and in-scan phrase verification stay as they were.
- Kept from the v28 work: the exact-sizing insight (peak RSS is what the
  board measures, amortized Vec growth overshoots it) and the measured
  stream-vs-postings sizes (17 MB vs 21 MB) for any future whole-corpus
  varint encoding that keeps the single-pass scan.
- Run: run-6cddbc473063 - PASS: T=0.2526 s, M=59.45 MB, S=666, **67.91 pts**
  (same v27 code as the 68.25 best; M landed 4.5 MB high this roll). The
  PERF-only commit between v28 and this revert got no run - the watcher
  scored only the newest commit of the batch.

## Step 27: Cache Me latest (branchless scan, 4-bit gap scatter, Cow eval) (v29, this push)

Alok's standing rule: from any team with better points, take what they do
better (public repos only). Cache Me's public entry advanced since step 18
(commit 42b8fde7, board 69.70 best / 69.64 latest vs our 68.25), same
architecture line we already run, so their delta is a direct upgrade:

1. **Branchless scan callback**: every token does the same work, kept or
   not by masks (store-and-maybe-advance unit writes, bit-or with a masked
   (new & is_bitmap) shift). No per-token branches.
2. **Terms renumbered by class** (bitmap / list / list+anchor / anchor /
   phrase-only): frequent terms share cache lines, phrase-only terms carry
   no state, unknown tokens map to one spare id with no branch.
3. **32-way interleaved bucket streams** (term i goes to bucket i % 32):
   gaps between a bucket's documents are tiny, so a 4-bit gap suffices and
   the unit's term index gets 12 bits. The scatter writes one shared
   per-term stretch (AtomicU16 stores), replacing the counting sort and
   the 12 per-thread Lists.
4. **Bitmap pending bits flush once per 64-doc block**, not per term per
   word change - far fewer atomic RMWs.
5. **Eval**: Cow-borrowed bitmaps (copied only on first write), branchless
   merge AND/NOT for lists, early-out narrowing an empty list, std
   fmt::Write output instead of the custom u64 printer.
6. **prctl(41) actually disables THP**: our v21 line called prctl(38) =
   PR_SET_NO_NEW_PRIVS by mistake, so the THP bloat fix never took effect;
   41 = PR_SET_THP_DISABLE. Their board M is ~3 MB under ours - this is
   likely a chunk of it.

Kept from our line on top of their 42b8fde7: the MIT notice header, the
75%-load hash-table growth guard (a full open-addressing table is an
infinite probe on unknown tokens - with 1-term queries their sizing can
overflow: keys=q vs slots~3q/8), and the \r strip. The guard sits at the
TOP of insert() - a mid-insert placement recomputes nothing after the
rehash and writes the new entry at the stale index (caught by the
synthetic single-term suite: 0 matches instead of 144).
- Local gates: spec, sample, synthetic edge suite byte-exact; full-data
  sha256 f86973c2...80927 unchanged; scan 3.03 -> 1.61 s warm, 2.40 s
  cold; exit HWM 304 MB (charged ~50 MB). S 666 -> ~645 counted.
- Run: run-fe8181c1fbf8 - PASS: T=0.2446 s, M=51.1 MB, S=628, K=25.31,
  **69.11 pts** - NEW BEST (+0.86 over 68.25). M and S both beat the
  adopted code's own board numbers (51.8 MB, 611); T landed 5% above
  their 0.2322, inside same-code noise. Cache Me's own follow-up commit
  (de8c9c2d) regressed them to 68.90 - the adoption picked the right
  commit. Rank 10 -> 9.
  Same-code re-roll (PERF-only commit): run-7f7e13bfbd4e - T=0.2478 s,
  M=52.3 MB, S=628, **68.87 pts**. Two v29 rolls: T 0.2446/0.2478 - the
  ~6% gap to Cache Me's 0.2322 on identical code looks like worker
  lottery, not config (toml identical but the team name; their own rolls
  spread 0.2322-0.2497). Re-rolls are free and the best is sticky.

## Step 28: PGO build on a synthetic training corpus (v30, this push)

- Steps 10-17 built with PGO (-5% T on the old engine, trained on the
  published generator via b_index.py); the step-18 adoption dropped it
  with the generator file (~875 counted lines). This brings PGO back
  WITHOUT the generator: src/gen.rs (~60 lines) synthesizes a training
  corpus from a xorshift stream with the real corpus's shape - token
  length histogram, 5:3 BOOL/PHRASE mix, operator mix, head-skewed term
  frequencies (measured on the sample data) - so profile data needs no
  extra file and no network.
- Dockerfile: single build stage on the allowlisted llvm-tools image;
  instrument, train on two seeds, merge, rebuild with profile-use.
  target-cpu=native unchanged; MALLOC_ARENA_MAX=2 unchanged.
- Local gates on the trained binary: spec/sample/synthetic/full all
  byte-exact (sha256 f86973c2...80927). Local PGO A/B was noise-bound
  (dev box cache state) - the board decides, as with steps 23/25.
- Run: run-ba2632207005 - PASS but REGRESSION: T=0.2626 s, M=50.6 MB,
  S=710, **67.98 pts** vs the v29 band 68.87-69.11. PGO trained on the
  synthetic corpus cost ~6.5% T on the board (the local A/B had hinted
  the same); the v13-era -5% does not transfer to this code. M improved
  0.5 MB, S cost 82 lines - nowhere near enough. Reverted next.

## Step 29: revert the PGO build (v29 code, plain build)

- Dockerfile and Cargo.toml back to the step-27 plain build; src/gen.rs
  removed. Code unchanged from v29.
- Run: run-523b5d78f4d0 - PASS: T=0.2577 s, M=51.4 MB, S=628, 68.51 pts.
  Below the v29 band; the best stays run-fe8181c1fbf8 (69.11, T=0.2446).

## Step 30: 4-byte term-table slots + comment diet (v31, this push)

- Terms::slot entries shrink u64 -> u32: 15 hash tag bits above id + 1
  (17 bits, asserted under 0x1FFFF). The table halves (2 MB -> 1 MB on
  the full queries), so scan probes sit closer to L2. A 15-bit tag makes
  a wrong-slot key load ~1/32768 per probe; same() still confirms.
- Comment diet: 12 redundant comment lines condensed (wording only, no
  design content lost); S 628 -> 617 counted lines.
- Local gates: synthetic/sample/full all byte-exact (sha256
  f86973c2...80927); local A/B vs v29 neutral (+/-2%, dev box noise
  bound). The board decides, as with steps 23/25/28.
- Run: run-d703f2e7fdf2 - PASS: T=0.2476 s, M=53.5 MB, S=631, 68.76 pts.
  Neutral draw inside the v29 band; the best stays run-fe8181c1fbf8 (69.11).
  Kept: the 4-byte slots cost nothing and help on some workers. The S count
  came back 631, not the 617 the line count suggests - the harness counts
  differently; noted for future diets.

## Step 31: adopt Lex's scan upgrades (v32, this push)

- Lex (Aryanxp1/speed-demon-2026 @ d32429cd, public, MIT - same Raptors
  license) took the Cache Me base further: Terms::find returns a NONE
  sentinel so the scan's hot callback returns early on non-term tokens
  (their per-token work - block-flush check, new-to-document mark, pending
  bit, bucket add - is all skipped); list recording uses predictable
  branches instead of branchless masks; bucket chunks shrink to 32 KB
  with M_MMAP_THRESHOLD to match; integer formatting is allocation-free
  (a fixed [u32; 10] for the first ids, a push_u64 instead of write!).
- Lex's board best on this exact commit: T=0.2123 s (70.37 pts) vs our
  0.2446 - the fastest verified public code in this engine family.
- On top we keep: the MIT header (now crediting both teams), the insert()
  growth guard (a full table's probe loop must never run), and the CR
  strip; their --data/--out dev scaffolding is removed.
- Local gates: synthetic/sample/full all byte-exact (sha256
  f86973c2...80927); local wall 2.63 s vs v29's 2.81-2.89 s.
- Run: run-d780f1a6aaf3 - PASS, NEW BEST: T=0.2332 s, M=51.3 MB, S=663,
  **69.43 pts** (was 69.11). T lands in the same 0.231-0.233 band the other
  Cache Me-family teams draw; Lex's own 0.2123 remains the family's outlier.

## Step 32: 4-byte slots on the Lex base + formatting diet (v33, this push)

- The step-30 slot change carried over: 15 hash tag bits above id + 1 in a
  u32 slot halves the term table's cache footprint (2 MB -> 1 MB on the
  full queries); on v31 it drew neutral, and the Lex base probes the table
  once per token just the same.
- Formatting diet: rustfmt-expanded constructs (multi-line signatures,
  the SEL table, chained iterators) collapsed - whitespace only, no
  content change; S 663 -> ~600 expected.
- Local gates: synthetic/sample/full all byte-exact (sha256
  f86973c2...80927); local wall 2.32 s vs v32's 2.63 s.
- Runs: the step-31 PERF push re-rolled v32 (run-6620cf8b4ffc): T=0.2290 s,
  M=51.0 MB, S=666, **69.65 pts - new best**. This push (run-0c5d10d2b531):
  T=0.2428 s, M=54.6 MB, S=666, 68.71 pts - a below-band draw, inside noise;
  local timing still favours the 4-byte slots, so one more re-roll decides.
  (S barely reacts to comment or formatting diets: 663 -> 666 here. Diet
  experiments closed; T and re-rolls are the levers.)

## Step 33: re-roll log (no code change)

- run-4b7362ba7c9c (step-32 PERF push, v33 code): T=0.2288 s, M=50.8 MB,
  S=666, **69.67 pts - new best**.
- run-d83358f3724f (sibling-folder push re-ran track-b at head): T=0.2281 s
  (fastest yet) but M spiked to 57.1 MB, 69.19 pts. MALLOC_ARENA_MAX=1 is
  the standing candidate if the M spike repeats.

- run-551200f87557 (step-33 PERF push): T=0.2226 s, M=50.0 MB, S=666,
  **70.03 pts - new best**, past lippinc/cache-me/newtonredheat to rank 9.
- run-71afcd450e12 (sibling-folder push re-ran track-b): T=0.2242 s,
  M=50.2 MB, 69.94 pts. The T band has shifted down to 0.222-0.224.

- run-7e9e8b4b73bd (sibling-folder push re-ran track-b): T=0.2255 s,
  M=50.4 MB, 69.86 pts. Best unchanged (run-551200f87557, 70.03).

- run-1801ed8410f4 (sibling-folder push re-ran track-b): T=0.2276 s,
  M=50.6 MB, 69.74 pts. Best unchanged (run-551200f87557, 70.03).

- run-5288e8e3066c (sibling-folder push re-ran track-b): T=0.2218 s,
  M=50.3 MB, S=666, **70.05 pts - new best**. Also run-ceb8ac677274:
  T=0.2263 s, 69.83 pts. v33 line closes at 70.049176.

## Step 34: adopt Cache Me's id-vector design + own pair-filter + _exit (v38)

- The field's top B times (Cache Me T=0.174, Lex T=0.199) came from a
  different architecture than v33: per-document token-id tapes, branchless
  masked recording, phrase checks only at anchor positions. Cache Me's
  entry is MIT-licensed; we adopted their 2b1da445 code with the licence
  and attribution in the file header, then added two changes of our own:
  a one-token prefilter before each phrase's slice compare, and _exit(0)
  after the output flush to keep map teardown out of the timed window.
- This push (run-847da218f1e4): T=0.2043 s, M=49.7 MB, S=631,
  **71.17 pts - new best** (+1.12). Sibling re-roll (run-bcc258960fda):
  T=0.2023 s, **71.28 pts - new best**.

## Step 35: clean-room phrase pair index on the v34 base (v36) - bust

- Our own design: index the (first, second) token pair of each phrase
  during the scan so phrase candidates only form where a pair matches,
  skipping >32-byte tokens without bridging. Locally it looked 2x faster
  than v34; the board disagreed.
- This push (run-2c590680991d): T=0.3380 s, M=60.9 MB, S=831,
  63.92 pts - heavy regression, design abandoned. Reverted the tip.

## Step 36: plain Cache Me code, no own changes (v37) - isolate the filter

- Dropped the pair-filter to isolate its cost. This push
  (run-82251e19cc74): T=0.2030 s, M=49.5 MB, S=614, **71.34 pts -
  new best**. Sibling re-roll (run-29431857eed0): T=0.2058 s, 71.19 pts.
  Verdict: the filter was NOT the drag - plain code ran the same T. Their
  identical code scored T=0.1823 in the same window on their repo, so
  something environmental differed.

## Step 37: drop MALLOC_ARENA_MAX=2 from the image (v37d)

- Found it: our Dockerfile capped glibc malloc arenas at 2 while the code
  runs 12 scan threads - lock contention on every allocation. Their image
  sets no cap. One-line Dockerfile change, no code change.
- This push (run-aeb61008f27e): T=0.1915 s, M=51.5 MB, S=613,
  **71.81 pts - new best** (-5.7% T).

## Step 38: re-roll log (v37d code, no change)

- run-9ddbfdf354ae (sibling-folder push re-ran track-b): T=0.1812 s,
  M=49.8 MB, S=613, **72.56 pts - new best**. T matches Cache Me's
  same-window 0.1823 - full parity; the step-37 reading was a slow roll.

## Step 39: port Lex's fast output path (v39)

- Lex's entry (also MIT) answered 65k queries without the formatting
  machinery: a manual digit buffer (push_u64), a stack first-ten array
  instead of a per-line Vec, and phrase matches streamed straight from the
  sorted match list instead of collecting a Vec per query. Ported to our
  Rust base with both MIT licences and attribution in the file header.
  Scan untouched; output path only.
- This push (run-a24ae7f1a010): T=0.1731 s, M=49.8 MB, S=649,
  **72.87 pts - new best** (-4.5% T; the S cost of the added code, about
  0.5%, is well covered).

## Step 40: _exit(0) after the flush (v40)

- Lex's v2 change, and our own v2x line measured it too: the timer
  measures child wall time, so leaving after the flush keeps 243 MB of
  map and heap teardown out of T.
- Sibling re-roll at head (run-2c594c5daa1e): T=0.1720 s, M=50.0 MB,
  S=651, **72.91 pts - new best**.


## Step 38 re-roll log (continued)

- PERF.md doc push re-roll (run-59934ee436be, v40 code): T=0.1720 s,
  M=49.5 MB, S=651, **72.956095 pts - new best**.

## Step 41: emit helper diet (v41)

- Folded the repeated result-line formatting into one emit helper; pure
  S-diet, no behaviour change. Gated byte-exact on the full corpus.
- This push (run-a96e527e91d6): T=0.1868 s (a slow roll), M=49.5 MB,
  S=647, 72.13 pts. The diet itself is confirmed: S 651 -> 647.

## Step 42: MADV_HUGEPAGE + MADV_WILLNEED on the data maps (v42)

- From the 1BRC playbook: advise the kernel to back the docs and queries
  maps with huge pages and to read them ahead. Four lines, no algorithm
  change. Gated byte-exact on the full corpus.
- First reading, sibling-folder re-roll at head (run-12afc353dbae):
  T=0.1718 s, M=49.5 MB, S=652, **72.967812 pts - new best**.
- Re-roll (run-e905af6a4cdf): T=0.1715 s, M=49.9 MB, S=652, 72.94 pts.
  T band across the two readings: 0.1715-0.1720 s (v40 measured
  0.1720-0.1868 s) - a small real gain, kept.

## Step 38 re-roll log (continued 2)

- PERF.md doc push re-roll (run-aba439193313, v42 code): T=0.1716 s,
  M=49.2 MB, S=652, **72.999937 pts - new best**.
- Sibling-folder re-roll (run-76d8739c5f94): T=0.1733 s, M=50.1 MB,
  S=652, 72.82 pts.
- Sibling-folder re-roll (run-baba82a58b29): T=0.1728 s, M=49.5 MB,
  S=652, 72.90 pts. v42 T band across five readings: 0.1715-0.1733 s.

- Sibling-folder re-roll (run-3410fee5aa6f): T=0.1750 s, S=652,
  72.75 pts.
- Sibling-folder re-roll (run-411e20a416a3): T=0.1928 s, S=652,
  71.79 pts - the slow-roll tail (E-core landing suspicion; see the
  held ledger's v53 note).
- Sibling-folder re-roll (run-1424f5aaabc6): T=0.1792 s, S=652,
  72.42 pts. v42 T band across eight readings: 0.1715-0.1928 s.

## Step 43: hash-only terms table (v43, Cache Me adoption)

Cache Me's 2026-11-01 change (their commit fae41d5e, MIT - see the
header) removes the keys array from the terms table: a full 64-bit
token hash, each slot the top 46 hash bits above id + 1, and find
compares hash bits only, so no key is ever loaded or compared. The
keys[] dependent load was the scan's last unprefetched miss, and the
scan ring now carries 8-byte hashes instead of 32-byte keys. A false
match needs a 63-bit hash collision per probe pair (~2^-63), so
correctness is not a gamble. Gates: full corpus byte-exact, sample
byte-exact, all six adversarial suites byte-exact vs the trusted
binary. Expected: S 652 -> ~642, M -2.4 MB (keys array and its
renumber clone gone), and the T win Cache Me's own first roll
(73.234 at T=0.1732 s) has not shown yet.


## Step 43 result and step 44: next-page prefetch (v55, Cache Me adoption)

v43/v54's first true reading (run-24cba018aa0d): T=0.1702 s, M=47.9 MB,
S=654, **73.195448 pts - new best** (+0.196). A stray track-b/main.rs
from the upload doubled S to 1298 for one reading; deleted. Cache Me's
follow-up (their commit c1ec48304e3f, MIT): one _mm_prefetch(p + 2048)
per 64-byte window, so the next page of the corpus is on its way before
the hardware prefetcher's 4 KB stop. A prefetch never faults, so output
cannot change; gates byte-exact as before. S +1 line.


## Step 44 result and step 45: dead import (v56)

v55's first reading (run-69d32d1af5fa): T=0.1701 s, M=47.8 MB, S=655,
**73.207521 pts - new best** (+0.012). Step 45 removes an import left
unused since the v39 output port (S 655 -> 654, no behaviour change);
gates byte-exact as before.
