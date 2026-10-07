Fixed
- The embedder-fallback concurrency test now drives both supervisor crash-restart cycles itself and decides by spawn count, instead of polling for give-up under a 45 s ceiling. A first spawn whose startup probe timed out on a loaded host left the old mock unable to reach the supervisor, and the test timed out (#3569).
