Fixed

- `spawn_dream_scheduler` staggers each palace's first dream tick instead of starting every loop's clock at the same instant (#7106). Palace k of n now waits `idle_secs * k / n` past its first interval, so the 61 loops on the reference host no longer wake in the same second — 61 `dream_stats.json` writes landed inside one 31-second window, and a 43 GB transient came with them. The startup log line records each loop's offset and the process-wide cycle cap.
