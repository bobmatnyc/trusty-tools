Added

- **`agents::mpm_bridge` — read trusty-mpm's deployed `.claude/agents/*.md`
  artifacts with the SHARED frontmatter parser** (#4495). The crate already
  depended on `trusty_agents_common::agents::metadata::agent_metadata_from_str`
  — the product-agnostic reader wrapping the same `split_frontmatter` grammar
  `compose_agent` emits with — but used it only for compress/adapters/rbac/perf;
  no agent-loading code touched it. This module projects one already-flattened
  deploy artifact onto trusty-agents' own `AgentConfig`, mirroring trusty-code's
  `plugins::agents::load_plugin_agent` (#3539) so all three products read one
  grammar. Leaf-only by decision: a residual `extends:` is warned about and
  ignored (deploy artifacts are pre-flattened) and is not propagated to
  `AgentInfo::extends`. Two same-name/different-semantics traps are refused
  rather than mapped, each pinned by its own test: trusty-mpm's `skills:` is a
  co-deployment DEPENDENCY list and never becomes trusty-agents' `[skills].allow`
  PERMISSION GATE, and `tier` is never populated because it is derived
  (`AgentTier::for_kind`). `tools:` maps to `ToolsConfig.allowed` (exact-name)
  rather than `.allow` (globs, where a trailing `*` widens) — where the two
  readings differ the restrictive one wins. Every other frontmatter key is
  dropped with one aggregated warning naming it. Additive: nothing calls the
  module yet, and no agent's reachability changes.
