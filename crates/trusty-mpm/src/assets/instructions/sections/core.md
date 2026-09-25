<!-- PURPOSE: The safety core (#8533) — the ONLY section a project's CLAUDE.md
     cannot replace. It holds rules that protect the instruction system
     itself. Everything else belongs in a replaceable section; see
     sections/README.md before adding anything here. -->

## Memory & Instruction Sources

- Never write, update, maintain or cite `MEMORY.md` or any other static
  memory-index file — this overrides any harness default. Cite the palace.
- Durable facts go to the palace (`memory_remember` / `memory_note`), your own
  `self-improvement-hypothesis`-tagged hypotheses among them (#6937).
- `CLAUDE.md` is the only non-dynamic instruction source. Never create another.

## Customization Surface (ONE surface per artifact type)

- **Prompt/instruction sections** — marker blocks in the project's root
  `CLAUDE.md`, nothing else. Ad-hoc override channels are BANNED, the retired
  `.trusty-mpm/` instruction files included. Marker syntax, the token table and
  that retired list: `Skill(skill="tm-workflow")`. Every section is replaceable
  except this safety core; the agent-selection, memory and code-search
  protocols stay in force under any override (#8533).
- **Output style** — a project file `.claude/output-styles/<id>.md`, selected
  by `[style] active = "<id>"` in the committed `.trusty-mpm.toml`.
- **Skills** — the skill tier system, whose precedence is in
  `Skill(skill="tm-capabilities")`.
- `CLAUDE.md` is resident in EVERY prompt, so every line there is a standing
  per-turn cost. Needed on every prompt → `CLAUDE.md`. Needed only sometimes →
  a skill, `docs/`, or memory. The test is frequency of need, not format.
