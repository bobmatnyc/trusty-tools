Changed

- bundled agents deploy to the tm-managed user tier, never per-workspace and never to `~/.claude` (closes [#4409](https://github.com/bobmatnyc/trusty-tools/issues/4409))
  - `FrameworkPaths::agent_deploy_dir()` is the single destination for every
    bundled-agent deploy, validate, staleness, reset, and doctor call site:
    `$CLAUDE_CONFIG_DIR/agents` (`~/.trusty-tools/trusty-mpm/claude-config/agents`).
    Unlike `claude_agents_dir()` it is never rewritten project-local by
    `for_managed_project`/`for_managed_workspace`.
  - `tm install` no longer writes composed agents into the operator's generic
    `~/.claude/agents/` — the highest-severity breach on the issue, since that
    directory belongs to a Claude Code install with nothing to do with
    trusty-mpm.
  - Session launch, `tm sessions sync-assets`, and `tm catalog apply` no longer
    deploy bundled agents into a workspace's `.claude/agents/`. That directory
    is now reserved for hand-placed (and future project-custom) agents, which
    are still never touched.
  - Session launch and sync-assets additionally RETRACT the bundled agents an
    older binary deployed into a workspace. The project tier outranks the
    config-dir tier in agent resolution, so a stale copy left behind would
    shadow the canonical roster permanently and would no longer be refreshed by
    any deploy — the #4408 shadowing incident made permanent. Retraction removes
    only manifest-tracked, framework-owned files; hand-placed and user-owned
    files survive byte-identical, and a corrupt manifest aborts the retraction
    rather than guessing.
  - `tm doctor`'s `agents` and `agent_skills` probes, `tm agent list`/`tm agent
    show`, and the deployment-completeness gate follow the roster to the new
    tier. Probing the workspace tier after the flip would have reported every
    healthy install as broken and driven the spawn/resume gate into a permanent
    repair loop; `tm agent list` would have reported an empty roster.
  - `tm install --reset-agents --reset-agents-workspaces` now RETRACTS a
    workspace's bundled agents instead of force-recomposing them. Recomposing
    was correct while workspaces were a deploy destination; after the flip it
    re-created, inside live sessions' workspaces, exactly the shadow this change
    removes. The `--reset-agents <names>` scope is still honored, and removals
    are reported on their own line.
  - `tm repair deploy` follows the agent ledger to the new tier and actually
    removes the scratch files it reports. It previously derived a "base" path
    from each `*.tmp` orphan and re-derived the old fixed `<name>.tmp` from it,
    which never matches the per-process scratch names below — so it unlinked
    nothing while still printing every orphan as removed. It now unlinks the
    path it found and reports only what it actually deleted.
  - EVERY writer of the shared agent ledger — session launch, sync-assets,
    retraction, `tm install --reset-agents`, and `tm catalog apply --prune` —
    now performs its read-modify-write under the lock below. `reset_agents` and
    `prune_agents` were the two that ran unlocked against the shared directory,
    which is the highest-traffic race with a concurrent session launch.
  - The agent deploy ledger's read-modify-write is now serialised across
    processes by an advisory lock on a `.trusty-mpm-manifest.json.lock` sidecar,
    and every atomic write stages through a per-process, per-attempt temp name
    instead of a fixed `.tmp` sibling. Both were survivable while each workspace
    had its own deploy directory; against ONE machine-global directory shared by
    every concurrent session launch, sync-assets run, and `tm catalog apply`,
    the fixed temp name lets two writers publish torn JSON and the unlocked
    load-modify-write silently drops one writer's entries — after which the
    files those entries described are treated as untracked and frozen, which is
    #4408's failure shape reached by a race.
  - Known consequences of a machine-global agent tier, neither solved here:
    - A per-project `[agents] exclude` no longer removes an agent from a
      session's view — it only refrains from writing it, and a sibling project
      that selects the agent still puts it there.
    - `tm catalog apply --prune` now deletes from the one directory EVERY live
      session reads, mid-session, driven by the daemon-wide baseline manifest.
      Before the flip it pruned `~/.claude/agents`, which no managed session
      read, so the blast radius was effectively nil. `--prune` remains opt-in
      and still only removes manifest-tracked managed files, but an operator
      running it now affects every running session, not one workspace.
- **`core::doctor::CheckStatus` gained an `Unknown` variant (issues #4005,
  #4001).** This is a BREAKING change for any downstream exhaustive `match`
  over the enum (E0004) and needs a MINOR bump, not a patch — the same trap
  that forced the `trusty-analyze` 0.7.3 yank. Four in-workspace match sites
  (Slack + Telegram formatters, the `tm doctor` CLI renderer) gained an arm.
  `Unknown` ranks above `Warn` and below `Fail`, so a single indeterminate
  check can never leave the aggregate report reading healthy.
- bump to 1.2.3 ([#4229](https://github.com/bobmatnyc/trusty-tools/pull/4229)) ([`7c4f056`](https://github.com/bobmatnyc/trusty-tools/commit/7c4f05643508bd9ad04b9cf5ba77dead3cb4cfcb))
