Fixed
- An idle-exiting UDS server now drains the kernel accept backlog for 50ms
  before giving up its listener, so a client that connected microseconds before
  the idle window elapsed is served rather than reset (#6350).
