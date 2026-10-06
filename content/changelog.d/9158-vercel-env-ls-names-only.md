Changed
- `local-ops`, `vercel-ops` and the `tm-secrets` skill list Vercel env var names only, filtered at the source with `vercel env ls <env> | awk 'NR>1{print $1}'`; `--json` and the unfiltered table are forbidden (#9158).
