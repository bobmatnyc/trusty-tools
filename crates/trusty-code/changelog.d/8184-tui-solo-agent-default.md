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
  unless asked not to.
- **The stock `pm` agent now asks before it writes or runs shell (#8184,
  #3422).** `assets/agents/pm.md` gained a `permissions:` block marking `bash`,
  `write_file`, `write_files` and `edit` as `ask`; reads stay unprompted. This
  is what gates the new default: before it, the shipped card declared no
  `permissions:` at all and an agent with no map is allowed everything, so a
  solo session would have written and run shell in the user's project root with
  no prompt. An `ask` needs a client watching that session to answer it — a
  headless run (no attached client) resolves it to deny per #8100, so a
  non-interactive `tcode run-task pm --no-delegate` now needs
  `TCODE_PERMISSION_MODE=allow-asks`. Only the four tools above gate; the
  agent's own `tcode_tools` allowlist still decides what exists at all.
- **A deployed roster card keeps the `permissions:` block its source declared
  (#8184).** The roster deployer re-emits frontmatter from a fixed key list
  that does not include `permissions:`, so the materialized
  `<project>/.trusty-code/agents/<name>.md` — which wins agent resolution over
  the embedded copy — silently lost the block, and an agent with no block is
  allowed everything. `md_loader` now restores the embedded source's block when
  a file the deployer wrote (`provenance: framework-owned`) carries none of its
  own; a hand-authored card, or a deployed one a user has since given rules, is
  left alone. The shared emitter still drops the key — that is worth fixing at
  the source, in `trusty_agents_common::agents::builder`.
- **PM/delegation mode is reachable by an explicit opt-in (#8184).**
  `session.create`'s new `delegate: true` param mints the pre-#8184 delegating
  PM, `POST /sessions` forwards it, and `tcode tui --delegate` is the CLI
  surface that sends it. `TcodeConnector::create_session` — which provisions
  fleet/automation sessions with no client to answer a prompt — sends
  `delegate: true` itself, so programmatic provisioning keeps its pre-#8184
  semantics rather than inheriting a default aimed at a human at a terminal.
- **A session's agent shape is immutable, like its project binding (#8184).**
  `task.run` with `no_delegate: true` against a session minted delegating is
  rejected with `-32003 invalid_argument` instead of running a shape
  `session.status`/`session.list` would keep reporting as the other one — the
  same record-vs-run divergence the sibling `project` and `workstream_id`
  guards prevent. The shape is reported as `no_delegate` on both methods.
- **The TUI holds a prompter claim from `setup`, before any run (#8184).** The
  daemon suspends a tool call on an `ask` rule only while someone is watching
  that session (#8100), and on this transport the only thing that claims a
  prompter is an open `session.events` stream. `run_chat_turn` issued
  `task.run` first and opened its stream afterwards, so the interactive
  default's first gated `write_file`/`bash` raced the stream open and a call
  that lost was refused with no prompt to answer — and every reconnect gap
  reopened the same hole. `setup` now opens a dedicated claim stream and
  confirms it before returning, held for the TUI's life alongside the per-turn
  stream, so a gap needs both down at once. The claim is confirmed, not
  assumed: a daemon that refuses or closes the stream fails `setup` rather than
  starting a session nothing is watching, the wait is bounded by the ordinary
  15-second call budget, and the background reconnect gives up after the same
  five attempts the per-turn tail uses instead of re-dialling a dead daemon for
  the life of the process.
- **The TUI's connect line states which agent runs (#8184).** It names the
  session's shape — `solo agent (no delegation)` or `delegating PM` — so the
  interactive default is visible at launch. #8164 then settled the line's
  wording, `… — home <root>, workstream <ws>, <shape>`; a session with no
  `--project` reads `home projectless`, and its run works in an ephemeral
  scratch root rather than the directory the TUI was launched from.
