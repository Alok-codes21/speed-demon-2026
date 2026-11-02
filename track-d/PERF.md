# Track D candidate: HighVelocity C++ engine, PR #23 champion (staged, not pushed)

Provenance: verbatim copy of track-d from the MIT-licensed public repo
insightlabs38-pixel/sd-sm, their PR #23 champion: scored commit
a5b6172a946ed30ffc912ea7e2c09474851e6768 (source blob
e11b09e30d2ac0fdfada01317b5dce3deae5f6dd), content identical at restore
commit 8fc0d908aeadb934648e44e70f1e0dcd1b35173f. Copyright (c) 2026
Hackathon Raptors CIC and contributors. Only an attribution header was
added to d_traverse.cpp and team set to Cosmic in .speed-demon.toml. MIT
license terms apply; their LICENSE is reproduced at the bottom of this file.
(Dockerfile unchanged from the PR #15 staging; .speed-demon.toml set to
team Cosmic / language C++.)

Why it is staged: official run-7cbf51892a7c passed all five gates:
T=0.088389 s, M=60,166,144 B, S=647, D=1, K=32.750840, points=75.761122.
Our banked D best is 71.704016 (T 0.1134 s, M ~71.2 MB, S 783), so this is
about +4.06 D points. It also leads every known D on the board (Lex's
latest 75.02 as of 22:47Z). D enters our overall geomean only if it beats
B's freeze tip (~76-78); 75.76 is close but still short, so expect D
ranking + JUDGE value, not overall points, unless B slips.

Verification done locally (1-core box, seed-1 datasets):
- Byte-exact output on the full dataset: sha256 a23dbdf0a3e49cc8...
  (same as our current engine), and on tiny (40 queries) and sample
  (200 queries) against both our current engine and an independent Python
  Dijkstra reference.
- 8 interleaved runs vs our engine and their earlier PR #15 engine:
  median 0.322 s vs 0.374 s (PR15) vs 0.398 s (ours); min 0.307 s.
  ~19% faster than our engine and ~14% faster than PR15 locally,
  consistent with their board T of 0.0884 s vs 0.1083 s (PR15).
  Peak VmHWM 143.6 MB vs 147.7 MB (ours).
- Their PERF.md records the full gate history and names this commit the
  champion with PR #19 kept as rollback.

Caveats: a swap replaces Anushka's track-d lineage and needs Alok's
explicit go. Board T swings roughly 5-10% run to run across teams. This
candidate's S (647) and M (60.2 MB) are both worse-than-best-in-class by
a hair (Lex: 60.8 MB / S 634), so hybrid polish on top is possible but
worth well under half a point.

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

## Update (2 Nov 2026, ~8:20 AM IST): PR #24 race fix - NOW THE LIVE SOURCE

HighVelocity commit 4e018992215560b90dcfd6d0c057b867836ef20c ("Fix Track
D lost concurrent claims") disclosed a real race in the PR #23 source we
first staged: claim() did a relaxed load followed by an ordinary atomic
store, so two threads could both claim the same vertex; the duplicate
appends could overrun construction buffers (their forced-stale-read and
simultaneous-high-degree repros SIGSEGV on the old source; fixed source
passes, plus their sanitizer and 300+ fixture suite). A crash or corrupt
build on the hidden-holdout re-score would zero the track, so the fix is
mandatory even though every gated board run of the unsafe source passed.

The fix replaces load+store with a relaxed atomic fetch_or and appends
only when the prior word lacked the bit - claims become globally unique.
Verified by us before taking it:

- The fetched source matches their published safe-source sha256
  (d084d2b3574341...) exactly.
- Byte-exact output on all three local datasets (full sha a23dbdf0a3...,
  tiny, sample), identical to the unsafe version's output.
- 12 interleaved local runs vs the unsafe build: median 0.3085 s vs
  0.3073 s - the fix is free within noise.
- Diff review: the only behavioral change is claim uniqueness; the
  comment updates match the code.

Their first safe-source board run: 75.296175 (T=0.092453 s, M=59.8 MB,
S=647, run-646f14ac127b) vs their unsafe-source runs K=31.54-32.87
(points 74.0-75.84) - the difference is inside their own run-to-run
noise on unchanged code.

d_traverse.cpp here is the PR #24 fixed source (attribution is kept in
THIRD_PARTY_NOTICES.md at the repo root, not in the file); everything else unchanged from the PR #23 staging.

## Worker-cap experiment, reverted (Nov 2)

A change to this folder capped query workers at 6 and read the cgroup CPU quota (source sha256 165431bebfbc, S=673). Board draws for it: 74.27 and 72.34 points (T=0.100 s and 0.121 s) against 73.5 to 75.6 for the PR #24 source on the same board, and HighVelocity's own public measurement found 12 workers at least as good as 8. With S up by 26 lines and no gain in the draws, the PR #24 source is restored. It is now exactly HighVelocity's published file (sha256 d084d2b3574341...), without a leading comment header; the compiled binary is byte-identical to the earlier credited copy (sha256 d2fdc5baac31). Output was byte-exact either way; this was a speed decision only.


## Qualified upstream solver adopted — 2 Nov 2026

The Track D source now uses the exact MIT-licensed HighVelocity solver blob
`02e0764ef534a8a6694006b12911776e0a9f2860` from
https://github.com/insightlabs38-pixel/sd-sm. Its upstream performance log
records an official run `run-3cf937618e77` with K=33.672416,
T=0.083599 s, M=57,802,752 B, S=683, D=1; all gates passed. This is an
upstream result, not yet a score for this repository's submission.

The adopted candidate combines the frontier/SSE2 parsing and traversal
improvements with compact degree/edge storage and 256-entry label pages.
The existing Track D Dockerfile is intentionally unchanged; the source uses
function-level SSSE3 targeting, so the baseline compiler target is retained.
The relaxed atomic `fetch_or` claim fix and exact wide-distance fallback are
present in the adopted source. Third-party MIT attribution is recorded in
`/THIRD_PARTY_NOTICES.md`.

Do not treat the upstream K as a local result until the official board scores
this repository's new commit. Rollback source remains available at the
preceding commit if this candidate fails any local repository gate.
