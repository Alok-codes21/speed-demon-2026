# Track B: Index

Answer 50,000 boolean and phrase queries over a 254 MB corpus (250k documents),
one wall-clock time for read + index + answer. Output: `/out/results.txt`.

## Approach

High-performance query-driven inverted index in modern C++20, zero external dependencies.

1. **Selective Query Vocabulary**:
   Parse `queries.txt` first into an open-addressing hash table ($2^{17} = 131,072$ slots).
   Only terms that appear in queries (~75k terms on full corpus) are indexed; the remaining
   ~85% of distinct corpus tokens are rejected in $O(1)$ time with no allocations.

2. **Two-Pass Contiguous Inverted Index (Zero Sort, Zero Allocations)**:
   - Chunk `docs.txt` across 12 worker threads.
   - **Pass 1**: Each thread scans its chunk and counts occurrences per `term_id`.
   - **Prefix Sum**: Compute global term offsets and exact thread write pointers into a single,
     flat pre-allocated array `global_postings` (~22 MB).
   - **Pass 2**: Threads scan in parallel and write `doc_id` directly to their pre-calculated
     slice. Because chunks are ordered strictly by increasing doc_id, postings are globally
     sorted upon insertion with zero sort passes and zero intermediate buffers.

3. **Candidate Intersect + Fast Ring-Buffer Phrase Verification**:
   No positional index overhead (~200+ MB avoided). Candidate documents are identified by
   intersecting term postings. For the surviving candidates (median 1 document), the exact
   phrase is verified directly on the mmapped document text using a stack-allocated ring buffer.

4. **Parallel Query Evaluation**:
   50,000 queries are evaluated in parallel across 12 hardware threads. Left-to-right boolean
   operations (AND, OR, NOT) use cache-aligned two-pointer merges into thread-local scratch
   buffers. Results are formatted into thread buffers and written sequentially to `/out/results.txt`.

## Dependencies

None ($D = 1$). Standard C++20 only.
