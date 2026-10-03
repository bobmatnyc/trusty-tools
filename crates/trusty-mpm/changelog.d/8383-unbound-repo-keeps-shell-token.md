Fixed

- `tm issue seed-labels`, `tm issue standard` and the other `tm` CLI `gh` calls no longer fail with "Could not resolve to a Repository" in a repository no project entry binds, when the shell exports a `GH_TOKEN` that can see it (#8383). The global `github:` fallback removed the shell's token, so `gh` asked as the global `config_dir`'s account. The global fallback now removes nothing: a global `config_dir` or `host` leaves the shell's token deciding, and a global `token_env` still sets `GH_TOKEN`, as explicit config. A project's own `github:` binding still outranks the shell's token.
