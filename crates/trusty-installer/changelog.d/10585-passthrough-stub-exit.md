Fixed
- `trusty-installer <tool> <verb>` passthrough commands, such as `trusty-installer health trusty-memory`, now exit 2 instead of 0 while they are unimplemented, so install validation no longer reads the "not yet implemented" stub as a passing check. Use `trusty-installer stack health` for the real health rollup (#10585).
