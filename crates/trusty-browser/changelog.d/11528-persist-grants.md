Fixed

- Grants survive a broker restart. The broker keeps `grants.json` in its state directory (0600, atomic write; grant id, scope, profile id and browser incarnation only, never a key or ticket) and restores it at start, still subject to the 45 s reconnect grace and the incarnation rule. A symlink, a non-owner-only mode, bad JSON or a malformed entry rejects the whole file and restores nothing. Every grant end rewrites the file, and a failed rewrite deletes it, so a revoked grant never returns.
