Fixed

- **`tcode run-task` reports turns, token usage and cost on the default daemon
  path (#8155).** The daemon path printed only the session snapshot — `agent`,
  `binding`, `created_at`, `id`, `mode`, `result`, `status`, `task`,
  `workstream_id` — so a run had no per-run cost unless the caller fell back to
  `--legacy-in-process`, and the ephemeral `tcode serve --stdio` child exited
  with the run, taking the in-memory session with it, so `tcode transcript
  <id>` could not recover the figures afterwards either. `run-task` now reads
  the run record the daemon already persisted (`session.get_transcript`, inside
  that same daemon's lifetime) and merges `turns`, `usage`, `cost_usd`, a new
  `usage_by_role` split and the full `transcript` into the document it prints;
  the human render gains the same figures as a footer. `cost_usd` prefers the
  provider's own authoritative per-turn cost and falls back to local pricing,
  and is `null` only when nothing priced. `--legacy-in-process` gains the same
  `turns` and `usage_by_role` keys, so one parser reads either path's output.
  A failed transcript read warns on stderr and leaves the pre-#8155 document
  rather than failing a successful run.
