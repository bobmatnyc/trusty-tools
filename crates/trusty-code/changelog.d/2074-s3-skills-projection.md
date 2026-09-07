Changed

- **An agent's frontmatter `skills:` list is no longer dropped when a `.md`
  agent is projected onto `AgentConfig` (#2074).** It now populates
  `SystemPrompt::append_skills`, which `agents.describe` reports as the agent's
  declared skills. The field was previously left empty because nothing consumed
  it; `agents.describe` is that consumer. Nothing injects those skills into a
  prompt yet — this is a reporting surface, not a runtime one.
