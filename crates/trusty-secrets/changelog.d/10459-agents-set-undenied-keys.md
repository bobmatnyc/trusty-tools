Changed
- An agent (a caller under Claude Code) may now `secrets.set` and locally `secrets.copy` a key in its project or owner scope that has no per-key deny; the deny is re-checked under the key index lock and a failed lookup refuses. Its `set` confirmation shows `[N chars]` only and its allow record carries `default_policy: true` (#10459, reverses #10386 Q2 for `set`).
- A per-key deny now refuses with the new wire kind `agent_key_denied` (-32085); `agent_use_refused` now means only an operator-only method (`delete`, removing a deny, a copy to a sync target) (#10459).
