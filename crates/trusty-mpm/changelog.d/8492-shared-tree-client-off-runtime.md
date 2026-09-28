Fixed
- `tm hook --pm-guard` builds the shared-tree query's HTTP client on a
  detached thread instead of inside the async call. The worktree-removal
  budget's deadline can now fire while that build is still running, and the
  process can exit with the deny without waiting for the build. A client that
  cannot be built still denies the removal. The client's 500 ms connect and
  2 s request timeouts are unchanged (#8492).
