Added

- **Assistant turns stream token-by-token to attached clients (issue #4425,
  epic #3696 Gap B).** `ToolEventSink::agent_message` is now called repeatedly
  per text turn — once per content chunk with `done: false`, then once with
  `done: true` and an empty delta marking the bubble complete — instead of once
  with the whole turn. A `session.attach`ed client (and therefore `tcode tui`)
  renders the assistant's words as they are produced rather than as one paste
  when the turn ends. A tool-only turn still emits nothing. Streaming engages
  only when a sink is attached: the `run-task` CLI path and every scripted test
  keep taking the blocking call unchanged, so their wire request is
  byte-identical to before.
