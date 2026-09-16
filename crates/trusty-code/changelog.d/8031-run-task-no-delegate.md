Added

- `tcode run-task --no-delegate` and the matching `task.run` `no_delegate` request field run the named agent alone — `delegate_to_agent` is not registered, so it cannot hand the task to `python-engineer` ([#8031](https://github.com/bobmatnyc/trusty-tools/issues/8031))
  - the named agent gets its OWN tcode tools instead (`read_file`, `write_file`, `edit`, `bash`, `glob`, `grep`, …) — previously those existed only on a delegated agent's per-delegation registry, so a non-delegating run had nothing to do the work with
  - the tool set is built by the same `ProjectToolFactory` and narrowed by the same `tools.allowed` gate a delegated run of that agent goes through, and the run's `permissions:` map decides each call as before
  - tool events for those calls are attributed to the named agent
  - applies to both execution paths: the default thin-client/daemon path and `--legacy-in-process`
  - omitting the flag or the field leaves delegation registered exactly as before
