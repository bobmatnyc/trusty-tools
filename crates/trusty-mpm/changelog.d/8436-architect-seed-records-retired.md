Changed
- `tm fleet init` no longer seeds `records/`. The Architect's records live in `local/` (`state.md`, `goals.md`, `actions.md`); per-project records, the owner model and the user profile live in palaces and `AGENTS.md`. An existing install keeps its `records/`; nothing removes it (#8436).
