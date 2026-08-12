Fixed

- Regenerated the three committed PM-prompt golden fixtures
  (`pm-prompt-bundled-fallback.md`, `pm-prompt-claude-md-override.md`,
  `pm-prompt-roster-absent.md`) that went stale when #5536 updated the
  `search_health` liveness doc text without updating the snapshots, which left
  `golden_bundled_fallback_prompt`, `golden_claude_md_override_prompt`, and
  `golden_roster_absent_assembly_prompt` failing on every run.
