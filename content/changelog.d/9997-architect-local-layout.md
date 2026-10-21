Changed
- The Architect skills `instructions/architect/skills/tm-fleet-check.md` and `tm-context-refresh.md`, the Architect `CLAUDE.md` template and `skills/tm-architect-setup.md` name the `local/` records layout instead of `records/` (#9997).
- `tm-fleet-check` reads `local/instance.md`, `local/user.md` and `local/goals.md` at pass start, verifies the observation channel every pass, and checks the live work against the goals. `tm-context-refresh` adds the pause and resume semantics and the Architect's own-session refresh. No context threshold changed (#9997).
