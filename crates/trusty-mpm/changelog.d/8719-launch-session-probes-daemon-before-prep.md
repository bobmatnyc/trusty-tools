Fixed

- `DaemonClient::launch_session` (the TUI's `/connect` launch) now checks that
  the daemon answers `GET /health` before it runs session prep. An unreachable
  daemon fails the launch with nothing written: no agent deploy, no
  skill-source refresh, no `~/.claude.json` and no project
  `.claude/settings.json`. The session is registered only after prep and the
  `claude` line are ready, so a fatal prep failure no longer leaves a
  registered session with no tmux session behind it.
