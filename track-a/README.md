# Track A: Ingest

The task specification and exact output format are available in the [Track A spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

Implemented in Rust using only the standard library.

The solution reads `requests.log`, parses each request, and groups records by UTC hour and endpoint. It tracks request counts, HTTP 5xx errors, and response latencies for each group.

For each group, it sorts the latency values to calculate the p50, p95, and p99 nearest-rank percentiles. It calculates the error rate in parts per million and sorts the final groups by hour and endpoint before writing `summary.txt`.

The implementation uses hash maps for endpoint interning and aggregation, and sorts the latency values within each group to calculate the percentiles.

## Dependencies

No third-party Rust crates. Standard library only.

## Validation

Passed the official local checker against the full generated dataset (seed 1, 3.1 million requests).

- Output SHA-256: `495ca09682b9673ef836a506496177680bb4a3af54255174befddb1f83481cd3`
- Local runtime: 5.83 seconds, including container startup.

The leaderboard runtime may differ because it uses its own scoring environment.

## With another 24 hours

I would profile memory usage and allocation patterns on the full dataset, investigate ways to reduce per-request and per-group allocations, and benchmark alternative aggregation and percentile strategies while checking every change against the reference output.
