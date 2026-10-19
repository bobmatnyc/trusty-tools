# tm-workflow — customizing the workflow

## How the PM Prompt Is Assembled

The bundled PM prompt has one source of truth: the JSON manifest
`assets/instructions/pm-instruction-package.json` (schema v2), embedded at
compile time via `bundled_pm_package.rs`. It declares section order and
composition; the prose for each section is stored separately in
`assets/instructions/sections/*.md`, pulled in as `include_str!` constants
registered in the `SECTION_SOURCES` table (`core/instruction_pipeline.rs`) — a
missing section file is a compile error, not a launch-time surprise. The
nineteen marker tokens (`core/claude_md_sections.rs::section_token`), in prompt
order, are `IDENTITY`, `CORE`, `PM-ALLOWLIST`, `DELEGATION-MECHANICS`,
`AGENT-ROUTING`, `SUBAGENT-RE-ENGAGEMENT`, `PHASES`, `QA-GATE`,
`GIT-FILE-TRACKING`, `TICKETS-PRS-RELEASES`, `MESSAGES-REPORTS-SESSIONS`,
`AUTONOMOUS-EXECUTION`, `MEMORY`, `SEARCH`, `WORKFLOW`, `AGENT-DELEGATION`,
`ENFORCEMENT`, `NON-OVERRIDABLE-RULES`, and `FRAMEWORK-GUARANTEED-CONVENTIONS`.
`IDENTITY` opens the prompt, so its override replaces the opening role
statement in place (#8533).

## The One Customization Surface

A project customizes any section outside the safety core exactly one way: a named-section
marker, `<!-- TRUSTY-MPM: <TOKEN> START v=1 -->` … `<!-- TRUSTY-MPM: <TOKEN> END
-->`, in the project's root `CLAUDE.md` — the sole marker host
(`core/claude_md_sections.rs::HOST_FILES`). This replaces exactly the matching
section, nothing else.

The five legacy per-file overrides
(`.trusty-mpm/{INSTRUCTIONS,AGENT_DELEGATION,WORKFLOW,MEMORY,
PM_INSTRUCTIONS_DEPLOYED}.md`) are RETIRED (#4286) and never read. Never create
one; if a project still has one, move its contents into `CLAUDE.md` and delete
the file — `tm doctor`'s `legacy_overrides` check fails until it is gone.

`CLAUDE.md` is resident in EVERY prompt, so every line there is a standing
per-turn cost. Content needed on every prompt belongs there; content needed only
sometimes belongs in a skill, a doc, or memory.

Robustness: a missing `CLAUDE.md`, a missing marker block, or an empty marker
body all fall back silently to the bundled default — a customization attempt
never blanks a section or crashes launch.

The agent-delegation roster is DYNAMIC, not authored prose: it comes from
`deployed_roster_section` → `roster_section_from_tiers`, a union of the project
tier, `$CLAUDE_CONFIG_DIR/agents`, and `~/.claude/agents`. It renders only the
agents Claude Code does NOT load itself, because the harness injects its own
`Available agent types for the Agent tool` listing covering the rest (#4513);
under tm that difference is the `~/.claude/agents` tier, which the harness never
reads once `CLAUDE_CONFIG_DIR` points at the managed directory. The section is
non-droppable — `validate_roster` rejects a package where the roster generator is
optional or absent (#4069).

## Trigger Phrases

| User says | PM writes to |
|---|---|
| "remember/always/never/for this project" | Plain prose in `CLAUDE.md` (no marker needed) |
| "use X agent for Y" / "route/change agent" | `<!-- TRUSTY-MPM: AGENT-DELEGATION START v=1 -->` block in `CLAUDE.md` |
| "add/change workflow phase" | `<!-- TRUSTY-MPM: WORKFLOW START v=1 -->` block in `CLAUDE.md` |
| "stop and check with me more/less often" | `<!-- TRUSTY-MPM: AUTONOMOUS-EXECUTION START v=1 -->` block in `CLAUDE.md` — e.g. tightening it to "ask before dispatching after a resume" (#8361) |
| "memory behavior" | `<!-- TRUSTY-MPM: MEMORY START v=1 -->` block in `CLAUDE.md` |
| "this project's voice / output style" | `.claude/output-styles/<id>.md`, selected by `[style] active = "<id>"` in `.trusty-mpm.toml` |

After writing an override, confirm the marker to the user and note it "takes
effect at next session startup" — the resolved prompt is assembled at
session-prepare time, not hot-reloaded.

## Inspecting the Resolved Prompt

```bash
tm sessions instructions       # prints the resolved prompt on stdout
cat .trusty-mpm/last-instructions.md   # the exact stash resolve_pm_prompt wrote
```

`tm sessions instructions` reports every applied, declined, and shadowed marker
on **stderr**, so `tm sessions instructions >/dev/null` alone answers "why didn't
my override apply?". Below the markers it prints one row per section — `core`,
`overridable`, or `overridden-by-project` — and the output style the launch
uses (#8533).

## Output Style Selection (#8533)

The style id resolves in this order: `tm launch --style <id>`, then
`[style] active` in the committed `.trusty-mpm.toml`, then the host config's
`[style] active`, then the harness manifest. A bundled id means the shipped
style; any other id names `.claude/output-styles/<id>.md` in the project. An
unknown or unreadable id is a visible warning, and the launch falls back to the
default style.

A project style keeps its prose, and the launch appends the floor to it: the
bundled style's **PRIMARY DIRECTIVE** and **Communication — Write Plainly**
sections. The floor is not overridable, and `tm sessions instructions` names
the style `<id> (project) + floor`. The launch writes that composite to
`.claude/output-styles/<id>.tm-floor.md` and names it in `outputStyle`, so a
bare `claude` in the project gets the floor too. `last-instructions.md` is written by `prepare_session` every
time a prompt is assembled, so the inspectable copy can never diverge from what
the PM received (#382).
