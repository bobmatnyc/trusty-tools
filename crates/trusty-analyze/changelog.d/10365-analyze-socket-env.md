Fixed
- `TRUSTY_ANALYZE_SOCKET` now sets the socket `serve` binds and every client dials; a `--socket` flag still wins, and a relative path or a missing parent directory is an error (#10365).
