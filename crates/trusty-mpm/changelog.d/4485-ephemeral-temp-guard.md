Fixed

- Managed hook commands and `statusLine.command` can no longer be persisted pointing at a binary in a system temp directory. A `cargo test --no-run` artifact in an agent harness's scratchpad (`/private/tmp/claude-<uid>/…/scratchpad/…`) passed the ephemeral-path guard, so `settings.json` was written with hook and statusline commands that ran a dead libtest harness on every hook event (#4485; #4492 is the same root cause reaching `statusLine.command`).
