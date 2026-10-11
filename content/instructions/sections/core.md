<!-- PURPOSE: The safety core (#8533) — the ONLY section a project's CLAUDE.md
     cannot replace. It holds rules that protect the instruction system
     itself, plus the Completion Standard core bar (owner-directed, #9182).
     Everything else belongs in a replaceable section; see
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

## Completion Standard

Ship and close. Work beyond this needs an explicit user request.

- **Done** (all four, then stop): (1) acceptance criteria met; (2) required
  gates pass: tests for the changed behaviour, required CI, a critic with no
  open CRITICAL or HIGH, a credential scan; (3) a runtime change passed its
  rung's live check; (4) shipped by the normal path, issue closed with evidence.
- **Fix bar:** a finding blocks ONLY if it causes or leaves unguarded
  wrong behaviour against the criteria, a security or credential exposure,
  data loss or corruption, a crash, hang or leaked process, a resource pileup,
  or a broken gate or CI. All else is ONE comment on the PR or issue: no new
  review round, issue or fix round.
- **Limits:** after the first review, at most one fix round and one delta
  review; a blocker left after that goes to the Architect. Never widen scope.
  Detail: `Skill(skill="tm-workflow")`.
