# The `~/.trusty-tools/<crate>/config.yaml` convention

This is the cross-crate configuration standard for the trusty-tools workspace.
It gives every trusty-* crate **one** canonical place to read and write its
user-facing configuration.

## Location & format

```
~/.trusty-tools/<crate>/config.yaml
```

- `<crate>` is the crate's package name, e.g. `trusty-mpm`, `trusty-search`.
- The format is **YAML**.
- An absent file means "no config" → the crate uses its built-in defaults.
- A malformed file is logged at `warn` (to **stderr**, never stdout) and the
  crate falls back to defaults — a bad config never aborts startup.

Examples:

- `~/.trusty-tools/trusty-mpm/config.yaml`
- `~/.trusty-tools/trusty-search/config.yaml` (when that crate adopts it)

## trusty-mpm's settings (the first adopter)

`~/.trusty-tools/trusty-mpm/config.yaml`:

```yaml
# Managed-session workspace root template. A leading `~` is expanded.
# Sessions nest under <root>/<owner>/<repo>/<session-id>/.
workspace_root_template: ~/trusty-mpm-projects
# Default supervisor auto-resume preference.
auto_resume: false
# Default model id or tier alias for launched sessions.
default_model: sonnet
```

Resolution precedence for the workspace root is:

1. `TRUSTY_MPM_WORKSPACE_ROOT` environment variable (back-compat escape hatch);
2. `workspace_root_template` from this file;
3. the built-in default `~/trusty-mpm-projects`.

These settings are editable from the **trusty-console Config tab**
(`/api/console/config/mpm`, backed by the `config_read` / `config_write` MCP
tools), so operators never hand-edit the YAML unless they want to.

## Workspace-root migration

The managed-session workspace root moved from a legacy
`~/.trusty-mpm/workspaces/<project>/<session-id>/` layout to the current
`~/trusty-mpm-projects/<owner>/<repo>/<session-id>/` layout. This was a
**migration, not a hard cutover**: `trusty_mpm::core::workspace_scan` discovers
sessions under **both** roots, so pre-migration workspaces are never silently
orphaned. The authoritative session state remains
`~/.trusty-mpm/session-manager/sessions.json`; the dual-root scan is the
migration-aware filesystem discovery layer.

## The project-level `.trusty-mpm.toml` (#5207)

Every surface above is **per-host**. A project's own conventions — "this repo
launches on main, never in a worktree" — had to be re-declared by every operator
on every machine, and could never be reviewed or versioned. Since #5207
trusty-mpm also reads one file **from the project itself**:

```
<project>/.trusty-mpm.toml
```

```toml
# Do this repo's managed sessions get a per-session git worktree?
worktree = false
# Does an agent DISPATCHED in this repo get a worktree of its own? (#5814)
agent_worktree = false
# Default model id or tier alias for sessions launched in this project.
default_model = opus
```

**This file is committed.** It is tracked in git, travels with clones, and shows
up in PR diffs. That is the point of it, and it is why the file sits at the
project ROOT rather than inside `<project>/.trusty-mpm/`: that directory holds
machine-local session state, so projects gitignore it wholesale (this repository
ignores `.trusty-mpm/*`, which already makes the #4832 `framework/manifest.toml`
layer untrackable here). A file that must be committed cannot live in a
directory that exists to hold uncommittable state.

Where a setting appears here, this file is the **top** of its precedence chain:

| Setting | Precedence, highest first |
|---|---|
| `worktree` | `.trusty-mpm.toml` → the `projects.json` registry → built-in `true` |
| `agent_worktree` | `.trusty-mpm.toml` → built-in `true` |
| `default_model` | `.trusty-mpm.toml` → `config.yaml`'s `default_model` → `config.toml`'s `[models] default` → built-in `sonnet` |

`worktree` and `agent_worktree` answer different questions and neither implies
the other. `worktree` decides where a managed SESSION is placed; `agent_worktree`
decides whether an agent DISPATCHED from a main checkout is given a worktree of
its own under ADR-0048 decision 1. `agent_worktree` reads no registry layer at
all — a machine-global record must not decide a project's dispatch workflow.
Setting `agent_worktree = false` suits a repo with no concurrent writers and no
build state (a writing or documentation repo): its agents edit in place, commit
on the checked-out branch, and push. It exempts the worktree grant only. The
ADR-0044 main-checkout write boundary still denies a source-file edit there, and
a second concurrent writer in the same checkout is still refused.

`default_model` is a *default*: an explicit `--model`, a per-agent
`[models.agents]` entry, and an agent's own frontmatter are all more specific and
still win over it.

Two settings are deliberately **not** here. `workspace_root` decides where a
project gets cloned, so it cannot be read from a project that does not exist
yet — it stays host-level. `auto_resume` is a property of the operator's
supervisor rather than of the repository.

### Unknown keys

This file is parsed with `serde(deny_unknown_fields)`. `worktre = false` is an
error, not a silently-ignored key. At spawn time a rejected file contributes
nothing at all — resolution falls through to the next layer and the parse error
is logged at `error` level naming the offending key. Nothing in a file that
failed to parse is trusted, because a typo means the author's intent is unknown
rather than partially known, and because a committed file is shared: one bad push
must not brick every operator's session launches.

The two **host** config files are not strict, on purpose. Both answer a parse
failure by returning defaults, so denying unknown fields there would upgrade "one
key is ignored" into "the entire file is ignored" — a worse failure than the one
it fixes. They keep the lenient parse and instead log a warning naming every key
they dropped.

## Relationship to the legacy `~/.trusty-mpm/config.toml`

trusty-mpm's older `~/.trusty-mpm/config.toml` (agent sources, per-agent model
overrides, PM toggles) is **unchanged** and still read. The YAML convention
above is **additive** — it carries its own settings without disturbing the
existing TOML.

## Adding this convention to a crate

Crate maintainers implementing this convention in a new trusty-* crate should
see
[config-convention-internal.md](config-convention-internal.md) for the
shared `trusty-common` helper and the `serde_yaml` dependency notes.
