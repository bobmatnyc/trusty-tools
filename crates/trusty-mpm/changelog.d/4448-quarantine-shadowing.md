Fixed

- a stale project-tier agent file that shadows a bundled agent is now quarantined on session launch and on `tm sessions sync-assets`, instead of silently winning the resolution race forever (closes [#4448](https://github.com/bobmatnyc/trusty-tools/issues/4448))
  - `retract_framework_agents` (#4409) can only delete what its ownership ledger names, so a copy written before that ledger existed survived it, outranked the canonical user tier, and was never refreshed again — [#4408](https://github.com/bobmatnyc/trusty-tools/issues/4408) made permanent
  - the sweep runs AFTER retraction, against the workspace's OWN `.claude/agents` — never `fw.claude_agents_dir()`, which is the operator's real `~/.claude/agents` on the non-git `tm session start` and TUI `/connect` paths
  - it never moves a git-tracked file, a file in a repository git cannot read, a claude-mpm artifact, an operator-owned ledger entry, or anything hand-authored, and it never deletes: each moved file keeps a verified backup under `.trusty-mpm/agent-quarantine/` plus an inert `.md.disabled` sibling, and a receipt records how to restore it
  - the bundled-name roster moved to `core::bundled_roster`, so `tm doctor`'s `asset_tier` probe and the quarantine resolve it from one place and cannot drift on what counts as canonical
