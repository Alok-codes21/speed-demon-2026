# Track D: Traverse

Track D is implemented in C++17 with no third-party runtime dependencies.

## Design

- Memory-map the graph and parse edge records in parallel using SIMD-assisted delimiter scanning.
- Bucket edges by source vertex, then build compact adjacency rows with packed 32-bit head/weight entries and side tables for weights that do not fit.
- Contract degree-2 chains into regions to reduce the graph traversed by queries while preserving accumulated edge weights.
- Build forward and reverse compact adjacency structures for bidirectional Dijkstra.
- Use paged distance storage and a compact priority queue in each query worker.
- Sort queries by endpoints, solve duplicate endpoint pairs once, and copy the answer to every matching query.
- Use worker threads for graph construction and independent query groups.

## Build and verification

The Dockerfile compiles the C++ source offline with GCC, C++17, -O3, -flto, and the x86-64-v3 target. It runs the resulting binary directly.

Run the repository's check.py d --data <directory> --expected <sha256> command with the official Track D input files and expected output hash to verify correctness. Local timings are not the official leaderboard timings. No performance improvement or rank is claimed unless an official scored run confirms it.
