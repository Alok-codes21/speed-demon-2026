# Runtime stage: nothing but the binary. Reads /data, writes /out, exits 0.
# MALLOC_ARENA_MAX caps glibc's per-thread arenas: with 12 threads the
# default (8 x ncpu) fragments the postings build across many mmap'd heaps
# and inflates measured peak memory; 4 arenas keep contention negligible
# (allocations here are bulk, not fine-grained).
FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
ENV MALLOC_ARENA_MAX=4
COPY --from=build /src/target/release/solution /app/solution
ENTRYPOINT ["/app/solution"]