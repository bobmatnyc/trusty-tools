Fixed

- `tm doctor` no longer treats an arbitrary working directory as a managed workspace. `run_doctor` applied the workspace path layout to whatever `project` the CLI sent, which pointed the operator-home skill tier at `<cwd>/.claude/skills` — the same path as the project tier — so the deploy-tier dedup dropped one of them, `~/.claude/skills` went unaudited, and project-tier findings were reported under the "operator home" label. The workspace layout now applies only to a directory a live session was provisioned into ([#5867](https://github.com/bobmatnyc/trusty-tools/issues/5867))
