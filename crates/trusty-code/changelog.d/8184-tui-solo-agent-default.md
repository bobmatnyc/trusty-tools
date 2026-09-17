Changed

- **`tcode tui` runs the no-delegate solo agent by default (#8184).** An
  interactive session created with no explicit choice now runs #8031's
  single-agent shape: the top-level agent carries its own tcode tools
  (`read_file`, `write_file`, `edit`, `bash`, …) and `delegate_to_agent` is
  never registered, so "add a doc comment to fn X" results in the agent reading
  and editing the file itself instead of briefing a sub-agent that the previous
  PM registry — which had no filesystem tools at all — could not reach. The
  default lives on the daemon, in `session.create`, so every interactive client
  inherits it; `task.run`'s own mint path is unchanged and still delegates
  unless asked not to. A session's shape is fixed at creation and is
  authoritative for every later turn, and is reported on
  `session.status`/`session.list` as `no_delegate`.
- **PM/delegation mode is reachable by an explicit opt-in (#8184).**
  `session.create`'s new `delegate: true` param mints the pre-#8184 delegating
  PM, and `tcode tui --delegate` is the CLI surface that sends it. Note this
  changes the default for every `session.create` caller, including the GUI's
  `POST /sessions` bridge; a caller that wants delegation must now ask for it.
- **The TUI's connect line states which agent runs and where it edits
  (#8184).** It now reads `… (session <id>; solo agent (no delegation), editing
  in <root>)`, or `… projectless — editing in a scratch workspace` for a
  session with no `--project`, whose run works in an ephemeral scratch root
  rather than the directory the TUI was launched from. Every tool call the solo
  agent makes still goes through the #3422 permission prompt and the agent's own
  `tcode_tools`/`permissions:` gating.
