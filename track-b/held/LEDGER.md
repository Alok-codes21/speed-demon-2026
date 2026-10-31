# Track B held-candidate ledger (speed-demon-held branch)

Updated: 2026-11-01 01:10 IST. Banked best on speed-demon: **70.0297 pts**
(run-551200f87557, v33 code = Lex v1 + our 4-byte slots + diet,
T=0.222647 s, M=50,044,928 B, S=666).

Scoring (calibrated, reproduces the banked 70.03 exactly):
pts = 50*log10(K); K = (56.35/T)^0.5 * min(2199961600/M, 50)^0.2 * min(91/S, 1)^0.15.

Overall standing: geomean of top-3 track bests. Cosmic: A=79.93, C=77.88,
B=70.03 -> 75.82, **rank 9/73**. B is the weak leg.
To pass: rank 8 unity (76.19) needs B ~= 71.2; rank 7 cache-me (76.30)
B ~= 71.6; rank 6 guseinov (76.52) B ~= 72.3.

## Candidate v34 - Lex v2 rebase (this branch's track-b/src/main.rs)
- Source: Aryanxp1/speed-demon-2026 @ 0c18cdf8 (MIT). Lex's newest:
  zero-allocation phrase answers, _exit(0) teardown skip, bucket vector
  restructure. Plus our deltas: 4-byte slots (15-bit tag), insert growth
  guard, CR strip, formatting diet, MIT header (Cache Me + Lex).
- Gates: synthetic/sample/full all byte-exact (full sha256 f86973c2..80927).
  Local wall = v33 within noise (2.63-2.90 s vs 2.75-2.85 s).
- Fire: replace track-b/src/main.rs on speed-demon; Cargo.toml/Dockerfile
  untouched. Est S ~690 (was 666, -0.1 pts).
- Expected (M ~50.5 MB): T=0.2226 -> 69.85 (-0.18); T=0.215 -> 70.24
  (+0.21); T=0.210 -> 70.55 (+0.52). Net positive only if the eval-phase
  opts beat Lex v1 by >1.5% T. VALIDATION SIGNAL: lex's own board run of
  0c18cdf8 (pushed 19:23 UTC, scoring pending) - if it beats 70.37, fire.
- A losing run cannot lower the banked best (board best is sticky).

## Candidate ARENA1 - MALLOC_ARENA_MAX=1 (held/Dockerfile.arena1)
- One-line env change on v33 code; no code change, gates unaffected.
- Expected: M -0 to -3 MB -> +0.0 to +0.25 pts, T neutral. Also a re-roll.
- Fire any time as a single Dockerfile swap.

## Candidate v35 - Dannyamah pair-index engine (held/v35/)
- Source: Dannyamah/speed-demon-2026 @ 36fef8ee (MIT; repo later went
  private - local copy preserved). Different engine: index query terms +
  adjacent phrase pairs, counting sort into postings, long phrases
  confirmed by re-tokenizing one document.
- Gates: synthetic/sample/full all byte-exact (full sha256 f86973c2..80927).
- Fire: held/v35/{main.rs,sets.rs,terms.rs} -> track-b/src/; Cargo.toml and
  Dockerfile unchanged (binary name "index" matches).
- Board T unverified. Scenarios (S ~545, M ~45-50 MB): T=0.193 (their
  team's scored family T) -> ~73.0 (+3.0, rank 6); T=0.2226 -> ~70.5-71.2
  (+0.5-1.2, from M and S); T=0.26 -> ~68.8 (best safe). Highest variance,
  highest ceiling. Explicit go only.

## Watch list
- Lex's board run of 0c18cdf8: validates v34's T direction.
- Teams ahead (CoD 77.2, kirk-mates 74.9, all-star 73.3, nicktrapp 73.2,
  unity 72.2, saalar 71.8, another-ello 71.5, guseinov 70.6): repos
  private, methods unknown. saalar's public mirror (Dannyamah) was the one
  window and is captured as v35.
- M spike watch: one run hit M=57.1 MB; if it repeats, ARENA1 gains priority.

## Harness facts (organizer's public harness, read 2026-11-01)
- T = mean of the middle 3 of 5 container runs (min/max dropped; batch
  re-run up to 2x if spread > max(5%, 50 ms)). Batch load state is the
  "worker lottery"; quiet hours score faster.
- M = MAX of memory.peak (cgroup v2) across the 5 runs - one spike counts.
  Peak-variance reduction (ARENA1) is worth real points.
- S = scc code lines AFTER rustfmt re-normalizes the source. Formatting
  diets are worthless (confirmed on board: 663 -> 666); only deleting
  statements lowers S. Comments and blanks are FREE - write them for the
  judges (35% weight on track_total).
- Final scoring: post-freeze, private holdout seed, dedicated box
  (no-turbo, performance governor). Output hash must match on unseen data.
- sd-timer measures the child process wall inside the container: _exit(0)
  skips teardown and saves real T (in v34 via Lex v2).

## Update 01:11 IST (freeze period, no push)
- Lex v2 board validation landed: their 0c18cdf8 run-740c6f3ecc05 = T 0.2200, 69.88 pts, BELOW their v1 best 70.37. v34 (Lex v2 adoption) downgraded: coin-flip vs v33, not a validated step up. Fire only if a later lex v2 re-roll beats 70.37.
- v35 (Dannyamah pair-index): licence unverifiable (no header in preserved files, repo now private) - DEAD as copied code under the licence rule. Only a clean-room rewrite of the idea is pushable. Parked.
- Rules re-read complete: adoption/MIT/PERF/AI/deps/timing all clean; report sent to parent.

## Candidate v36 - clean-room phrase pair index (built 01:50 IST, gates PASS)
- Idea level from the saalar/Dannyamah design (their code NOT copied - licence
  unverifiable, repo private): index every adjacent term pair of every phrase;
  two-term phrases answer from one list, longer ones intersect pair lists and
  confirm candidates against the document text. Implementation is fresh code
  on our v34 base (Lex v2, MIT): pair registry + per-term sorted pair LUT,
  scan records pair postings in the id range [nkeep, npost), SWAR line-offset
  table for candidate confirmation. ~+130 code lines.
- Gates: sample byte-exact; full sha f86973c2...80927 exact; differential vs
  v34 on 14 synthetic/edge suites (tiny, sx4-sx1152, s_one/s_two, t1/t2,
  gendata/2, pgotest) all identical.
- Local wall (1-core sandbox, indicative only): 1.17 s vs v34's 2.63-2.90 s =
  2.3-2.5x. The anchor around() checks were the dominant scan cost.
- Expected board (calibrated to banked 70.049 at T=0.2226, M=50.0M, S=666;
  v36 estimates M~55M, S~770):
  T=0.21 -> 69.81 | T=0.20 -> 70.31 | T=0.18 -> 71.49 | T=0.16 -> 72.77
- Overall rank (geomean top-3, A=79.93 C=77.88): B=70.3 -> 75.92 (~rank 10);
  B=71.5 -> 76.35 (~rank 8); B=72.8 -> 76.81 (rank 7-8). Now 75.84, rank 11.
- Risk: board-only unknowns are M (pair postings volume) and the S penalty
  (~-0.55 pts already in the estimates above). Fire when Alok says go.
