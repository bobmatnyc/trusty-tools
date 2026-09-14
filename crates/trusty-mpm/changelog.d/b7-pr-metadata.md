Fixed

- `tm pr open` inherits the linked issue's project and milestone from a `Closes #N` body as well as a `Refs #N` one; the derivation read only `Refs`, so every `--closes` PR opened with neither (Refs #7869).
- `tm pr open` reports the PR it created when `gh pr create` exits non-zero AFTER creating it — a 502 on the follow-up assignee/label call printed no number at all. The command now prints the number and URL, retries that metadata step once, and exits 3 (the PR exists, metadata incomplete) only when the retry also fails (Refs #7869).
- `tm pr open` retries a failed metadata apply once, field by field, so an unresolvable milestone no longer takes the component labels down with it; the warning names the field, its value and the issue it was inherited from instead of relaying `gh`'s raw stderr (Refs #7646).
- `tm pr open` falls back to `gh pr view` when a link line names a pull request rather than an issue, instead of abandoning the milestone and projects on the `Could not resolve to an Issue` 404 (Refs #7786).
- `tm pr merge` refuses only on a missing attribution footer, which is part of the squash commit message it writes; the seven-field body contract is now reported rather than enforced at merge time, so a body written to the sparse prose rules no longer forces a fallback to raw `gh pr merge` (Refs #7868).
