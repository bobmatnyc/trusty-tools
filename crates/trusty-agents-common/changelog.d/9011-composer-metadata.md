Changed
- The agent composer passes a `metadata:` map (block or flow form) through
  an `extends:` chain, per key child-wins, and emits it last, so a deployed
  agent keeps its `metadata.version`. `agent_schema` lists `metadata` as a
  composer key and no longer reads its indented `version:` child as the
  claude-mpm top-level `version:` marker (#9011).
