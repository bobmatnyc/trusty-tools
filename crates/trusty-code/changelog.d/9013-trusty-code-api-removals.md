Removed
- `trusty_code::assets::DEFAULT_SKILLS` is now `DEFAULT_SKILL_NAMES`, a table of names only; the skill text is read from content.
- `trusty_code::assets::EmbeddedSkill` and `tm_agent_sources` are removed, and so is `REFERENCED_SKILL_FILES`; the roster and the skill files an agent points at are read from content.
- `EmbeddedAgent::Direct` no longer carries compiled-in text; it names the agent and its text is read from content `agents/trusty-code/`.
