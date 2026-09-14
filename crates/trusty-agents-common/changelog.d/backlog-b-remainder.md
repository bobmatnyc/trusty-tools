Documentation

- local-ops.md documents pinning `verify.sh` to a deployed SHA (`git show <sha>:path > scratchpad/verify.sh`) during a rollout, so an unpinned script run against a moving `main` no longer produces spurious failures (Refs #7565).
- code-critic.md requires quoting the project's own line-cap script output for a file-size finding instead of a hand-rolled `grep`/`wc` count (Refs #7819).
- svelte-engineer.md's Testing standard requires a stale-response race test for any component issuing an async fetch keyed on a selection, mocking only the API module boundary, citing `AssistantKnowledgePipeline.test.ts`/`KnowledgeProjectSync.test.ts` as the worked example (Refs #7334).
