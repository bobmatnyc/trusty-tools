Fixed

- Bounded the per-palace chat-session store cache, closing an unbounded
  file-descriptor leak that would have hit `EMFILE` in ~3-4 weeks (#4639)
  - `AppState::session_stores` was a `DashMap` with no `remove`, TTL, or cap, so
    every palace the daemon ever touched leaked one `chat_sessions.redb` handle
    for the process lifetime — a live daemon was measured holding 844, all of
    them pinning files already unlinked from disk, growing ~250-300/day against
    an 8 192 fd ceiling
  - it is now an LRU cache capped at 32 resident handles
    (`TRUSTY_MEMORY_MAX_OPEN_SESSION_STORES`), mirroring the `PalaceRegistry`
    LRU that already bounds kg/usearch/recall handles; evicted stores reopen
    from disk transparently on the next request
  - eviction never closes a store a caller still holds, so an in-flight chat
    stream cannot be interrupted and no second open of a live redb file can
    occur
  - `delete_palace` now drops the palace's cached handle, so deleting a palace
    actually releases its fd instead of pinning the deleted inode
