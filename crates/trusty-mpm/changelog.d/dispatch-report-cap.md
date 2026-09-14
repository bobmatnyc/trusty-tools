Added

- `tm hook --prompt-feedback` now measures a stopping subagent's final assistant message against the BASE-AGENT hand-back word cap — 300 prose words clean, 600 when the report names a failure — and on an overrun writes one line naming the agent type, the word count and the cap to stderr, and records the same line to the prompt-feedback ledger. Words inside fenced blocks are not counted, so raw gate output stays free. Warn-only and fail-open: it never blocks, rewrites, or fails the hook (owner ruling 2026-09-14).
