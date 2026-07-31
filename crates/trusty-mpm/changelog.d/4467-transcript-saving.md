Fixed

- managed sessions no longer silently lose their Claude Code transcripts (closes [#4467](https://github.com/bobmatnyc/trusty-tools/issues/4467))
  - Every managed spawn now scrubs the process-local session markers Claude Code
    injects into its child processes. When `tm` was invoked from inside a Claude
    Code session it inherited `CLAUDE_CODE_CHILD_SESSION` and passed it through,
    which made the spawned `claude` turn session persistence OFF: no native
    `--resume`, no `--continue`, no `/rewind`, and the session never appeared in
    `--resume` listings. Since tm's own pause/resume writes only a condensed
    summary, a session that died lost everything since its last snapshot.
  - Scrubbed: `CLAUDE_CODE_CHILD_SESSION` (the sole transcript-saving trigger),
    `CLAUDE_CODE_SESSION_ID`, `CLAUDECODE`, `CLAUDE_PID`, `CLAUDE_EFFORT`,
    `CLAUDE_CODE_EXECPATH`. Deliberately NOT scrubbed: `CLAUDE_CONFIG_DIR` (tm
    relocates it on purpose — [#4455](https://github.com/bobmatnyc/trusty-tools/issues/4455)
    depends on it to keep the bundled roster in a settings tier managed sessions
    load), `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_MAX_OUTPUT_TOKENS` (an
    operator-facing knob) and `CLAUDE_CODE_ENTRYPOINT`.
  - Applied to every launch line, not just the default one: `spawn_command` and
    `resume_command` (a spawn-only fix would leave every resumed session broken),
    the in-place bare-`tm` relaunch, the headless stream-JSON backend, the
    `tm launch` / `tm connect` / agent-delegation launch line, the pane-relaunch
    line, and `tm run`.
  - `tm run` mattered most and was fixed last: it spawns `claude` with no tmux, so
    Claude Code's `tmux show-environment -g` escape hatch returns false
    immediately and the suppression fired there every time rather than depending
    on the tmux server's environment.
