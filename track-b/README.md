# Track B: Index

Answer 50,000 boolean and phrase queries over a 254 MB corpus (250k documents),
one wall-clock time for read + index + answer. Output: `/out/results.txt`.

## Approach

Query-driven inverted index. The queries are known before the corpus is read,
so only terms that actually appear in `queries.txt` get indexed - the other
~85% of distinct corpus tokens are probed in the term table and skipped.

1. **Parse queries first.** Every distinct term goes into an open-addressing
   hash table (fxhash-style u64 hash + full byte compare on hit). BOOL queries
   keep their operator list; PHRASE queries keep their term list.
2. **One pass over docs.txt, split across threads.** Byte ranges aligned to
   line boundaries; doc ids are parsed from each line, so chunks stay
   independent. The tokenizer is the spec's exact rule: fold `A-Z` to `a-z`
   only, a token is a maximal run of `[a-z0-9]`, every other byte (including
   all bytes >= 0x80) separates. Every occurrence of a query term appends a
   packed `(doc_id << 32) | position` to that term's per-thread list; merging
   per-thread lists in chunk order yields globally sorted entries with no sort.
3. **BOOL**: per-term doc lists (deduplicated on merge) combined with
   two-pointer intersect / union / difference, strictly left to right as the
   spec demands (`a OR b AND c` is `(a OR b) AND c`).
4. **PHRASE**: pick the rarest term as anchor (98% of phrases contain an
   anchor with df <= N/1000), then binary-search each remaining term's packed
   position list for `(doc, anchor_pos + offset)`. Duplicate matches inside one
   document are collapsed; output stays in ascending doc order.
5. **Output**: count, exact id sum (u64), and the ten smallest ids - the
   result lists are already sorted, so the first ten are free. One big buffer,
   one write.

## Dependencies

None. Rust standard library only (`dependency_count = 0`).

## Verification

- `sha256(results.txt)` on the seed-1 **full** public dataset matches the
  published expected hash `f86973c2…80927` byte for byte.
- Same for the seed-1 `sample` hash (`eeeec1e9…8a8f`).
- Output additionally diffed equal to the organisers' Python reference on
  freshly generated `tiny`/`sample` corpora at seeds 7 and 123 (the final
  scoring dataset uses a holdout seed, so this guards against overfitting).

The naive reference (`b_index.py`, kept byte-identical for local checking;
byte-identical copies of the published references are free in source size) is
what the Dockerfile replaces: the image builds and runs only the Rust binary.

## Local check

```
docker build --network=none -t sd-b track-b
docker run --rm -v $PWD/data/b/full:/data:ro -v $PWD/out:/out sd-b
sha256sum out/results.txt   # expect f86973c2…80927 on the public full set
```

## With another 24 hours

- SIMD (AVX2) classifier for the token scan - the byte-class table is the
  remaining hot loop.
- Radix-partitioned posting merge to cut the serial merge pass further.
- mmap the corpus instead of one big read.
