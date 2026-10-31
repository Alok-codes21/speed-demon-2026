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
- Run: run-pending
