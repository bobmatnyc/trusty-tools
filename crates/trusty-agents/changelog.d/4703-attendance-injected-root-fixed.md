Fixed

- Every attendance recording site now takes its root as an argument, closing a
  class of defect where a call site was untestable by construction
  (closes [#4703](https://github.com/bobmatnyc/trusty-tools/issues/4703)). Five
  production entry points — `POST /api/task` (`submit_task`) and the Slack and
  Telegram `handle_message` / `handle_command` handlers — recorded a human turn
  through a helper that resolved `$HOME` internally. The natural regression test
  for any of them ("call the handler, assert attendance was recorded") could
  therefore only write into the developer's real
  `~/.trusty-agents/attendance/<persona>.json`, with nothing local to assert
  against — so it passed whether or not the handler worked. PR #4695 hit exactly
  that: a first-draft test passed against known-broken code.
