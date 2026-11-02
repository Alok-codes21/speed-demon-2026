# Track D performance notes

Solver: `d_traverse.cpp` (C++, built with the Dockerfile in this folder). Exact directed shortest paths; see README.md for the design.

## Correctness checks (local)

- Output is exact on the full, tiny and sample datasets at 1, 4 and 12 workers (output hashes: full a23dbdf0a3e49cc8, tiny ecaca77d3221fe39, sample a4c76ed030bc7e36).
- Vertex claims use a relaxed atomic `fetch_or`, so each vertex is claimed by exactly one worker.
- Limit: edge heads use an 11-bit weight field and 21-bit vertex ids, which covers graphs up to 2,097,151 vertices. The full dataset has about 1.74 million.

## Experiments that did not help

Forward probe lengths 128 to 512, MADV_POPULATE_READ, queue shift changes, transparent huge pages, heap variants, ALT and contraction-hierarchy approaches, and capping query workers at 6 or 8. A 6-worker cap with cgroup quota reading drew 74.27 and 72.34 points on the board and was reverted.

## Board notes

Repeated board runs of identical source varied by about 20 percent in time, so a single run is not evidence of a change in speed. Best board score for this track so far: 75.84.
