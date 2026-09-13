Fixed

- `tm-ticketing.md`'s `gh issue create` examples told an agent to pass
  `--add-project`, a flag that subcommand does not have (it takes `--project`;
  `--add-project` is `gh issue edit`-only). `tm-workflow.md`'s "Minimal PR Body
  (seven fields)" section now names the seven exact headings `tm pr open`
  checks (`## Outcome`, `## Changes`, `## Risk`, `## Tests`, `## Baseline`,
  `## Docs`, `## Review`), read back from the checker's own `Field::heading`
  list by a new test (`seven_body_headings_are_named_verbatim_in_the_assets`)
  that fails if either asset drifts from it. Three of five PR-opening runs had
  hit `tm pr open`'s exit 2 with no asset naming the headings it checks
  (#7727).
