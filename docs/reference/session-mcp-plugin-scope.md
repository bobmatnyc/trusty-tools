# Session MCP and plugin scope (default-deny)

Issue [#7422](https://github.com/bobmatnyc/trusty-tools/issues/7422). Owner
ruling 2026-09-11: **default-deny**.

## What changed

A tm-managed session points `CLAUDE_CONFIG_DIR` at one shared directory
(ADR-0042), and Claude Code connects every entry in that directory's
`.claude.json` `mcpServers` map with no approval. One `tm mcp add` for one
project therefore loaded that server — and its whole tool catalog — into every
session on the host. Installed plugins did the same through the managed
`settings.json` `enabledPlugins` map.

Since #7422 a session loads:

1. the trusty-* framework builtins — `trusty-memory`, `trusty-mpm`,
   `trusty-review`, `trusty-search` — always;
2. the project's own `<workspace>/.mcp.json`, if it has one;
3. the shared servers the project's `.trusty-mpm.toml` names.

Everything else in the shared map is scoped out. Plugins are off unless the
project names them.

## The opt-in keys

`.trusty-mpm.toml` at the project root — the committed, project-level config
(`crates/trusty-mpm/src/core/project_config.rs`). Both keys are ALLOWLISTS: an
absent key denies.

```toml
[session]
mcp_servers = ["slack-mcp", "gworkspace-mcp"]
plugins = ["aws-core"]
```

- `mcp_servers` names keys of the managed `.claude.json` `mcpServers` map.
  Naming a server that map does not declare is a no-op — the allowlist grants
  access to a declaration, it does not create one.
- `plugins` names Claude Code plugins, either as the full
  `<plugin>@<marketplace>` key or the bare `<plugin>` half.
- The file is parsed with `deny_unknown_fields`, so a misspelled key fails
  loudly instead of silently denying.

## How it reaches the session

Every spawn that relocates `CLAUDE_CONFIG_DIR` composes
`<workspace>/.trusty-mpm/session-mcp.json` and launches with
`--strict-mcp-config --mcp-config <that file>`. The file is rewritten on every
launch, lives in the session's own state directory — so two worktrees of one
repository never share one — and is gitignored by tm's scaffolded block.

Plugins have no per-invocation flag, so tm writes an `enabledPlugins` map into
the project's `.claude/settings.json`, which outranks the user tier. tm owns
only the keys it enumerated from the managed config dir; a key an operator
added by hand for a plugin tm cannot see is carried through untouched.

## Failure arms

Two, deliberately different:

- **An unreadable or malformed `<workspace>/.mcp.json` degrades.** The
  project's own servers are dropped, a warning names the file, and the launch
  proceeds with the builtins plus the opt-ins. The session loses servers; it
  never gains one it did not ask for.
- **An unwritable state directory fails the launch.** The only alternative is
  spawning with no `--mcp-config`, which is the unscoped shared map this change
  exists to stop. The error says so.

## `tm mcp add --project`

```bash
tm mcp add my-server --project -- npx -y @scope/server
```

writes the server into `<cwd>/.mcp.json`. A session loads a project's own
`.mcp.json` unconditionally, so this is both the declaration and the
permission — one line, tracked in git, reviewed in the PR that needed it. That
is the single declaration point ADR-0042 asks for; the alternative (a shared
`.claude.json` definition plus a separate `[session] mcp_servers` entry naming
it) is two places to keep in step.

Without `--project` the behaviour is unchanged: the server goes to the shared
user scope, where it loads only in projects that opt into it. Use that for a
server whose definition genuinely belongs to the operator's machine — a
secret-bearing remote, a path only that host has.

## Migration

Run `tm doctor` in each project. Its `session_scope` check names every shared
MCP server and installed plugin that project's sessions have stopped loading,
and the exact keys that put one back. It is informational and never fails
doctor — an excluded server is the designed outcome, not a fault.

`tm session instructions` prints the same excluded set on stderr, beside the
composed prompt it writes to stdout.

`tm mcp list` marks each shared server `opted-in` or `scoped-out` for the
current directory, and prints a ready-to-paste `[session] mcp_servers` block
for the scoped-out ones.

## Spec References

- [ADR-0042](../adr/0042-mcp-configuration-is-static-and-persistent.md) — one
  static declaration point per MCP server, which this change narrows from
  "declared" to "declared and opted into".
