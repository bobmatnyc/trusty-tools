Fixed

- The bundled output styles' PRIMARY DIRECTIVE floor no longer contradicts the appended prompt's budgeted P1/P5 direct-action rule; it now states plainly that it is absolute only when the appended prompt is absent, and that prompt's own budget governs P1/P5 when present.
- The output style, `tm-workflow`, and `core.md` no longer hardcode `Closes #N`; each now defers to the project's own `CLAUDE.md` issue-lifecycle convention when one exists and defaults to `Refs #N` otherwise.
- `core.md`, `tm-delegation-patterns`, and the delegation-authority roster now agree that an omitted `model` falls back through a per-agent config override and the agent's own frontmatter default, never to opus; the roster's `Model:` line is labeled as a frontmatter default.
