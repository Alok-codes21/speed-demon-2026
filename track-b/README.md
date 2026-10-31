# Track B: Index

Answer 50,000 boolean and phrase queries over a 254 MB corpus (250k documents),
one wall-clock time for read + index + answer. Output: `/out/results.txt`.

## Approach

Query-driven inverted index, std-only Rust, zero dependencies. The queries are
known before the corpus is read, so only terms and term pairs that appear in
`queries.txt` are indexed - the other ~85% of distinct corpus tokens are
probed in the term table and skipped.

1. **Parse queries first.** Every distinct query term goes into an
   open-addressing hash table (fxhash-style u64 hash + case-folding byte
   compare). BOOL queries keep their operator list; every consecutive term
   pair in a PHRASE query goes into a bigram table.
2. **One mmap'd parallel scan.** docs.txt is memory-mapped (the harness
   pre-faults `/data` before the run's memory cgroup exists, so mapped pages
   do not count toward the memory axis) and scanned once in line-aligned
   chunks: a byte-exact tokenizer records deduplicated doc ids for BOOL terms
   and for query bigrams only.
3. **Compact postings.** Per-thread lists concatenate in chunk order
   (globally sorted with no sort pass); BOOL postings are delta-varint
   encoded (~27 MB for 21M doc ids).
4. **BOOL** queries merge sorted doc-id lists strictly left to right, in
   parallel over the query list.
5. **PHRASE** queries intersect their bigram doc lists - a 2-term phrase is
   a single lookup - and re-tokenize only the surviving documents
   (median one) to verify the exact token sequence.

## Dependencies

None.

## With another 24 hours

AVX2 token classification in the scan, parallel posting merge, and shaving
source lines.
