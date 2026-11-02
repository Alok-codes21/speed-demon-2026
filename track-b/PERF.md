

## Step 82: v12 (prefetch distance 40) - retired, v11 restored

- One-knob trial: the scan's bucket prefetch distance went from 20 tokens to 40. Local box was too loaded to resolve it (an earlier quiet-box probe showed about -1%); the board run settled it.
- Board: run-ab9e15493d63 T 0.106735, M 45,367,296, S 1206, points 76.504562 - behind v11's 0.105786 two-run median, so the distance stays 20 and v11 is restored (this commit).
- v11 two-run median T 0.105786 (run-c10c9cf7c855 0.105443, run-5e575cc5d4d4 0.106129), S 1206, M about 44.3 MB - freeze tip going into the final day.
