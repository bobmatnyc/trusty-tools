Fixed

- Workflow mode no longer refuses to start when the process CWD has no
  `.trusty-agents/agents/`. The pre-flight check consulted the CWD alone while
  agent resolution walked the project → `$HOME` → bundled hierarchy, so the
  macOS `.app` sidecar (`cwd = /`) loaded its full agent roster and then bailed
  with ``no `.trusty-agents/agents/` found in /`` — every Implementation task
  launched from the GUI failed this way. The pre-flight now routes through
  `agents::registry::agent_search_paths`, the same resolver the registry uses,
  and its error lists every path that was searched instead of naming one.
- `--workflow <name>` resolves `<name>.json` hierarchically too. It previously
  read a single CWD-relative `.trusty-agents/workflows`, so widening only the
  pre-flight would have traded the bail for a `WorkflowNotFound` two steps
  later. Workflow resume resolves the same way.
- The bundled workflow definitions (`prescriptive.json`, `prescriptive-gpt.json`)
  now deploy to `~/.trusty-agents/workflows/` at startup, alongside the bundled
  agent roster. They previously existed only inside a checkout of this repo, so
  a `cargo install`ed `tagent` had no workflow definition on any search tier.
  The deploy reuses the agent roster's stamp/lock/refresh path rather than
  copying it.
