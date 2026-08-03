Fixed

- **`AgentRegistry::load` now reads the `.claude/agents` tier with trusty-mpm's
  own frontmatter grammar** (#4496). That tier has been a first-class roster
  source for some time, but its `.md` files were parsed by `parse_md_agent` —
  trusty-agents' OWN overlay reader — even though they hold trusty-mpm's
  DEPLOYED artifacts. The two schemas disagree in a way that loses agents
  silently: trusty-mpm's `tools:` is a flat list of tool names, while
  `MdAgentFrontmatter::tools` is a nested `ToolsConfig` map, so a `serde_yaml`
  deserialize of the former into the latter hard-errors and `load` warns and
  skips the entire file — the agent disappears from the roster with only a log
  line to show for it. `.claude/agents` (project-local and `$HOME`) now routes
  through `agents::mpm_bridge` (#4495); `.trusty-agents/agents` and
  `<config_dir>/agents` keep `parse_md_agent`, which remains the reader for
  trusty-agents' own overlay schema (`capabilities:`, `runner:`,
  `display_name:`, live `extends:`). Neither reader is a superset of the other,
  so this is a per-tier selection rather than a merge.
- Name-collision precedence is now pinned by tests: a hand-authored or bundled
  `.trusty-agents/agents/*` agent still wins over a trusty-mpm-sourced agent of
  the same name. `agent_search_paths` already ordered the tiers that way and
  `load` is first-occurrence-wins, so this is preserved, not introduced — but
  now that the two tiers use different parsers, a silent ordering inversion
  would also silently change which schema an operator's file is read with.
- No reachability change: `ASSISTANT_REACHABLE_SUBAGENTS`, every persona's
  `[subagents].delegate_allowed`, and `SubagentAllowSet` are untouched, and
  every agent projected from this tier carries `SubagentsConfig::default()`
  (no delegation targets granted). Catalog population cannot make anything
  reachable from an assistant.
