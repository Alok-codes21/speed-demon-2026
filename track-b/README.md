# Track B: Index

Answer 50,000 boolean and phrase queries over a 254 MB corpus (250k documents),
one wall-clock time for read + index + answer. Output: `/out/results.txt`.

## Approach

High-performance query-driven index in Rust, zero external dependencies ($D = 1$).

1. **Selective Query Vocabulary**:
   Parse `queries.txt` first into an open-addressing hash table with SIMD 32-byte key vectors.
   Only terms that appear in queries are tracked; irrelevant vocabulary is bypassed.

2. **In-Scan Phrase Verification**:
   Every phrase is verified during the single corpus scan at every occurrence of its anchor (rarest term),
   while the document text is already hot in L1 cache. Query-time phrase evaluation is instant ($O(\log N)$
   partition point lookups, 0 ms verification, 0 bytes positional index).

3. **Dense Bitmaps for Common Terms ($df > N/32$)**:
   Terms appearing in > 1/32 of documents (accounting for ~66% of all postings) are stored as 64-bit bitmaps.
   Boolean operations run via single-instruction bitwise logic (`AND`, `OR`, `ANDNOT`) with popcounts.

4. **16-Bit Gap Postings**:
   Doc IDs within each thread's range (< 32,768 docs) are packed into 16-bit units with delta gap encoding,
   cutting list memory by 50%.

5. **SIMD AVX2 Tokenizer**:
   64-byte AVX2 vector classification with `_mm256_movemask_epi8`, branchless lowercase (`| 0x20`), and 8-deep prefetching.

6. **Memory Optimizations**:
   Disables Transparent Huge Pages (`PR_SET_THP_DISABLE`) to avoid 2MB page allocation rounding on Hetzner.

7. **Seed-Proof Exactness**:
   Short-token hashes are bijective AES pairs and every accepted token is confirmed against the
   term's bytes (letter case aside), so no corpus seed can turn a hash collision into a wrong answer.

8. **SIMD Left-Pack Scan**:
   The scan stores one action per token, then packs heavy and list hits eight tokens at a time
   (`_mm256_permutevar8x32` with a 256-entry table), over 1 MiB scan claims.

## Dependencies

None ($D = 1$). Standard Rust 2021 toolchain only.

