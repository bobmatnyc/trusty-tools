# trusty-code TUI Design Analysis — Index

**Status:** Informative (design analysis, not a behavior-contract spec)
**Subsystem:** trusty-code-tui / trusty-code TUI client
**Owner:** Engineering (trusty-code)
**Last-updated:** 2026-09-06
**Scope:** A full design analysis of the real Claude Code interactive terminal
UI (the `claude` CLI's TUI), producing a traceable requirements set for
trusty-code's own TUI (`trusty-code-tui` + `crates/trusty-code/src/tui_client`).

## What this set is

This is a **design-analysis document set**, not a spec. It does not carry
`spec_refs:` frontmatter and creates no new `SPEC-*` IDs, because no governing
spec cites it yet — per DOC-38's rule against fabricating spec links. Where a
requirement in [requirements.md](requirements.md) should become binding, it is
a candidate for folding into [DOC-50](../../../specs/DOC-50-tcode-tui-claude-code-clone.md)
(or a successor spec) in a follow-up change; this set does not itself edit
DOC-50.

## Files in this set

1. [screen-inventory.md](screen-inventory.md) — every screen/overlay the real
   Claude Code TUI presents: trigger, layout, editable surface, exit path.
2. [interaction-model.md](interaction-model.md) — the full keyboard map, vim
   mode, multi-line input, paste/@-mention/slash autocomplete, history,
   interrupt semantics, message queueing, rewind/checkpoints.
3. [rendering-and-layout.md](rendering-and-layout.md) — transcript structure,
   markdown/diff rendering, truncation/expansion, theme handling, resize,
   scrollback, notifications, title-bar.
4. [state-and-session.md](state-and-session.md) — session ids, resume/continue,
   checkpoints, compaction/context indicators, settings precedence, memory
   files, output styles, permission modes, hooks visibility.
5. [extensibility.md](extensibility.md) — slash commands/skills, subagents
   display, MCP status, plugins, status line scripts, IDE bridge behavior.
6. [requirements.md](requirements.md) — the numbered MUST/SHOULD/MAY
   requirement list derived from files 2–5, each traced to a source section,
   each marked as DOC-50-covered / new / conflicting.
7. [gaps-and-open-questions.md](gaps-and-open-questions.md) — what could not
   be observed or verified, and what would settle it.

## Sources

- **Installed binary:** `claude` v2.1.263 (Claude Code), run non-interactively
  only (`--help`, `--version`, and every listed subcommand's `--help`). No
  interactive session was started inside this analysis.
- **Official docs** (`code.claude.com/docs/en/*`, redirected from
  `docs.claude.com`): interactive-mode, commands, slash-commands, statusline,
  hooks-guide, ide-integrations, memory, iam/authentication, permission-modes,
  permissions, output-styles, mcp, checkpointing, fullscreen, settings,
  sub-agents, context-window. Each fact below is cited to its source page or
  to the binary's own `--help` output; where the two disagree, both are
  recorded (see [gaps-and-open-questions.md](gaps-and-open-questions.md)).
- **trusty-tools prior art (read, not modified):**
  [DOC-50](../../../specs/DOC-50-tcode-tui-claude-code-clone.md) (the existing
  tcode TUI spec — MVP Slices 1–7 and 10 are already implemented per
  `crates/trusty-code-tui/CHANGELOG.md`), [DOC-48](../../../specs/DOC-48-tcode-workstreams.md)
  (workstream domain object and SSE events the TUI surfaces),
  [DOC-51](../../../specs/DOC-51-tcode-plugin-support-phase1.md) (plugin
  agents/skills discovery), `crates/trusty-code/README.md`,
  `crates/trusty-code-tui/src/**` (survey level), `docs/design/trust-code/`
  (checked — empty at the time of this analysis; nothing to reconcile against),
  `docs/design/UI/README.md` (Foundry design system).
- **Date of analysis:** 2026-09-06.

## Relation to DOC-50

DOC-50 is the existing functional spec for trusty-code's TUI: a Claude-Code
*clone* built as a thin client (`TuiEngine` trait) over `trusty-code-tui`
(shared crate) with `CodeEngine` as the tcode-specific adapter. Its MVP scope
(Slices 1–7, 10) is implemented; Phase 2 (permission prompts, tool cards,
pickers, SSH fallback) is open. This design-analysis set:

- **Does not replace DOC-50.** DOC-50 states trusty-code's own architecture
  (thin-client axiom, engine-adapter seam, extraction plan) — none of that is
  in scope here.
- **Adds** a requirements baseline derived from what the *real* Claude Code
  TUI does today (2026-09), which DOC-50 (drafted 2026-07-20, before several
  of the described features shipped in the real product) could not have
  anticipated — session recap, `/btw` side questions, the diff panel,
  fullscreen rendering, auto mode, agent-view/background-session panels,
  emoji shortcodes, spell-check, PR-status badges, and more.
- **Flags** (in [requirements.md](requirements.md)) every place a requirement
  here conflicts with a DOC-50 decision or an owner directive, without
  resolving the conflict — resolution is Bob's, as DOC-50 §6 establishes for
  its own open questions.
- **Reuses owner directives already in force:** trusty-code reuses trusty-mpm
  agents and skills; the Projects roster in any workstream/project picker
  comes from the shared trusty-mpm project registry (DOC-48 §8 Rev 9,
  DOC-39 §5.8.1, issue #3435) — carried forward here, not re-litigated.
