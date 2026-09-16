Added

- `tcode run-task --no-delegate` and the matching `task.run` `no_delegate` request field run the named agent alone — `delegate_to_agent` is not registered, so it cannot hand the task to `python-engineer` ([#8031](https://github.com/bobmatnyc/trusty-tools/issues/8031))
  - applies to both execution paths: the default thin-client/daemon path and `--legacy-in-process`
  - omitting the flag or the field leaves delegation registered exactly as before
