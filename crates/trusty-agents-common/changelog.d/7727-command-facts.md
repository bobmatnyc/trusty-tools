Fixed

- `ticketing.md` told an agent to run `gh issue create --add-project`, a flag
  `gh issue create` does not have (it takes `--project`; `--add-project` is
  `gh issue edit`-only). Every ticketing run that filed an issue hit this —
  9 failures across 7 runs, recurring three more times after that. Also
  removed a stale claim that `gh issue create --blocked-by` has no flag;
  installed `gh` 2.98 supports it directly. `version-control.md` now names the
  seven exact body headings (`## Outcome`, `## Changes`, `## Risk`,
  `## Tests`, `## Baseline`, `## Docs`, `## Review`) `tm pr open` checks, so an
  agent drafting a PR body no longer has to run `--help` to find them (#7727).
