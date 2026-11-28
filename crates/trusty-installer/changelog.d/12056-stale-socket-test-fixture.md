Fixed

- The stale-socket probe test no longer reads `Serving` when a concurrent test's
  forked child holds the dropped listener's fd. The fixture binds the socket
  without `listen()`, so no copy of the fd can accept a dial (Refs #12056).
