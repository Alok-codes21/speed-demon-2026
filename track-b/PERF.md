

## Step 80: v10 (left-pack hit recording from upstream) - trial

- Upstream (the MIT-licensed engine this entry builds on) moved hit recording to a left-pack scheme: the lookup loop stores one 32-bit action per token, then packs the hits eight lanes at a time (movemask + a 256-entry permutation table + popcount advance), and per-line list actions carry the line number so the document id is rebuilt at emit. Ported onto v9 unchanged in spirit; exactness is untouched (64-bit tags plus the byte confirms).
- Gates: 60/60 fuzz, tiny/sample/full sha256 EXACT, three runs one hash. Local 1 vCPU ABBA: +3.6% over the raw engine (min 0.322s vs 0.311s), vs +11.7% for v9 on the same rig - locally the port cut most of the remaining tax.
- Board: run-fa21b467837a T 0.105919, M 45,600,768, S 1275, points 76.384372 - T inside v9's spread (0.1047-0.1080) while S grew 86 lines, about -0.5 points against v9's best. A second run (queued by this note) decides whether the T median beats 0.1051, the breakeven where the T gain would pay for the S; otherwise v9 is restored.
