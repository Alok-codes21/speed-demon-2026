# Track D candidate: HighVelocity C++ engine (staged, not pushed)

Provenance: verbatim copy of track-d from the MIT-licensed public repo
insightlabs38-pixel/sd-sm, commit 650272f4043d367210da65af1ca0d8f07f640f5f
(team HighVelocity, their PR #15). Copyright (c) 2026 Hackathon Raptors CIC
and contributors. Only an attribution header was added to d_traverse.cpp and
team set to Cosmic in .speed-demon.toml. MIT license terms apply; their
LICENSE is reproduced at the bottom of this file.

Why it is staged: their official run-9757529e8b52 passed all five gates:
T=0.108321 s, M=60,317,696 B, S=592, points=73.831705. Our banked D best is
71.704 (T 0.1134 s, M ~71.2 MB, S 783). Under the real formula this
candidate is +2.1 D points and beats every known D score on the board
(Lex 73.62, AVENGERS 72.24). D still sits below our B (~75.69), so it does
not change the overall geomean; the value is D ranking and the JUDGE
component. Design: SIMD parse into 6-byte bins, fused degree-2 contraction +
BFS locality ordering, 256-vertex probe then bidirectional Dijkstra on a
ring-128 bucket queue, 32-bit paged labels with 64-bit fallback.

Verification done locally (1-core box, seed-1 datasets):
- Byte-exact output on the full dataset: sha256 a23dbdf0a3e49cc8...
  (same as our current engine).
- Byte-exact on tiny (40 queries) and sample (200 queries), matching both
  our current engine and an independent Python Dijkstra reference.
- Head-to-head on the 1-core box, 8 interleaved runs vs our engine:
  median 0.396 s vs 0.412 s (min 0.360 vs 0.392), peak VmHWM 142.0 vs
  147.7 MB - faster and leaner, consistent with their board numbers.
- Exactness claims in their README cover zero weights, parallel and
  duplicate edges, self-loops, isolated and unreachable vertices, and
  64-bit sums; their PERF.md documents the full gate history.

Caveats: their code shares lineage with our current engine family (same
reference ancestry), which keeps the review surface small, but a swap still
replaces Anushka's track-d lineage and needs Alok's explicit go. Board T
swings roughly 5-10% run to run across teams; their 73.83 is an official
gated run, not a best-of-many sample claim.

License (from their repo):

MIT License

Copyright (c) 2026 Hackathon Raptors CIC
Copyright (c) 2026 the contributors of this repository

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
