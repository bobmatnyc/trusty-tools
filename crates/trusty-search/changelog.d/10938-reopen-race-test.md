Fixed
- The reopen-versus-delete race test now waits on the queued DELETE instead of a fixed sleep, so it no longer flakes on a slow runner (#10938). Test-only; no runtime behavior change.
