# Cache Me Track A alternative, held only

Source: https://github.com/manusingh090/speed-demon-2026-entry/commit/20e2ecc3682a096de0e5911e55d3609d86ce0a31 . MIT license retained in cache-me-candidate.cpp. Prior HighVelocity credit retained too.

This is a research alternative, not the selected implementation. The held branch's older Dockerfile is deliberately untouched and does not build this candidate. Do not promote this branch wholesale. A standalone candidate build uses the selected live two-stage gcc15 Dockerfile, renaming this file to main.cpp. Expected candidate-only S is328 (321 C++ plus7 Dockerfile), not the sum of all research files on held.

Changes from upstream: SD_DATA_DIR/SD_OUT_DIR/SD_THREADS overrides for testing, and exact bounded memcmp for key equality. The upstream full-vector comparison can read beyond a published short key into the next arena region while another worker fills it. Bounded equality avoids that overlap; no claim of a full sanitizer audit is made.

25/25 byte-exact: tiny/sample/full/shuffled/reversed generated inputs, each at1/2/4/7/12 workers. Full-seed1 expected SHA256495ca09682b9673ef836a506496177680bb4a3af54255174befddb1f83481cd3.

21 interleaved paired full-data runs on the local one-CPU host, milliseconds:

| Workers | Selected THP+dedup | Candidate | Ratio |
| --- | --- | --- | --- |
|1|46.239|77.195|1.6695|
|12|50.962|79.690|1.5637|

Canonical clang-format23.1.3 plus scc4.1.0: candidate321 code lines versus selected504; Dockerfile7 in both. S saving predicts about+1.44points at equal T/M. Local T penalty predicts about-4.85 to-5.57points, before M. These are ratio estimates, NOT official scores. Official T/M/net points are unmeasured because held never scores. No verified net gain; no live trial recommended from these results. A one-CPU local timing cannot establish the organizer's twelve-thread result.

The engines differ: candidate parses all input first, ranks4096 slot IDs, then computes and maps output in later phases. Selected engine streams finalization by hour and recycles buffers. Slot IDs cannot be pasted into the current parser without redesigning its dense2048-ID record/group representation.
