Fixed

- Slack pairing now survives a gateway restart and can be completed without a REPL (closes [#4853](https://github.com/bobmatnyc/trusty-tools/issues/4853), closes [#4854](https://github.com/bobmatnyc/trusty-tools/issues/4854))
  - paired channels persist to `~/.trusty-agents/state/slack-paired.json`, loaded on boot and saved on pair — mirroring the Telegram precedent in #467
  - a direct message from a user already in the Slack RBAC table pairs itself, so the launchd-run gateway (which has no REPL and can never mint a pairing code) is reachable; shared channels and unknown users still require an explicit code
  - `/slack-start` now describes a flow the user can actually complete instead of pointing at a REPL that does not exist in `--slack` mode
