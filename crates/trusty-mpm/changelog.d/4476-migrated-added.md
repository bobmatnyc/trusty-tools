Added

- `tm doctor` gains an `agent_reachability` check that fails when the bundled roster is unreachable ([#4451](https://github.com/bobmatnyc/trusty-tools/issues/4451))
  - The existing `agents` and `deployment` checks are presence-only — they
    counted 42 files and diffed them against the canonical roster, and both
    reported green throughout the outage above. The new check asserts the one
    thing they cannot: that the settings tier the roster deploys into is a tier
    the managed spawn's `--setting-sources` flag actually loads. It reads both
    sides from production code, so moving the deploy destination without
    updating the flag (what happened here) is a hard `Fail`, not a silent
    regression.
- bundled `tm-slack-canvas-delivery` skill — codifies the Slack canvas delivery protocol (resolve destination, create with native `channel_id` binding to survive free-tier restrictions, post the link, verify the send) so canvas creation alone is never mistaken for delivery ([#4447](https://github.com/bobmatnyc/trusty-tools/issues/4447))
- `tm` CLI accepts unambiguous abbreviated subcommands, e.g. `tm doc` for `tm doctor` ([#4398](https://github.com/bobmatnyc/trusty-tools/issues/4398))
  - Turns on clap's `infer_subcommands` for the top-level `Cli`, propagating to every nested action enum. Exact matches still win over prefix inference, so ambiguous-prefix pairs (`hook`/`hooks`, `project`/`projects`, `session`/`sessions`, `status`/`statusline`) keep resolving to their own exact command.
