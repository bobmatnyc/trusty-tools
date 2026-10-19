# tm-cli-operations — managed auth and other operator commands

## 4. Managed Auth (`CLAUDE_CODE_OAUTH_TOKEN`)

Because managed sessions run under a relocated `CLAUDE_CONFIG_DIR`, a `/login`
inside a managed session can diverge from your default-dir login (the macOS
Keychain entry is keyed by a hash of the config dir) — producing a "login
successful" → "not logged in" loop (#2246). `tm auth` manages a tm-owned token
that bypasses the Keychain entirely.

```bash
claude setup-token | tm auth set-token   # store from stdin (safer — no shell history)
tm auth set-token --token <val>          # explicit (lands in history — avoid in shared shells)
tm auth status                           # presence of stored token + env vars + on-disk creds — NEVER the value
tm auth clear-token                       # remove it (roll back to ambient auth)
```

The token is written 0600 to `~/.trusty-tools/trusty-mpm/claude-code-oauth.token`
and injected by the daemon into managed sessions. `tm auth status` never prints
the secret — safe to include in a `tm doctor`-style report.

---

## 5. Other Operator Commands

```bash
tm install [--force]               # deploy bundled framework artifacts to ~/.trusty-mpm/framework/
tm register <owner/repo> [alias]   # map a repo alias → URL (no clone; GitHub assumed; alias defaults to owner-repo)
tm load <alias>                    # clone/refresh + deploy managed config
tm run <alias> [--task …]          # launch an interactive claude session for an alias
tm path <alias>                    # print the stable repo path (IDE-attach / cd)
tm update [<alias>]                # git pull --ff-only + re-deploy config (all loaded aliases if omitted)
tm rm <alias>                      # deregister + delete the alias's project dir (shared config untouched)
tm start | stop | restart | status # daemon lifecycle (see note below)
tm serve --stdio                   # the MCP stdio bridge wired into .mcp.json
```

🔴 **`tm start` and `tm restart` REFUSE when launchd owns the daemon** (issue
#4230). On a host with a registered daemon launchd unit they exit with an error
naming the right command instead of spawning a second, unsupervised daemon that
would seize the port and keep serving a stale binary. Use launchd there:

```bash
tm doctor            # read the exact recipe from `daemon_orphan` / the refusal message
tm doctor            # after restarting: `daemon_orphan` must be OK
```

Take the label from `tm doctor`'s own output rather than assuming
`com.trusty.mpm` — the refusal message and every `daemon_orphan` remediation name
the unit that is actually registered on this host, which is not always the
canonical label. The recipe has the shape
`launchctl kickstart -k gui/$(id -u)/<label>`.

`tm stop` still works, but launchd will NOT bring the daemon back on its own
(`KeepAlive.SuccessfulExit=false`) — use the `kickstart` above. To run an
unsupervised daemon deliberately, `tm daemon --force`.

`--root <path>` on the alias commands (and `tm mcp`) switches to a standalone
managed root via the precedence chain in §1 of `SKILL.md`.

---
