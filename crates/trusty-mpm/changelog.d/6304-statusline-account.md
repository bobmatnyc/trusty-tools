Changed

- `tm statusline` no longer renders the daemon's TCP port
  ([#6304](https://github.com/bobmatnyc/trusty-tools/issues/6304)). The port was
  not actionable from a status bar and invited port-based connection attempts;
  its only other job was implicit, since it appeared only while the daemon was
  reachable. That one bit is kept as a bare `●` after the version, so the
  lead-in segment now reads `TM <ver> ●` when the daemon is up and `TM <ver>`
  when it is not.
- The statusline names the Claude Code account the session runs under, as
  `✻<email>` between the `@<gh>` and model segments (#6304). Claude Code's
  `statusLine` stdin payload carries session, cwd, model, cost, context-window
  and rate-limit fields but no account, so the email is read from
  `$CLAUDE_CONFIG_DIR/.claude.json` — the relocated personal tier every
  daemon-managed session runs under — falling back to `~/.claude.json`. A
  missing, unreadable, or malformed config, or one with no login recorded,
  omits the segment rather than rendering a placeholder. The read is bounded at
  100 ms on a detached thread, matching the sibling `gh` probe, so it can cost
  the segment but never the render.
