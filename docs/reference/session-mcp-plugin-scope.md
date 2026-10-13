# Session MCP and plugin scope

> **Superseded in part, 2026-09-14 ([#7892](https://github.com/bobmatnyc/trusty-tools/issues/7892)).**
> The MCP-server half of the 2026-09-11 default-deny ruling
> ([#7422](https://github.com/bobmatnyc/trusty-tools/issues/7422), PR #7468) is
> withdrawn: tm follows the Claude Code standard, and no MCP server is scoped
> out of a session any more. The PLUGIN half of #7422 stands unchanged. This
> page describes the state after #7892.

## MCP servers — the Claude Code standard

A tm-managed session points `CLAUDE_CONFIG_DIR` at one protected directory
(ADR-0042) whose `.claude.json` holds the operator's user-scope `mcpServers`
map. That map has **standard Claude Code user-scope semantics**: every entry in
it loads in every tm session, in every project, with no grant. A server added
with `claude mcp add --scope user` under that `CLAUDE_CONFIG_DIR` — or with
`tm mcp add` — appears in the next session's `/mcp` with no further step.

A session therefore loads:

1. every user-scope server in the protected `.claude.json`;
2. tm's trusty-* builtins — `trusty-memory`, `trusty-mpm`, `trusty-review`,
   `trusty-search` — added **on top**, through the file tm passes with
   `--mcp-config`;
3. the project's own `<workspace>/.mcp.json`, subject to Claude Code's own
   approval.

**`--strict-mcp-config` is not passed.** That flag makes Claude Code ignore
every MCP source except the `--mcp-config` file, and it is what made #7422's
default-deny possible. Dropping it is what restores the standard: `--mcp-config`
alone is additive, and `--setting-sources user,project,local` (already on every
relocated spawn) is what makes the protected dir's map count as user scope.

### What tm composes, and where

Every spawn that relocates `CLAUDE_CONFIG_DIR` writes
`~/.trusty-tools/trusty-mpm/session-mcp/<hash of the workspace path>.json` and
passes it with `--mcp-config`. It holds **the four builtins and nothing else**,
so it is identical for every project and cannot vary with trust state,
`.mcp.json` content, or `.trusty-mpm.toml`. It is never written inside the
repository: it lives at mode `0600` under a `0700` directory beside tm's other
user-scope state. Keying the filename on the workspace path keeps two worktrees
of one repository on separate files, and the file is rewritten on every launch.

The builder that renders the flag takes the path its caller's own `provision`
call returned, so the file and the flag come from one decision.

### A project's `.mcp.json`

Claude Code approves project-scope servers itself, through
`enableAllProjectMcpServers` or `enabledMcpjsonServers` in settings, or through
its own prompt. tm neither pre-approves nor suppresses them; it only reports the
state (`core::project_mcp_approval`).

**A non-interactive pane cannot show that prompt.** tm's unattended launches
pass `--dangerously-skip-permissions`, so in those sessions an entry that no
settings tier names connects unasked rather than waiting. That is the posture
every `claude -p`, Agent SDK and CI run already has; `tm mcp list` labels such
an entry `unapproved` rather than implying an approval step that cannot fire.

`tm mcp add <name> --project -- <command>` writes the entry for you, with no
trust grant required.

### Failure arms

- **A protected `.claude.json` that cannot be read or parsed fails OPEN.**
  Nothing in the composition depends on that read any more, so the launch
  proceeds with the builtins and prints one warning line naming the file. The
  file is never quarantined — it also holds OAuth state.
- **An unwritable state directory fails the launch.** Without the composed file
  the session silently loses tm's own builtins, which is the one guarantee this
  mechanism exists to make. Composition runs before the `claude` binary is
  resolved, so the gate never depends on an earlier lookup succeeding.

### Owner launch config (#8988, #8992)

An operator-local file `<repos_root>/<owner>/.trusty-mpm-owner.toml` subtracts
servers, skills and tools from every session under that owner. The owner is
the first path segment under the repos root, else the owner the `origin`
remote names.

```toml
extends = "bob-duetto"          # optional, one parent, at most 4 files
[mcp]
allow = ["apex"]                # user-scope servers that load; absent = all
deny  = ["heygen"]              # applied after allow
claude_ai_connectors = false    # turn the claude.ai connectors off
[skills]
off = ["xlsx", "breeze:voice"]  # exact skill names; there is no `on`
[tools]
deny = ["NotebookEdit", "mcp__heygen"]  # bare tool names only
```

Any other table, `[agents]` included, is an unknown key and so a bad file.

- **Carrier.** Each launch writes
  `~/.trusty-tools/trusty-mpm/session-settings/<sha256 of the body>.json`
  (mode `0600`) and passes it as `--settings <file>`. The name is the body's
  hash, so a file is never rewritten with other contents: launches with
  different policies get different files and identical policies share one.
  The directory is bounded by the number of distinct policies and nothing
  deletes a file (another live session may use it). It holds only
  subtractive keys: `deniedMcpServers`, `disableClaudeAiConnectors: true`,
  `skillOverrides` with every value `"off"`, and `permissions.deny`. It never
  holds `permissions.allow`, `ask` or an `"on"`. Claude Code unions deny lists
  across scopes, so an operator or managed deny survives, and the PM's own
  `--disallowedTools Workflow,DesignSync` is untouched. Nothing is written into
  a repository or into any `settings.json`.
- **Deny set.** Every user-scope server in the `.claude.json` the launch
  reads (the managed config dir when relocated, else `$CLAUDE_CONFIG_DIR` or
  `~`) that `allow` does not name, plus every `deny` name. The trusty-*
  builtins and `claude-in-chrome` are never named; a `deny` that names one is a
  bad file. Names match exactly and case-sensitively, as Claude Code matches
  `serverName`.
- **`[skills] off`.** Each name is written as `"off"` in `skillOverrides`,
  which removes the skill from the listing. Exact names only: no wildcard;
  ASCII letters, digits and `. _ : -` (`plugin:skill` is allowed). An owner can
  only turn skills off. The `--settings` key beats the project tier's
  `skillOverrides` (#7751), so an owner `off` hides a skill the project lists
  `"on"`, and nothing here can re-list a skill the project tier turned off.
  The owner off is re-sent at every launch and ends when the file is deleted;
  the project tier's off is sticky. `extends` unions the lists, so a child
  cannot remove a parent's entry. The doctor row warns on a name that matches
  no skill under the managed config dir's `skills/` or the project's
  `.claude/skills/`.
- **`[tools] deny`.** Each name goes into `permissions.deny`: a built-in tool
  name, `mcp__<server>` or `mcp__<server>__<tool>`. A parenthesised rule
  (`Bash(rm:*)`, `Agent(engineer)`) or a wildcard is a bad file. `extends`
  unions the lists.
- **Floors.** Compiled in, in `core/launch_policy/floor.rs`; no file changes
  them, and naming one is a bad file. MCP: the trusty-* builtins and
  `claude-in-chrome`. Tools: `Agent`, `Task`, `SendMessage`, `Skill`,
  `ToolSearch`, `TodoWrite`, `Read`, `Write`, `Edit`, `Bash`, `BashOutput`,
  `KillShell`, `Grep`, `Glob`, `WebSearch`, `WebFetch` and the `Cron*` tools,
  plus any `mcp__<floor server>` rule. Skills: every `tm-*` skill and every
  skill an agent preloads or a prompt loads by name. Tool and skill floors
  compare case-insensitively. Drift tests fail when an agent grant or a
  prompt `Skill()` load falls outside a floor. A last-good record that names a
  floor entry added later has it dropped.
- **Architect launches.** An Architect (supervisor) launch gets the `[mcp]`
  part exactly as a PM does, and neither `[skills]` nor `[tools]`: it keeps
  every skill and tool, as the PM tool trim already leaves it every tool. The
  validator and floors still run for it, because the file is shared.
- **No policy, no change.** No owner, no file, or a policy that subtracts
  nothing adds no flag and writes no file.
- **Fail open.** A bad file uses the last good policy (or none) with a loud
  warning and an `owner_config` doctor `Fail`. An unreadable `.claude.json`
  keeps the explicit denies and every skill and tool part, which never read
  it. A last-good record from before #8992 still loads. A settings file that
  cannot be written launches
  without `--settings` and prints that the policy was NOT applied. Deleting the
  file turns the layer off and retires its last-good record at the next
  launch. None of these refuses a launch or resume.

## Plugins — still default-deny

Claude Code has no per-project plugin approval. `enabledPlugins` is
settings-only, and a plugin brings its own skills, slash commands and hooks into
every session in the project — inside a pane running
`--dangerously-skip-permissions`. There is no native standard to defer to, so
the #7422 gate stays exactly as it was.

`<workspace>/.trusty-mpm.toml` declares the allowlist. An absent key denies.

```toml
[session]
plugins = ["aws-core"]
```

`plugins` names Claude Code plugins, either as the full
`<plugin>@<marketplace>` key or the bare `<plugin>` half. The list is read
through `granted_plugins`, which resolves the project-trust store
(`crates/trusty-mpm/src/core/project_trust.rs`) — the durable, USER-scope
decision `tm project trust --dir <path>` records under `~/.trusty-tools/trusty-mpm/`,
which a repository cannot flip from inside itself (issue #3033, owner ruling
2026-07-18). An untrusted project grants nothing, and every plugin tm can see is
written `false`.

Trust is per-directory, not per-content: re-cloning different content into a
trusted path inherits the grant. Revoke and re-trust, or clone to a new path.

Plugins have no per-invocation flag, so tm writes an `enabledPlugins` map into
the project's `.claude/settings.json`, which outranks the user tier. tm owns
only the keys it enumerated from the managed config dir; a key an operator added
by hand for a plugin tm cannot see is carried through untouched.

`[session] mcp_servers` is **retired** (#7892) — parsed so an existing file
still loads, never read. The `[session]` table rejects unknown fields, so the
key had to stay in the schema.

## Diagnostics

`tm mcp list` prints the user-scope table, the builtins tm adds on top, and each
`.mcp.json` entry with its Claude Code approval state (`approved`, `refused`,
`unapproved`). It no longer marks rows `scoped-out` or suggests a
`[session] mcp_servers` block, because there is nothing left to opt into.

`tm doctor`'s `session_scope` check reports the same inventory, plus the plugin
half: which installed plugins the project does not load, and whether the
project's `.claude/settings.json` already carries the `enabledPlugins` map a
launch would write (issue #7678). That settings comparison is what makes the
plugin line trustworthy — the write happens ONCE, at launch, so a project whose
session was paused before it existed kept loading every user-tier plugin while
the check reported them as "NOT loaded". The check stays read-only and never
fails doctor; `tm doctor --fix --yes` re-applies both writes — the project-tier
`enabledPlugins` map and the composed `session-mcp/<key>.json` file — through
the same functions the launch path calls.

A rewritten `settings.json` does not reach a session that is already running:
Claude Code reads plugin enablement at startup. The repair makes the NEXT
session correct.

`tm session instructions` prints the excluded PLUGIN set on stderr, beside the
composed prompt it writes to stdout.

`tm mcp share` and `tm mcp unshare` are retired to notices, and
`tm mcp add --share-with-projects` is accepted and ignored. The subcommands and
the flag remain so scripts that spell them keep exiting zero.

## Spec References

- [ADR-0042](../adr/0042-mcp-configuration-is-static-and-persistent.md) — one
  static declaration point per MCP server, which #7422 narrowed from "declared"
  to "declared and opted into" and #7892 restores.
