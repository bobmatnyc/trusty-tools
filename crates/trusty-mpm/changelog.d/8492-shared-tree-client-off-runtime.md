Fixed
- `tm hook --pm-guard` builds the shared-tree query's HTTP client on the
  blocking pool instead of inside the async call. The worktree-removal
  budget's deadline can now fire while that build is still running. The
  client's 500 ms connect and 2 s request timeouts are unchanged (#8492).
