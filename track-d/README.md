# Track D: Traverse

Track D is implemented in Rust with no third-party runtime dependencies.

## Design

- Parse the memory-mapped graph once and build per-part CSR adjacency.
- Pack each edge's head and common-case weight into one 32-bit word; keep oversized weights in a side table.
- Renumber vertices using compact BFS-grown blobs to improve graph locality.
- Build a reverse CSR graph for bidirectional Dijkstra.
- Use compact 4-ary heaps and paged distance storage.
- Use scoped worker threads for graph construction and query processing.

The Dockerfile builds the Rust binary offline and runs it directly. Official output-hash and leaderboard checks must be performed after each change; no performance gain is claimed without a measured run.
