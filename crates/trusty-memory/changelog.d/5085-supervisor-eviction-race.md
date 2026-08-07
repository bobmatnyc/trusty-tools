Fixed

- BM25 supervisor: a daemon that had stopped serving its socket could be handed
  back to callers instead of respawned. `ensure_running`'s liveness check
  trusted `try_wait()`, which reports whether a child has been reaped rather
  than whether it is serving, so a killed daemon read as alive for the window
  between closing its listener and becoming reapable. Liveness is now backed by
  the socket the caller is about to use, and eviction fires only when the kernel
  proves nothing is listening (ENOENT/ECONNREFUSED) so a busy daemon is never
  mistaken for a dead one. A daemon evicted this way is still running, so it is
  now given a SIGTERM and time to flush its acked writes before the replacement
  takes over its socket, rather than being SIGKILLed on the spot.
