Fixed
- Every trusty-memory call now checks the daemon's wire protocol first, through one `memory::checked_rpc` door, and refuses an unsupported daemon by name instead of misparsing its replies (Refs #9538).
