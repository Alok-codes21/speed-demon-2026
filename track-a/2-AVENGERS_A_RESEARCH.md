# AVENGERS Track A exact-name research

Source: https://github.com/Mukul07777/speed-demon-2026/blob/speed-demon/track-a/src/main.rs
Fetched tip580d9f9916c5e8752b827681a2a930f5ba2bb094. Public MIT license embedded in candidate. Prior HighVelocity code is not incorporated in this new engine.

Candidate is research only, not wired to held's old Rust build. Do not merge held wholesale. Local candidate adds environment overrides and exact endpoint identity checks. A boxed2048x128-byte table is populated under the existing names lock and published by the existing slot release store. Acquire readers compare up to64bytes by SIMD, remainder by exact slice equality, including terminal space. Input names cannot contain spaces, so unequal lengths reject too. The shared table uses UnsafeCell for disjoint append-only slots; unsafe Sync remains dependent on release/acquire publication.

Upstream identifies endpoints by53 hash bits only. That probabilistic shortcut is deliberately removed. Tests force every hash equal and still match independent integer oracle.

## Gates

25/25 baseline differential:tiny/sample/full/shuffled/reversed x1/2/4/7/12workers.276 independent integer-oracle comparisons:12unseen tiny seeds,3unseen sample seeds,original/shuffled/reversed,1/4/12workers,normal and forced-all-collision builds; plus explicit1,15,16,17,31,32,33,63,64,65,95,124,125-byte name boundaries with middle-byte differences. All pass. This is evidence, not proof for all possible inputs.

## Costs

Canonical rustfmt1.99.1+scc4.1.0 Rust399; with7-line Dockerfile, estimated S406. Upstream official A best run-867a52e2a913 atca6d835c9c58e73290484c616e72912d36c3cf93:T0.018866s,M15876096B,S370,K52.903631,86.174274pts. Cosmic selected source official best85.009524,S511. Different samples, not a paired experiment.

Release-equivalent local builds(opt3,fatLTO,1codegen-unit,x86-64-v3,+aes,+vaes,+crt-static).41paired runs of an earlier inline comparator build gave exact-check overhead19.34% on1worker and18.42% on12workers. A subsequent audit restored an inadvertently changed long-key hash load from len-63 to upstream len-64; final candidate reran all gates clean, but that exact final source has no paired upstream timing receipt yet.Name table overhead is about0.25MiB, not measured organizer memory. Conditional estimate at those earlier overheads and S406:83.95-84.04pts at unchanged M, below85.01. Local1CPU cannot establish organizer12-thread scaling. No live gain established.

Simple OnceLock slice-equality version cost20-22% with release flags. Fixed-name memcmp version cost25-28%. Atomic-pointer SIMD version cost18-20% and S413. None promoted. No official T/M for any port. Live remains THP+dedup source SHA dc4bb7ce40018b02c4e9569456faba4988370ee6a5088c2beaa4cf3325bf9fb2.

Final exact source21paired receipt:1worker upstream96.008885ms/candidate114.093613ms(+18.84%);12workers101.318778/118.128074(+16.59%). Still not net-positive at S406 by conditional points estimate.
