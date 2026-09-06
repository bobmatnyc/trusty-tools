# Requirements — trusty-code TUI (derived from the Claude Code TUI analysis)

**Status:** Informative (candidate requirements — not yet accepted into
DOC-50 or any binding spec)
**Last-updated:** 2026-09-06

Each requirement traces to a section in `screen-inventory.md`,
`interaction-model.md`, `rendering-and-layout.md`, `state-and-session.md`, or
`extensibility.md` (cited as `file §n`). Each is written so a wrong
implementation observably fails it. **DOC-50 status** is one of: `COVERED`
(DOC-50 already requires this, cite the AC/slice), `NEW` (not in DOC-50),
`CONFLICT` (contradicts a DOC-50 decision or owner directive — flagged, not
resolved). Numbering is sequential across the whole set; MUST/SHOULD/MAY
sections are separated for scanability.

All requirements are subject to DOC-50 §2.1's thin-client axiom (C-1..C-4):
any requirement below that needs a fact only the daemon can supply is a
**daemon API requirement**, not a UI-only one — implementing it client-side
without a corresponding daemon RPC/event is itself a spec violation, not an
acceptable shortcut.

## MUST

**R1.** The input composer MUST accept multi-line input via at least one
escape mechanism that works with no terminal configuration (`Ctrl+J`, per
`interaction-model.md` "Multi-line input"). — **NEW**. Rationale: DOC-50's
MVP composer is single-line-oriented per its slice descriptions; no
multi-line escape mechanism is named in DOC-50 §4.1.

**R2.** `Esc` MUST interrupt an in-flight turn while preserving work done so
far and MUST send any queued input immediately afterward
(`interaction-model.md` "Interrupt semantics", "Queueing"). — **COVERED**:
DOC-50 §5 Slice 5 wires `Ctrl-c` to `engine.cancel_session()`; extend the
same acceptance criterion to `Esc` with queue-drain semantics.

**R3.** A message typed and submitted while a turn is running MUST be
queued, not interrupt the turn, and MUST be delivered per the turn-boundary
rule in `interaction-model.md` "Queueing a message while Claude works". —
**NEW**. trusty-code-tui's current `busy` gate (CHANGELOG 0.1.1) *blocks*
submission outright while busy rather than queueing it — a behavioral gap
against the real product, not equivalent design.

**R4.** Every user-facing permission gate MUST render tool name, a
human description, and an explicit Allow/Deny choice, and MUST offer a
"don't ask again" affordance where the underlying rule can be persisted
(`screen-inventory.md` §3; `state-and-session.md` "Permission modes"). —
**COVERED (Phase 2)**: DOC-50 §4.2 Slice 9 scopes permission/diff prompts to
Phase 2; this requirement is the acceptance detail for that slice.

