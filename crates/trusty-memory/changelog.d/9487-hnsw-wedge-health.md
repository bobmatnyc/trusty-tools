Fixed
- `memory.health` now reports a wedge when an HNSW `upsert` or `search` has been blocked past the wedge threshold, naming the palace and `stalled_lock.lock: "hnsw"`. Before, a graph call left blocked after its timed-out future was dropped read as healthy with 0 in flight (#9487).
- `trusty-memory doctor` names the restart for an HNSW wedge: `launchctl bootout gui/$(id -u)/com.trusty.memory`, then `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.trusty.memory.plist` (#9487).
