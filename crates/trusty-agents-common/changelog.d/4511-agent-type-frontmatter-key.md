Added

- **`agents::metadata::AgentMetadata::agent_type` — the claude-mpm-format
  spelling of an agent's declared domain** (for
  [#4511](https://github.com/bobmatnyc/trusty-tools/issues/4511)). A deployed
  `.claude/agents/*.md` artifact that originated from claude-mpm declares
  `agent_type:` and no `role:` at all, so a consumer reading one through this
  read-only projection saw no domain whatsoever and had to fall back to a
  fail-closed default. `split_frontmatter` now parses the key (with the same
  unescape treatment as `role:`) and projects it alongside `role`, so one
  reader answers "what domain does this file declare?" for both artifact
  dialects instead of each consumer hand-rolling a second scan. The two
  spellings stay independent fields — which one wins is the CONSUMER's
  reviewed policy, and the value is a DECLARATION that must be translated
  before it reaches any authorization decision, never used verbatim
- Compose output is byte-identical: `agent_type:` is deliberately NOT merged
  across an `extends` chain nor re-emitted by `merge_frontmatter`, because
  this composer canonicalises on `role:` and emitting a second domain key
  would change the bytes of every deployed artifact for a value nothing in the
  compose path consumes. Dropping it on emit is exactly what happened before
  the field existed; `agent_type_is_parsed_but_never_emitted` pins it
