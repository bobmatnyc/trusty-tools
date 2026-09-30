Changed
- In a repository with no `origin`, a `tm pr` verb given no `--repo` prints `local-only repo: no remote; skipping push/PR` and exits 0. A verb given `--repo` runs as usual and fails under the session's gh pin, so `tm pr merge <n> --repo o/r` never reports a skip that a delivery flow reads as a merge. The allow-listed supervisor directory never skips (#8934).
