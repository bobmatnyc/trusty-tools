Fixed

- Managed session spawns now export `CLAUDE_CODE_ENABLE_TODO_TOOLS=1`, so `TodoWrite` and the `TaskCreate` family survive Claude Code's model gate on a model outside its allowed list (refs [#8066](https://github.com/bobmatnyc/trusty-tools/issues/8066))
- The bundled output styles' "TodoWrite Framework" section no longer assumes the tool exists; it names the prose task-list fallback for a harness that does not expose it (refs [#2799](https://github.com/bobmatnyc/trusty-tools/issues/2799))
