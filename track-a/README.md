# Track A: Ingest

The task specification and exact output format are available in the [Track A spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

Implemented in Rust using only the standard library (mmap/madvise/ftruncate via extern "C").

The log is mapped read-only behind one guard zero page and split into line-aligned ranges, one per worker thread. Each worker parses its range: the group hour comes straight from the fixed timestamp digits (the spec pins the 72-hour window), a single 64-byte SIMD probe locates the field separators, the endpoint is interned to a worker-local id through a small open-addressing table, and a 4-byte packed record (20-bit latency, 5xx flag, 11-bit local id) is appended to a per-hour chain of 16 KiB chunks carved from the worker's own anonymous mapping. Latencies of 2^20 us or more (about 1% of lines) overflow the packing and go to a per-hour side list.

After a barrier, one worker merges the intern tables so that an endpoint's global id is its rank in unsigned byte order, which is exactly the output order. Workers then claim hours: an hour's records from every worker are counting-sorted by endpoint rank into one latency array, and nearest-rank p50/p95/p99 come from a full sort for small groups or three nested linear selections for large ones. The exact byte size of each hour's text follows from the digit widths, so after a prefix sum the output file is sized once and mapped shared, and every worker formats its hours straight into the file with no write serialization.

## Dependencies

No third-party Rust crates. Standard library only.

## Validation

Output matches the reference byte for byte (sha256) on the tiny, sample and full seed-1 datasets, at 1, 2, 3, 5 and 12 worker threads:

- full: `495ca09682b9673ef836a506496177680bb4a3af54255174befddb1f83481cd3`

## With another 24 hours

Release parsed chunk pages back as hours complete to shave peak memory further, overlap input readahead with parsing, and trim the source line count.
