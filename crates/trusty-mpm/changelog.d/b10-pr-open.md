Changed
- `tm pr open --head <branch>` opens a source PR when the named head resolves to the commit the checkout stands on, including through `origin/<branch>` — only a head the changelog gate cannot diff still requires `--docs-only` (#7747).
- `tm pr open --minimal` skips the seven-heading body contract for a project whose own `CLAUDE.md` names a different PR-body standard; the attribution-footer, `Refs`/`Closes` and changelog-fragment gates still run (#7615).
- A `tm pr open` failure naming a missing body heading now prints the seven-heading skeleton verbatim, ready to paste and fill (#7574).
