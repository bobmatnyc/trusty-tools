Fixed
- BASE-AGENT's "Never Narrate a Wait" section again points at
  `condition-based-waiting` with the ``Read `<path>` `` form an agent acts on;
  #8075's rewrite left the path inside a prose sentence, which a Skill-less
  agent cannot follow and which reddened trusty-code's
  `embedded_agent_skill_pointers_open_with_read_file` (#8107).
- `rust-engineer.md`'s rename-a-test rule names the project's own doc-pointer
  lint rather than a gate script only this repository ships, restoring the
  deploy-anywhere property #7247 and #7270 established (#8107).
