Removed

- The `cto-assistant` workspace crate is gone
  (closes [#3732](https://github.com/bobmatnyc/trusty-tools/issues/3732)).
  **No capability is lost.** The four CTO DB tools (`query_headcount`,
  `query_budget`, `query_risks`, `query_work_classification`) remain available
  to the `cto-assistant` agent through the declarative `cto-db` Python skill
  (`.trusty-agents/skills/cto-db/`) that landed in #4476 — the deleted crate was
  a Rust `ToolExecutor` adapter that no binary had wired in since PR #3310
  removed its only `install_plugins(...)` call site, so it shipped no reachable
  behaviour. It was `publish = false` and never released to crates.io, so
  nothing external can break.
- The agent itself is untouched: `cto-assistant` continues to live where every
  other assistant does, as a bundled agent package at
  `crates/trusty-agents/.trusty-agents/agents/cto-assistant/`. Per the owner's
  directive, it is an assistant, not a crate.
- `AgentPlugin` / `install_plugins(...)` are unaffected and still supported for
  an out-of-workspace private launcher. In-workspace, the only remaining
  producer is `crate::tools::python_skill`, which registers *declared* skills —
  so no agent's tools require Rust any more.
