Fixed

- `[skills].allow` (dispatch-time tool grants, `agent_skills.rs`) and `[system_prompt].skills` (prompt-text injection, `config.rs::SystemPrompt::skills`) doc comments now cross-reference each other and state that the two combine additively, with neither overriding the other (#7904).
