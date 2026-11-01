# Track C held candidates ledger (speed-demon-held)

Staged, gated, ready to promote to speed-demon on Alok's word. Only this
folder's files differ from the staging point; S/M/T below from the board.

## CM-PORT (staged 2026-11-01 ~05:21 UTC)
- What: Cache Me's public track-c entry (github.com/manusingh090/speed-demon-2026-entry),
  main.cpp copied verbatim + an argv data/out shim (defaults /data, /out),
  attribution header in the file. Licence: MIT (repo-root LICENSE, verified
  2026-11-01). Dockerfile mirrors theirs (gcc:16 pinned, plain
  -O3 -march=native -static -pthread).
- Gates (local, our fixtures): full seed-1 sha256 d8088b1c...a0f536
  byte-exact; tiny 1/2/7 + sample 1/2/7 byte-identical to the reference;
  stress x3 one hash; ASAN+UBSAN clean; TSAN clean.
- Expected gain vs live v159 config, today's box: +1.5 to +2 pts.
  Evidence: Cache Me's own board run today run-babda5443fbb scored 77.955
  (T 0.033409, M 8.60MB, S 398) on the same drifted box where our v157 runs
  scored 76.459/75.672 (T ~0.0406-0.0408) and a v135 re-roll 77.694.
  T gain +2.15 pts ((0.0408/0.0334)^0.5), S cost -0.32 (398 vs ~325),
  M cost -0.06 (8.60 vs 8.37MB). Banked 77.91 means promotion only makes
  sense while a re-roll can land >77.91 - CM's own run today did (77.955).
- Risk: none to correctness (all gates pass); T on the box should reproduce
  CM's own runs since it is their code. Attribution is in the file header;
  MIT requires the copyright notice - add their LICENSE text to the folder
  before promotion if the checker requires it (contest rules allow copying
  with credit).
- Promote by: paste this folder's c_join.cpp + Dockerfile over speed-demon's
  track-c via the edit flow (never the multi-file upload page), then delete
  this ledger from speed-demon's track-c (S counts every file) and update
  PERF.md with the run ID.

## Not staged (audited, rejected)
- Parallel regions parse: regions.csv is only 2.18MB / 200K rows; serial
  parse ~0.3ms on the box; S cost of the machinery exceeds the T gain. Dead.
- clang build: blocked by the pinned base-image allowlist. PGO: Cache Me
  measured within noise. Privatized/sharded intern: our rdtsc data shows
  hits are 96% of intern cycles; insert-side ideas target 4%. Dead.
