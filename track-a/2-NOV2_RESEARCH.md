# Nov2 Track A candidate ledger

Current baseline: THP+dedup S511, selected source SHA dc4bb7ce40018b02c4e9569456faba4988370ee6a5088c2beaa4cf3325bf9fb2.
All trials below local only, independent single changes. Each passed25/25 byte-exact against selected baseline on tiny/sample/full/shuffled/reversed x1/2/4/7/12 workers. Timings are medians of21 interleaved randomized-order pairs, not official measurements. Local host one CPU, no organizer scaling claims.

|Candidate|1worker ratio|12worker ratio|Canonical total S|Judgment|
|---|---:|---:|---:|---|
|Names length rejection|1.0120|1.0083|511|No demonstrated gain|
|Remove lat reserve|1.0042|1.0031|511|Neutral|
|Lag2->1|2.1024|2.1561|511|Reject: fallback rerun penalty|
|Streams4->2|1.0715|1.0580|511|Reject|
|Streams4->8|1.1358|1.1247|511|Reject|
|Block256->128KiB|1.0557|1.0554|511|Reject|
|Block256->512KiB|1.0079|1.0333|511|No gain|
|Local slots16384->8192|1.0310|1.0114|511|No gain|
|Remove unused Job destructor|1.0216|1.0141|507|Identical machine text to fresh control; source-axis gain|
|Remove prefetches|1.0172|1.0273|510|No gain|
|Prefetch1280->640|1.0241|1.0069|511|No gain|
|Local slots16384->32768|1.0006|1.0230|511|No gain|
|Lag2->3|1.0113|1.0055|511|No gain|

Control caveat: rebuilding identical baseline gave+0.8%/+1.4% timing differences over41 pairs, consistent with binary path/measurement noise. Fresh-control .text and destructor-candidate .text are identical SHA256 fef770563a2c8904179fb4952254a4efd31c3f6dc7cd2a44ba5e607dfbf7655b. Removing never-invoked destructor is therefore machine-code neutral, despite apparent local timing difference. No official net result yet. Original source MIT notice preserved in every candidate.
