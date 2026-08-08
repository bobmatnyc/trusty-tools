Added

- `palace_reembed` MCP tool — reports drawers that have no vector and optionally re-embeds them ([#4906](https://github.com/bobmatnyc/trusty-tools/issues/4906))
  - defaults to a dry run: it returns the exact set of vectorless drawer ids without touching the embedder
  - `dry_run: false` repairs them; it lives in the daemon because the daemon holds the palace's writer lock, so a CLI would only ever get a read-only snapshot it cannot write vectors into