**R5.** The active permission mode MUST be visibly indicated at all times
(status line or equivalent persistent indicator), and switching modes MUST
be a single explicit user action (`state-and-session.md` "Permission modes
and their TUI reflection"). — **NEW** relative to DOC-50 §4.1's MVP status
line (session/workstream/model/project only, no permission-mode field named).

**R6.** Tool invocations MUST render at minimum as a one-line summary with
an expand affordance; expansion MUST NOT be the only rendering mode
(`screen-inventory.md` §6; `rendering-and-layout.md` "Tool-result truncation
and expansion"). — **COVERED**: DOC-50 §4.1 MVP already specifies
`[TOOL] name: args` / `[RESULT] …` text rendering; fancy cards are §4.2
Slice 8. This requirement adds the collapse/expand toggle DOC-50 doesn't
name explicitly.

**R7.** `/help` MUST list every available command, including
skill/plugin/MCP-contributed ones with a provenance marker
(`extensibility.md` "Slash commands and skills"). — **COVERED**: DOC-50 §4.1
lists `/help`; this requirement adds the provenance-marker detail (missing
from DOC-50's text).

**R8.** Command history MUST be scoped per working directory and MUST
survive `/clear` (new conversation) without losing prior-session recall
(`interaction-model.md` "Command history"). — **COVERED**: DOC-50 §4.1 MVP
lists "Input composer history works"; this requirement adds the
per-directory-scope and post-`/clear` recall details.

**R9.** A running subagent/background task MUST be visible in a dedicated
panel distinct from the main transcript, MUST show its task description,
and MUST offer a stop action (`screen-inventory.md` §9; `extensibility.md`
"Subagents display"). — **NEW**. Nothing in DOC-50 §1–§5 names a subagent
panel; DOC-50's scope is a single-turn daemon RPC loop with no visible
concept of concurrent background work.

**R10.** MCP server connection status MUST be visible via a dedicated
command/panel (`/mcp`-equivalent) with at minimum connected/failed/pending
states (`extensibility.md` "MCP server status and tool loading"). — **NEW**.
DOC-50 does not mention MCP at all — trusty-code's daemon-owned MCP surface
(per CLAUDE.md's model-context-builder conventions) has no client-visible
status requirement stated anywhere in DOC-50.

**R11.** Settings precedence (managed → CLI → project-local → project-shared
→ user) MUST be documented and MUST be discoverable from within the TUI
(equivalent of `/config`) (`state-and-session.md` "Settings precedence"). —
**NEW** relative to DOC-50, which defines no settings-precedence model for
tcode-tui itself (it inherits `.claude/` config per `crates/trusty-code/
README.md` "Design Constraints", but no TUI-visible settings surface is
specified).

**R12.** The TUI MUST NOT read the filesystem, spawn a process, or shell out
to git directly to satisfy any requirement in this document — every fact
(git branch/status for a status line, PR badge state, file diff content,
etc.) MUST arrive from a daemon call (DOC-50 §2.1 AC-19.1..19.3). — **COVERED
(binding, restated)**. This requirement exists here only to flag that several
real-product behaviors (PR-status badge via local `gh`/`glab` credential
lookup, spell-check via a local `aspell`/`hunspell` binary) are, in the real
Claude Code CLI, done by direct local process/credential access — a design
that DOC-50's thin-client axiom explicitly forbids trusty-code-tui from
copying verbatim. See **CONFLICT** entries below.

## SHOULD

**R13.** The TUI SHOULD support vim-style editing as an opt-in mode with at
minimum NORMAL/INSERT mode switching and basic motions
(`interaction-model.md` "Vim mode"). — **NEW**.

**R14.** The TUI SHOULD render a rewind/checkpoint affordance that lists
prior prompts and supports at least "restore conversation to here"
(`screen-inventory.md` §20; `interaction-model.md` "Rewind and checkpoints").
— **NEW**. No checkpoint/rewind concept appears anywhere in DOC-50.

**R15.** The TUI SHOULD show a colored context-usage indicator (percentage
or grid) so the user can anticipate compaction (`state-and-session.md`
"Checkpoints and compaction/context indicators"). — **NEW**. DOC-50 §6 Q9
explicitly defers even *cost* display as a missing daemon API; context-usage
display is a distinct, likewise-undeclared daemon API gap.

**R16.** The TUI SHOULD offer a fullscreen/alt-screen rendering mode with
flicker-free redraws and mouse support as an alternative to a
scrollback-append classic mode (`rendering-and-layout.md` "Transcript
structure"). — **NEW**. Ratatui's alt-screen mode (DOC-50 §2.2/§3.1 already
mandates alt-screen + raw mode unconditionally) makes this **partially
COVERED** by construction — trusty-code-tui has no non-alt-screen "classic"
fallback at all, which is the inverse gap from the real product (real
product defaults toward fullscreen only recently and keeps classic as an
explicit fallback for narrow/incompatible terminals). Cross-reference R23.

**R17.** The TUI SHOULD support an emoji-shortcode and/or spell-check input
aid, each independently toggleable (`interaction-model.md` "Quick commands";
`rendering-and-layout.md` "Colour and theme handling"). — **NEW**, low
priority.

**R18.** The TUI SHOULD show a PR/MR review-state badge in the footer when
the current branch has an open pull/merge request, sourced from a daemon
call, not a local `gh`/`glab` invocation (`rendering-and-layout.md`
"Notification bell / title-bar updates"). — **NEW**, and see **R12/CONFLICT**
below for the sourcing constraint.

**R19.** The TUI SHOULD surface hook configuration read-only (a `/hooks`-
equivalent) without implying hooks execute client-side (`state-and-session.md`
"Hooks surfaced in the UI"). — **NEW**.

## MAY

**R20.** The TUI MAY offer a `/btw`-style side-question overlay that answers
from existing context without adding to the transcript or requiring tool
access (`interaction-model.md`, referenced via `screen-inventory.md`
context). — **NEW**, explicitly optional; no daemon-side equivalent is known
to exist.

**R21.** The TUI MAY offer a session-recap line when reattaching to an idle
session (`interaction-model.md` context). — **NEW**, optional.

**R22.** The TUI MAY support `:emoji:` shortcode completion
(`interaction-model.md` "Quick commands"). — **NEW**, optional, folds into
R17.

## Resolved by owner ruling, 2026-09-06 (now MUST)

**R23 (owner ruling, 2026-09-06).** The TUI MUST render so the terminal's
native scrollback survives. Alt-screen MUST NOT be the default rendering
mode; classic/inline rendering — like Claude Code's own default
(`rendering-and-layout.md` "Transcript structure") — is the default, and
fullscreen is at most an opt-in mode. — **Resolves R23**, previously flagged
as a conflict with DOC-50 §3.1/§9 Q3, which mandated ratatui's alt-screen +
raw mode unconditionally in the shared `trusty-code-tui` crate and deferred
an SSH/narrow-terminal plain-line fallback to Phase 2 (§9 Q3, "RESOLVED …
Phase 2"). Asked what trusty-code-tui uses today (alt-screen + raw mode via
ratatui/crossterm, DOC-50 `setup_terminal`), the owner ruled:

> "I want rendering that allows scroll back."
> — Bob, 2026-09-06

DOC-50 is amended accordingly — see its 2026-09-06 Amendment near
`setup_terminal`, the Phase 2 SSH fallback, and the "requires full-size
terminal with alt-screen support" known limitation. The SSH/narrow-terminal
plain-line fallback (#3405) is no longer a separate Phase 2 feature: classic
rendering is the default everywhere, so the same rendering mode already
covers SSH and narrow terminals. Cross-reference R16.

**R25 (owner ruling, 2026-09-06).** The `/resume`-equivalent picker MUST read
the shared trusty-mpm project registry (DOC-48 §8 Rev 9, DOC-39 §5.8.1, issue
#3435) and MUST filter its roster to the project that owns the current
working directory — registry and cwd are meant to agree. A cwd outside any
registered project is a designed-for edge case, not an error: the picker MUST
either offer to register the cwd as a new project, or show every registry
project and let the user pick one, and when the user picks, the picker MUST
mark that pick as the PM's assumption rather than a confirmed match. —
**Resolves R25**, previously flagged as a conflict between cloning Claude
Code's cwd-scoped `/resume` picker verbatim and the owner directive to source
the roster from the shared trusty-mpm registry:

> "Resume picker should use registry which should align to cwd."
> — Bob, 2026-09-06

## Conflicts and flags (not resolved here)

**R24 — CONFLICT with DOC-50 §2.1 C-4 (thin-client axiom).** The real Claude
Code TUI sources its PR/MR badge (R18) and spell-check (R17) from **local**
`gh`/`glab` credentials and a **local** `aspell`/`hunspell`/`ispell` binary
respectively — direct local process/credential access that DOC-50's
thin-client axiom (C-1..C-4, AC-19.1..19.3) forbids trusty-code-tui from
doing. Any trusty-code-tui implementation of R17/R18 MUST route through a
daemon RPC instead (per R12) — meaning these are **daemon API gaps to
specify**, not UI features to build directly, exactly as DOC-50 §2.1 C-2
already states for any such case. Flagging for Bob: is a daemon-side
`gh`/`glab`/spell-check proxy in scope at all, or are R17/R18 explicitly
descoped for trusty-code-tui because the daemon should never shell out to
per-user local tooling on the client's behalf?

## Summary counts

- MUST: R1–R12, R23, R25 (14)
- SHOULD: R13–R19 (7)
- MAY: R20–R22 (3)
- Flagged conflicts: R24 (1) — counted separately, not folded into the
  MUST/SHOULD/MAY totals above since it is a flag, not an accepted
  requirement. R23 and R25 were flagged conflicts too, until the 2026-09-06
  owner rulings above resolved each into a MUST.
