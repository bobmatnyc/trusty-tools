Changed

- The 42 bundled agent `.md` assets moved from `src/assets/agents/` to
  `trusty-agents-common`, and `core::bundle` now embeds them from
  `trusty_agents_common::agent_assets`. Every `pub const` in `core::bundle`
  keeps its name and content, so nothing downstream changes. Edit a bundled
  agent at `crates/trusty-agents-common/src/assets/agents/<name>.md` from now
  on; the whole roster moved together because `extends:` resolves within a
  single directory.
