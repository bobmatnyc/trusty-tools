Fixed

- **A server that idled out could make its own successor fail to start.** `serve_with_idle` unlinked the socket while the router — and through it every `AnalyzerAppState` clone, so both redb handles — was still alive, so the `facts.redb` and `scip_overlays.redb` locks outlived the path a client keys off. A client that saw the unlink spawned a successor, whose `FactStore::open` hit `Database already open. Cannot acquire lock.`; the successor died before binding, `Supervisor::ensure_running` never noticed, and the caller waited out the full 20s spawn probe for a `SpawnTimeout`. The router is now dropped before the unlink, so the locks are free by the time anything can observe the server as gone (#6595)
  - measured at 54–560 ms of exposure on an idle machine, with `lsof` naming the exiting server as the only holder in 15 rounds out of 15; 0 out of 15 after the change
  - the same ordering applies to the SIGTERM/SIGINT exit, which had the identical window
