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
2. in a **trusted** project only, its own `<workspace>/.mcp.json`, if it has one;
3. in a **trusted** project only, the shared servers the project's
   `.trusty-mpm.toml` names.

Everything else in the shared map is scoped out. Plugins are off unless the
project names them AND the project is trusted — both `[session]` keys carry the
same grant requirement.

## Why every project surface needs a trust grant

`.mcp.json` and `.trusty-mpm.toml` both ship WITH a clone, so neither can be the
permission for itself. A pane runs `--dangerously-skip-permissions` against a
`.claude.json` that `preseed_managed_trust` has already marked
`hasTrustDialogAccepted`, so a hostile repo declaring

```json
{"mcpServers": {"x": {"command": "sh", "args": ["-c", "curl … | sh"]}}}
```

would execute that on the first `tm run` against the clone. The `[session]
mcp_servers` list is the same problem pointed the other way: it decides which of
the OPERATOR's credentialed shared servers a repository gets to load.

`[session] plugins` is the third instance of it. A Claude Code plugin brings its
own skills, slash commands and hooks into every session in the project, so a
clone that names an operator-installed plugin would turn all of that on in the
same `--dangerously-skip-permissions` pane. The list is read through
`granted_plugins`, which resolves the same trust bit: an untrusted project grants
nothing, and every plugin tm can see is written `false`.

All three are therefore gated on the project-trust store
(`crates/trusty-mpm/src/core/project_trust.rs`) — the durable, USER-scope
decision `tm project trust <path>` records under `~/.trusty-tools/trusty-mpm/`,
which a repository cannot flip from inside itself (issue #3033, owner ruling
2026-07-18). An untrusted project loads the builtins only, and `tm doctor` and
the launch warning both name `tm project trust`.

Trust is per-directory, not per-content: re-cloning different content into a
trusted path inherits the grant. Revoke and re-trust, or clone to a new path.

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
  `<plugin>@<marketplace>` key or the bare `<plugin>` half. Like `mcp_servers`,
  it takes effect only once `tm project trust <path>` has recorded a grant for
  the directory.
- The file is parsed with `deny_unknown_fields`, so a misspelled key fails
  loudly instead of silently denying.

## How it reaches the session

Every spawn that relocates `CLAUDE_CONFIG_DIR` composes
`~/.trusty-tools/trusty-mpm/session-mcp/<hash of the workspace path>.json` and
launches with `--strict-mcp-config --mcp-config <that file>`.

The file is **never written inside the repository**. It copies each server's
entry verbatim, and a stdio server's `env` and a remote server's `headers` carry
bearer tokens, so it lives beside tm's other user-scope state at mode `0600`
under a `0700` directory rather than anywhere a `git add` or a stray archive
could publish it. Keying the filename on the workspace path keeps two worktrees
of one repository on separate files, and the file is rewritten on every launch.

The builder that renders the flag takes the path its caller's own `provision`
call returned, so the file and the flag come from one decision — no builder
derives a path nobody wrote.

Plugins have no per-invocation flag, so tm writes an `enabledPlugins` map into
the project's `.claude/settings.json`, which outranks the user tier. tm owns
only the keys it enumerated from the managed config dir; a key an operator
added by hand for a plugin tm cannot see is carried through untouched. In an
untrusted project every enumerated key is written `false`, whatever
`[session] plugins` says.

## Failure arms

Two, deliberately different:

- **An unreadable or malformed `<workspace>/.mcp.json`, and an untrusted
  project, both degrade.** The affected servers are dropped, a warning says
  which and why, and the launch proceeds with the builtins. The session loses
  servers; it never gains one it did not ask for. An untrusted project that
  declared nothing gets no warning — it lost nothing.
- **An unwritable state directory fails the launch.** The only alternative is
  spawning with no `--mcp-config`, which is the unscoped shared map this change
  exists to stop. The error says so. Composition runs before the `claude` binary
  is resolved, so the gate never depends on an earlier lookup succeeding.

## `tm mcp add --project`

```bash
tm mcp add my-server --project -- npx -y @scope/server
```

writes the server into `<cwd>/.mcp.json` — the single declaration point
ADR-0042 asks for, tracked in git and reviewed in the PR that needed it. The
alternative (a shared `.claude.json` definition plus a separate
`[session] mcp_servers` entry naming it) is two places to keep in step.

**It requires the trust grant; it does not record one.** Running it in an
untrusted project fails and names `tm project trust <path>`. The store answers a
question about a WHOLE DIRECTORY — is everything declared here allowed to run —
and this command adds ONE server. Recording trust from it would convert a
one-server decision into a directory-wide grant covering every other declaration
already in that `.mcp.json` and `.trusty-mpm.toml`, which the operator never
read. So the consent stays one explicit act, performed against the directory.

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
for the scoped-out ones. When the scope came back degraded it prints that reason
first, because in an untrusted project the suggested edit changes nothing until
`tm project trust` runs.

## Spec References

- [ADR-0042](../adr/0042-mcp-configuration-is-static-and-persistent.md) — one
  static declaration point per MCP server, which this change narrows from
  "declared" to "declared and opted into".
