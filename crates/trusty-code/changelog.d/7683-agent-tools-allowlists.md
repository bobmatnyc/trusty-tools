Changed

- The four forked agents and the four tcode-only defaults declare their tool
  allowlist under `tcode_tools:`, and the `.md` loader reads that key instead of
  `tools:`. The shared roster's `tools:` now carries Claude Code's vocabulary
  (`Read`, `Bash`, `mcp__trusty-search`), which intersects this runtime's
  registry at zero tools — honouring it here would have left every embedded
  roster agent unable to call anything, `finish_task` included (#7683).
