Security
- A pane write (`inject`, so the Slack inbound answer, `session_send`, the idle nudge and task injection) now types nothing into a pane whose harness has exited or cannot be confirmed; it fails with `no_live_harness:` or `harness_unconfirmed:` instead of reaching a shell prompt. Ctrl-C stays ungated ([#10127](https://github.com/bobmatnyc/trusty-tools/issues/10127))
- A channel answer's framed line JSON-escapes the dollar sign and the backtick, so an answer that reaches a shell substitutes and runs nothing ([#10127](https://github.com/bobmatnyc/trusty-tools/issues/10127))
