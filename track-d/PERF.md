# Track D local candidate: edgepref

Not submitted. No board run ID exists for this candidate.

Base: 48159df75b18107bd17353ee2cc4e28e5e3c32a1, banked run-aaf0ca261822, T=0.172782 s, M=70500352 B, S=475, D=1, points=68.801804.
Restore run-98555c37390f at b32d6e4796322cca86ae87ace49e5f1af74071ca passed every gate at 68.644372 points.

Local byte-exact gates: full public seed-1 SHA256 a23dbdf0a3e49cc8901310a9ee1dc6a9a569a85297b6da75c2a7002cb2ed277e; sample a4c76ed030bc7e365db33191f6cf74708fba7f25eb48d64c8c4279f2e57046a0; tiny ecaca77d3221fe3928109da36480cfdb39f288f2e57ecbc61d3d7706d5cb4bd5, identical to reference.

Single-core local median from 8 interleaved runs: 0.584345 s, baseline 0.615605 s. Not official scoring hardware or 12-core timing.

Attribution: adapted from MIT-licensed manusingh090/speed-demon-2026-entry. Retain repository LICENSE and source attribution.

Safe prefetch correction: avoid bounds-checking CSR offset when a node has no outgoing edges; use wrapping_add on the base pointer.
