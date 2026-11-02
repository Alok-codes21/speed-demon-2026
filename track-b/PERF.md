

## Step 81: v11 (two-digit-LUT output formatting) - trial

- The 1.46 MB of output was formatted one digit at a time (divide and modulo per digit, then an in-place reversal), inside the timed query phase. put_number now writes two digits at a time from a 100-entry pair table into a stack buffer, most significant pair last; the bytes written are identical.
- Gates: 60/60 fuzz, tiny/sample/full sha256 EXACT, 0 DBG markers. Local 1 vCPU ABBA (8 pairs): min 0.339s vs v9's 0.344s, about -1.5%.
- Board: run-c10c9cf7c855 T 0.105443, M 44,261,376, S 1206, points 76.743969 - T ahead of v9's 0.1073 four-run median. A second run (queued by this note) sets the median; v11 becomes the freeze tip if it holds.
