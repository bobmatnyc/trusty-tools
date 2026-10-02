Changed
- The agent roster and harness-understanding docs are embedded from the
  repo-root `content/` tree (ADR-0064) instead of `src/assets/`;
  `AGENT_ASSETS_DIR` now names `content/agents`. The consts, `AGENT_ASSETS`
  and the `harness_doc` accessors keep their names and types (#9011).
- The agent composer passes a `metadata:` map (block or flow form) through
  an `extends:` chain, per key child-wins, and emits it last, so a deployed
  agent keeps its `metadata.version`. `agent_schema` lists `metadata` as a
  composer key and no longer reads its indented `version:` child as the
  claude-mpm top-level `version:` marker (#9011).
