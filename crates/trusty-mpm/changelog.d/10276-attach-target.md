Changed
- The `project:` block in the repo's `issue-state.yaml` is now the single attach target (#10276). `tm issue standard` prints it as `attach_target`; `agents.ticketing.default_project` is used only by a repo without a block. A single-issue `tm issue audit` FAILs the project row when the issue is on a different board than the block names.
- A `project:` block is checked against the board spec when `issue-state.yaml` loads, and a block in the user-level or configured host model is no longer inherited by a repo that has none (#10276).
- `tm pr open` no longer adds a PR to `agents.ticketing.default_project` when an inherited project is closed (#10276). Pull requests are not board items.
- `tm issue epic create` attaches issues through the shared `tm issue board` module (#10276). Its behaviour is unchanged: only a missing project scope posts a `no-project:` comment.
