# Track D: Traverse

This Track D implementation is adapted from the MIT-licensed Rust solution in [manusingh090/speed-demon-2026-entry](https://github.com/manusingh090/speed-demon-2026-entry), with attribution retained in `src/main.rs`. The upstream approach is used as a substantial optimization reference; its reported timings are not claimed as measurements of this repository.

## Approach

- Parse the graph once from a memory-mapped file using parallel workers.
- Build compact, packed adjacency data in node-range partitions.
- Reorder nodes into small locality-friendly BFS blobs.
- Build a reverse graph and answer queries with bidirectional Dijkstra.
- Use compact 4-ary heaps and lazily allocated distance pages.

## Dependencies

No Rust crates. Uses the Rust standard library and Linux/POSIX memory mapping interfaces.

## Validation status

The code has been integrated, but this repository's full official Docker/hash benchmark has not yet been run. Check the sample and full datasets before treating the result as verified.

