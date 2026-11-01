# Track D candidate: AVENGERS Rust engine (staged, not pushed)

Provenance: verbatim copy of track-d from the MIT-licensed public repo
Mukul07777/speed-demon-2026 (branch speed-demon), Copyright (c) 2026
Hackathon Raptors CIC and contributors. Only an attribution header was
added to src/main.rs and team/language set to Cosmic/Rust in
.speed-demon.toml. MIT license terms apply; their LICENSE is reproduced
at the bottom of this file.

Why it is staged: their board best is 72.24 (run-5c222b763b05 family:
T 119.0 ms, M 67.8 MB, S 608) vs our banked 71.704 (T 113.4 ms,
M 71.18 MB, S 783). Under the real formula that is about +0.4 to +0.5
D points. D still sits below our B (~75.69), so this does not change the
overall geomean; the value is D ranking and the JUDGE component.

Verification done locally (1-core box, seed-1 datasets):
- Byte-exact output on the full dataset: sha256 a23dbdf0a3e49cc8...
  (same as our current engine).
- Byte-exact on tiny (40 queries) and sample (200 queries) against both
  our current engine and an independent Python Dijkstra reference.
- Head-to-head timing on the 1-core box: ~7% slower than our C++ engine,
  peak RSS ~6 MB leaner - consistent with their published board numbers
  (they trade a little T for much better S and slightly better M).
- Their own PERF.md documents multi-seed testing and the full
  optimization log; see
  https://github.com/Mukul07777/speed-demon-2026/blob/speed-demon/track-d/PERF.md

Caveats: their log notes board T swings ~10% between runs and that
laptop gains often fail on the box. The 72.24 is their best of several
samples of the same code; first samples read 71.84-71.89. A language
swap (C++ -> Rust) replaces Anushka's track-d lineage and needs Alok's
explicit go.

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
