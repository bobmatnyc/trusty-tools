Fixed

- a session whose instructions cannot be written no longer starts (refs [#4752](https://github.com/bobmatnyc/trusty-tools/issues/4752))
  - `build_instructions` returned a non-fatal error, so every launch path logged it and started the session anyway — with no instructions established and no `.mcp.json` written. Reachable whenever the project's `CLAUDE.md` or the framework instructions path cannot be read, for example when something has left a directory at `CLAUDE.md`
  - it is now the same fatal condition as the compiled-prompt write, reported as one error rather than two classes
- bare `tm` in a managed pane now refreshes the project's compiled prompt before relaunching, and refuses the relaunch if that write fails (refs [#4752](https://github.com/bobmatnyc/trusty-tools/issues/4752))
  - this path calls neither session preparation nor the daemon resume route, so it was the one way to start a session on a stale or missing compiled prompt where a fresh launch would have refused
  - the two resume-shaped paths now share one `refresh_compiled_prompt` entry point; session preparation keeps its own composition because it must apply the operator's resolved output style
- an unwritable `.trusty-mpm/` no longer causes a session to start without its instructions recorded (refs [#4752](https://github.com/bobmatnyc/trusty-tools/issues/4752))
  - the `last-instructions.md` stash write degraded the whole preparation to an early non-fatal error, so callers launched the session having skipped the fatal write below it
  - the stash is an inspection copy; it now logs and continues instead of short-circuiting
- the `CLAUDE.md` compiled-instructions pointer no longer names the retired global path (refs [#4752](https://github.com/bobmatnyc/trusty-tools/issues/4752))
  - it pointed at `~/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md`, which nothing writes since the compiled prompt became project-local; it is now the project-relative path, pinned against the pipeline that writes the file
