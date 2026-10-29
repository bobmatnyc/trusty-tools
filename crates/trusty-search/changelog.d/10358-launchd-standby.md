Fixed

- A launchd-started `trusty-search start --foreground` that finds a healthy daemon (one answering `search.health`) holding the lock now stands by instead of exiting 1 into a 30 s `KeepAlive` respawn loop. It logs the holder pid at info, retries the lock every 5 s before any embedder or model load, takes over when the holder exits, and exits 0 on SIGTERM. A manual `start --foreground` and an unhealthy holder keep the old refusal. The post-race duplicate exit now logs at info.
