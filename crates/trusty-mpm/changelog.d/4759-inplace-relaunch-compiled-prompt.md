Fixed

- bare `tm` in a managed pane now refreshes the project's compiled prompt before relaunching, and refuses the relaunch if that write fails (refs [#4752](https://github.com/bobmatnyc/trusty-tools/issues/4752))
  - this path calls neither session preparation nor the daemon resume route, so it was the one way to start a session on a stale or missing compiled prompt where a fresh launch would have refused
  - all three pre-spawn paths now route through a single `refresh_compiled_prompt` entry point so they cannot drift apart
- an unwritable `.trusty-mpm/` no longer skips the MCP injectors (refs [#4752](https://github.com/bobmatnyc/trusty-tools/issues/4752))
  - the `last-instructions.md` stash write degraded the whole preparation to an early non-fatal error, so callers launched the session anyway with the `trusty-mpm`/`trusty-review` content-pinning defense against the #3918/#3950 name-squatting class never applied
  - the stash is an inspection artifact; it now logs and continues instead of short-circuiting
