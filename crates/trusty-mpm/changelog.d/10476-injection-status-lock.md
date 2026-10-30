Fixed
- The turnkey task-injection status write now reads and writes the session record under one store lock, so it no longer overwrites a field another setter wrote in between (#10476).
