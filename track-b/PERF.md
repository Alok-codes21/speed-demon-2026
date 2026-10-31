# PERF.md - Track B optimisation log

Numbers are wall time for the whole run (read corpus + build index + answer
50,000 queries), the way the harness measures it.

Baseline: the organisers' naive Python reference (`b_index.py`).
Published measured reference on the scoring box: **56.35 s** (seed-1 full).
On an Apple M4 Pro the same reference is 27.2 s with 2.29 GB peak RSS.

## Steps

1. **Baseline reproduced locally.** Reference run on the seed-1 `sample`
   dataset to confirm the harness contract and the expected output hash.
2. **Rewrite in Rust, query-driven index.** Instead of indexing all 609,833
   distinct tokens, index only terms that appear in the 50,000 queries
   (75,747 distinct terms). Tokenization: byte-class table, no regex, no
   UTF-8 decoding. Postings appended in scan order - no sorting anywhere.
3. **Parallel corpus scan (std::thread).** Line-aligned chunks, per-thread
   posting lists concatenated in chunk order (globally sorted without a sort
   pass). Parallel query evaluation on top; both phases share the read-only
   index.
4. **Memory-driven redesign (current).** The score formula weighs peak
   memory, so the positional index was dropped entirely:
   - docs.txt is *streamed* in 2 MB blocks instead of held resident
     (saves ~254 MB of peak RSS);
   - only deduplicated doc-id lists are stored (one flat arena);
   - a doc-id -> file-offset table (2 MB) lets PHRASE queries re-tokenize
     just the candidate docs (~11 per query after doc-list intersection)
     to verify adjacency - no stored positions at all;
   - the term table is one u64 per slot (hash32 | id) plus a 4 Mbit summary
     filter, so the scan's 32 M tokens mostly never touch the table;
   - a tiny per-thread (term, doc) recency cache keeps hot vocabulary terms
     off the posting vectors.
   Result on the dev sandbox (1 vCPU, 2 GB): **5.3 s** wall, **209 MB** peak
   RSS on the full 254 MB dataset (vs 4.4 s / 519 MB for the positional
   version). Hash matches the published expected hash exactly.

## Wrong turns

- Storing positions for every corpus token (full positional index) was the
  first design; the query distribution makes it pointless - 85% of distinct
  tokens never appear in a query, so it was dropped before implementation.
- Packed-position postings for phrase terms (binary-search verification)
  evaluated faster, but the position storage cost more points under the
  memory term of the score than the eval time bought back, so the current
  version verifies phrases by re-reading candidate docs instead.

## Verification

| dataset | check | result |
| --- | --- | --- |
| full, seed 1 (the board's data) | sha256 = f86973c2…80927 | match |
| sample, seed 1 | sha256 = eeeec1e9…8a8f | match |
| tiny/sample, seeds 7 and 123 | byte-diff vs Python reference | identical |

## Live board runs

| commit | run | result |
| --- | --- | --- |
| 72d27fd (positional v1) | run-cb0c84fd5f0f | PASS all gates: T=0.882 s, M=649 MB, K=7.96, 45.05 pts (rank 44/58 at 2026-10-31 07:27 UTC snapshot) |
| streaming v2 (this push) | pending | awaiting board run |
