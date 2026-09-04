Fixed
- A managed relaunch no longer emits a bare `claude --continue`. Both relaunch
  paths — the tmux-pane resume and the in-place `tm` exec — now resolve the
  target explicitly: `--resume <id>` when the session record carries a
  `claude_session_id` that exists in that session's OWN store, otherwise a fresh
  launch. The eligibility check behind the old `--continue` branch read
  `~/.claude/projects`, the operator's store, while the command it built exported
  the managed `CLAUDE_CONFIG_DIR`; the check therefore answered `true` on any
  machine with transcripts and `--continue` resolved "most recent conversation"
  against the managed store instead. When that store's newest conversation was a
  still-live `claude agents` background job, Claude Code refused to double-attach,
  printed "Your most recent conversation is running in the background", and exited
  0 — leaving the pane at a bare shell
  ([#6765](https://github.com/bobmatnyc/trusty-tools/issues/6765)).
- `has_prior_conversation` / `has_prior_conversation_in` are deleted: with no
  `--continue` branch they had no caller. `session_id_exists`, which already
  resolves the store through `projects_dir_for(config_dir)`, is now the only
  conversation-store lookup either relaunch path makes.
