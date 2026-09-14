Fixed

- `tm validate --repair` resyncs toggle-driven hook groups into a live project.
  `validate_settings` recomputes the hook set the launch path would write for
  the project's CURRENT config — the base triad plus every toggle-driven
  addition — and reports both a group the config asks for that the file lacks
  and a tm-owned group the config no longer asks for. The repair merges in place
  through the same locked settings writer, so a `prompt_self_improvement` flag
  that flipped on after `.claude/settings.json` was written registers the
  `Stop`/`SubagentStop` capture without a session restart, and one that flipped
  off has its stale group removed (#7849).
- The resume/relaunch hook merge no longer strips the prompt-feedback capture it
  had just written: it hard-coded that flag off while its strip domain covered
  both events either way, so every resume removed the two groups (#7849).
- `tm doctor`'s `hooks_missing_tm_group` names a missing toggle-driven group and
  the `tm validate --repair` remedy, alongside the lifecycle events it already
  reported, and `tm doctor --fix` now closes that gap instead of leaving the
  check warning forever (#7849).
- A settings file that fails to parse, and a hook write the repair cannot
  perform, are reported as failures rather than as "no gaps found" (#7849).
