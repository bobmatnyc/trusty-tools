Removed

- **Ticketing is gone from the skills surface entirely** — it is reachable as a
  sub-agent only (owner, 2026-07-29). The grouped `Ticketing` skill card, its
  twelve leaf ticket/CI skill cards, and `cto-assistant`'s four direct
  ticket-tool grants (`ticket_search`, `list_tickets`, `get_ticket`,
  `create_ticket`) are all removed. The ticketing TOOLS are unchanged and still
  granted to `ticketing-agent`, and the authored `ticketing-epic` /
  `ticketing-ticket` skill assets — that sub-agent's own domain knowledge — are
  preserved and still injected into its system prompt.
- **Removed the unused `ompm` `[[bin]]` target.** It was a thin HTTP client
  shim never invoked by any current workflow, CI job, or script; removed
  ahead of trusty-agents' first crates.io publish so the publish doesn't
  lock in a binary with no reason to exist (#4056).
- **The Tools tab's glob textarea, and with it GUI editing of
  `[tools].allow` (#3932):** capability is now shown as skills, and editing
  returns as `PATCH { skills_allow }` over skill names rather than over the
  globs beneath them (DOC-57 §5.7). Until then `agent.toml` is the edit surface,
  which the Skills pane says outright.
- **The Listeners pane's hardcoded "not bound" badge (#3932):** it asserted a
  per-agent binding state for every agent regardless of configuration, inventing
  a listener that may not exist and hiding one that is quietly failing. The pane
  now labels itself as connector scaffolding until `GET /api/agents/:name/listeners`
  lands (#3937). The Personality pane's fabricated per-connector-instructions
  list is gone for the same reason — it enumerated connectors it had never
  looked at.
