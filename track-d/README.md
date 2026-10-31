# Track D: Traverse

This submission is implemented in **Python 3.13 using only the standard library**. The supplied reference repository, [manusingh090/speed-demon-2026-entry](https://github.com/manusingh090/speed-demon-2026-entry), is used only as an optimization reference; its Rust implementation is not included or used at runtime.

## Python approach

- Build forward and reverse compressed sparse row (CSR) adjacency.
- Answer queries with bidirectional Dijkstra and a lower-bound stopping rule.
- Process queries in endpoint-sorted order to improve locality, then write answers in their original input order.
- Skip self-loops, cache duplicate query answers, and use low-degree shortcuts.
- Reset only nodes touched by each search and buffer the output.

## Dependencies

None. Python standard library only.

## Validation status

The Python implementation has been restored and updated. The official sample/full output hash checks and leaderboard benchmark still need to be run; no runtime or rank improvement is claimed until those checks are measured.
