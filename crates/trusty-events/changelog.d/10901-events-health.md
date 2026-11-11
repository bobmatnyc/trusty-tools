Added
- `events.health` on the bus socket: reports status, version, uptime, the ingest/dedup/eviction/log-drop counters, open subscribers and the head `seq`, behind the same peer check as `events.subscribe` and without taking the ring lock (#10901).
