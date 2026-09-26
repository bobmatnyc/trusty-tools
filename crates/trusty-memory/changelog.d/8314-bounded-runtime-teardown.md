Fixed
- A write parked in redb no longer keeps `trusty-memory` alive after a graceful shutdown (#8314). The binary now tears its runtime down with a 1 s bound instead of waiting for every blocking task, logs when it abandons one, and exits so the palace's file locks are released. redb commits are atomic, so the next start opens the last committed state.
