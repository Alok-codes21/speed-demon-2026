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
   distinct tokens, index only terms that appear in the 50,000 queries.
   Tokenization: byte-class table, no regex, no UTF-8 decoding. Postings are
   `(doc, position)` u64s appended in scan order - no sorting anywhere.
   Result on the dev sandbox (1 vCPU, 2 GB): **4.4 s** wall, **519 MB** peak
   RSS on the full 254 MB dataset, vs the reference's 56.35 s / ~2.3 GB class
   footprint. Hash matches the published expected hash exactly.
3. **Parallel corpus scan (std::thread).** 12 line-aligned chunks, per-thread
   posting lists concatenated in chunk order (globally sorted without a sort
   pass). Parallel query evaluation on top; both phases share the read-only
   index. Dev sandbox has one core, so the parallel win shows up on the
   scoring box, not here.

## Wrong turns

- Storing positions for every corpus token (full positional index) was the
  first design; the query distribution makes it pointless - 85% of distinct
  tokens never appear in a query, so it was dropped before implementation.
- Phrase checking by re-tokenizing candidate documents (the reference's
  approach) loses to packed-position binary search once positions exist
  anyway for the anchor term.

## Verification

| dataset | check | result |
| --- | --- | --- |
| full, seed 1 (the board's data) | sha256 = f86973c2…80927 | match |
| sample, seed 1 | sha256 = eeeec1e9…8a8f | match |
| tiny/sample, seeds 7 and 123 | byte-diff vs Python reference | identical |

## Live board runs

Run IDs from speeddemon.dev/board get appended here as pushes land.
