# Track D: Traverse

Python 3.13, standard library only. The supplied repository is used only as an optimization reference; its Rust code is not used at runtime.

- Two-pass memory-mapped graph parsing avoids a huge token list and temporary edge arrays.
- Forward and reverse CSR use 32-bit node and weight arrays.
- Bidirectional Dijkstra uses a lower-bound stopping rule.
- Duplicate-query caching, endpoint shortcuts, and touched-node resets reduce repeated work.

Official output-hash validation and leaderboard timing are still pending.