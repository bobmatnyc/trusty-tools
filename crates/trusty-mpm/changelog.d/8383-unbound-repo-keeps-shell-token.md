Fixed

- `tm issue seed-labels`, `tm issue standard` and the other `tm` CLI `gh` calls no longer fail with "Could not resolve to a Repository" in a repository no project entry binds, when the shell exports a `GH_TOKEN` that can see it (#8383). The global `github:` fallback removed the shell's token, so `gh` asked as the global `config_dir`'s account. Only a project's own `github:` binding now outranks the shell's token.
