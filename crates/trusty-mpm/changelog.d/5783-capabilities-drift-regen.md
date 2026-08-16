Fixed

- Regenerated `tm-capabilities/references/cli.md` and `mcp-tools.md`, which had drifted from `main` since v1.5.0 (#5769) added a daemon route and changed the guard surface. `mcp-tools.md`'s `session_new` entry now reflects the ADR-0037 reversal: a local `repo_url` runs the session on that main checkout itself, only a remote URL is cloned into a freshly-provisioned workspace.
