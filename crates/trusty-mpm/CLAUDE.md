# Project Instructions

<!-- trusty-mpm: created by `trusty-mpm session start` — customize for your project -->
<!-- trusty-mpm will never modify this file again after creating it. -->

## Project Context

<!-- Describe your project, tech stack, and any conventions the agent should know. -->

## Framework Instructions

This file is the ONLY surface for customizing the PM's instructions. To replace
a section of the framework prompt, put the replacement between a marker pair —
`<!-- TRUSTY-MPM: WORKFLOW START v=1 -->` on its own line, your text, then
`<!-- TRUSTY-MPM: WORKFLOW END -->` on its own line.

(Those markers are shown inline deliberately. A marker alone on a line IS the
mechanism, so a worked example here would take effect as a real override — see
`seeded_claude_md_declares_no_overrides`.)

Tokens: `IDENTITY`, `MEMORY`, `SEARCH`, `WORKFLOW`, `AGENT-DELEGATION`,
`ENFORCEMENT`, `NON-OVERRIDABLE-RULES`, `FRAMEWORK-GUARANTEED-CONVENTIONS`.
`CORE` is the one token that is always declined. Prose outside the markers is
project context — Claude Code loads it natively, so it is never copied into the
composed prompt.

The instructions actually delivered to this session are written to
`.trusty-mpm/last-instructions.md` on every launch. Read that file to see what
the PM received, including which of your overrides applied:

```
tm session instructions          # print it, with applied/declined markers on stderr
cat .trusty-mpm/last-instructions.md
```

## Preferences

<!-- Any agent behavior preferences specific to this project. -->
