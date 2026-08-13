Changed

- Every `gh` invocation now routes through `trusty_common::gh::GhCommand`
  ([#5475](https://github.com/bobmatnyc/trusty-tools/issues/5475)) — the
  `gh_tools` PR/CI tools, the `gh_cli` ticketing backend, the GitHub Actions
  client, and the workflow ticket manager. No behaviour change: `gh pr checks`
  still reports a non-zero exit as check STATE rather than a tool failure, and
  a missing `gh` still surfaces the `gh auth login` hint. The outcome mapping
  that used to be reachable only by spawning a real subprocess is now a pure
  function (`map_gh_outcome`), so the tolerance rule is pinned deterministically
  instead of via POSIX `true`/`false`.
