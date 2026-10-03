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
# Machine-level Rust build settings (#6868). Absent → the defaults below.
build:
  # Shared CARGO_TARGET_DIR. Default: ~/.trusty-tools/cargo-target/<owner>/<repo>,
  # derived from the project's `origin` remote. A leading `~` is expanded.
  cargo_target_dir: ~/.trusty-tools/cargo-target/bobmatnyc/trusty-tools
  # CARGO_BUILD_JOBS. Default: half this host's cores, minimum 2.
  build_jobs: 8
  # Whether briefs should carry RUSTC_WRAPPER=sccache. Default false.
  sccache: false
```

### The `build:` section and the `rust_build_env` row (#6868)

Every tm-provisioned worktree gets an empty `target/`, so each dispatched
engineer pays a cold full-workspace build. Measured on the 16-core dev host on
2026-09-16: ~200 s cold in a fresh worktree, 103 s with `CARGO_TARGET_DIR`
pointed at a warm shared directory from another worktree, 17 s from that path
again. sccache was neutral on the same tree, because path crates build
incrementally and incremental artifacts are not cacheable. Cargo's target lock
serialises concurrent builds sharing the directory, which is wanted here — six
concurrent cold builds crashed that host on 2026-08-08.

`tm doctor`'s `rust_build_env` row reports the resolved values for any project
whose detected stack includes Rust, and closes with the line a PM pastes
verbatim into an engineer brief:

```
CARGO_TARGET_DIR=<dir> CARGO_BUILD_JOBS=<n> [RUSTC_WRAPPER=sccache] SKIP_UI_BUILD=1
```

`RUSTC_WRAPPER=sccache` appears only when `build.sccache` is `true`. The row is
`Ok` when the directory exists and is writable, `Warn` when it does not exist
yet, and `Fail` only when it exists and cannot be written.
`tm doctor --fix --yes` creates the directory and seeds this section when no
`build` key is present, preserving every key already there. Neither the row nor
the repair ever writes `~/.cargo/config.toml` — that file is machine-global for
every Rust project on the host, so wiring `build.rustc-wrapper` there stays an
operator decision, and the row warns when `sccache: true` sits beside an unwired
config.

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
# Does every prompt composed for this project ask for a `## Prompt feedback`
# addendum? (#7688)
prompt_self_improvement = true
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
| `prompt_self_improvement` | `.trusty-mpm.toml` → `config.toml`'s `[pm] prompt_self_improvement` → built-in `false` |

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

`prompt_self_improvement` (#7688) asks the model what was wrong with the prompt
it received. On, the composed PM prompt gains a section requesting a
`## Prompt feedback` addendum of at most 5 lines — what was unclear, what was
unnecessary — and telling the PM to append the same request to every dispatch
brief it sends, which is how a dispatched agent is asked. `tm hook
--prompt-feedback` is registered on `Stop` and `SubagentStop` only where the flag
is on; it extracts that section into `~/.trusty-mpm/prompt-feedback.jsonl`, which
`tm prompt-feedback` reads back. Off — the default — nothing is injected and
nothing is registered. It belongs on this committed surface because whether a
repository wants its prompts critiqued is a property of the repository: a
high-churn harness repo is where the signal pays, and a stable consumer project
is where five extra lines per response are pure cost. Nothing is written into the
deployed agent files, which are one machine-global set shared by every project on
the host, so two projects disagreeing about the flag never rewrite each other's
copies.

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

### The `[pm_guard]` section (#9018)

```toml
# Register the `tm hook --pm-guard` PreToolUse entry at every launch?
[pm_guard]
enabled = false   # default: true
```

`enabled = false` turns the PM guard off (owner ruling 307). The launch, resume
and `tm install` writers then leave the `<tm> hook --pm-guard` entry out of
`.claude/settings.json` and remove one a prior launch wrote, matched by its
` hook --pm-guard` suffix. Every other managed hook stays, including the
observability `tm hook` entry. A missing key or section means `true`, and so
does a `config.toml` that cannot be read or parsed: a broken file fails closed
to guarded. The `tm doctor` `pm_guard` row reports the state, and warns
"pm-guard disabled by [pm_guard] enabled = false" when the key is off.

A running Claude Code session keeps the hooks it loaded at startup, so the
change takes effect at the session's next launch.

#### `runtime_checkouts` (#8524)

```toml
[pm_guard]
# Cron-host checkouts that launchd jobs read in place (#8524).
runtime_checkouts = ["/Users/me/Projects/cron-host"]
```

The list defaults to empty, which keeps every main checkout under the full
rules.

- **Match semantics.** An entry matches a main checkout when both
  canonicalize to the same path. The guard first walks up to the checkout root
  (the nearest ancestor with a `.git` directory), so an entry must name that
  root: an entry naming a subdirectory matches nothing. A leading `~` is not
  expanded, so write absolute paths. An entry or a checkout that cannot be
  canonicalized matches nothing, and the guard then applies the full rules.
- **`runtime_checkouts`.** In a listed checkout the destructive-git rule
  allows exactly one shape: a lone `git reset --keep [-q] [<rev>]`, with
  nothing but `cd` around it, and only when `git diff --quiet` reports that
  the tracked content already equals `<rev>`. A probe that cannot answer
  keeps the deny.
- **Trust.** The list is trusted to the #8878 trust-anchor floor, which
  refuses a write to `~/.trusty-mpm/config.toml` from any session but the
  Architect's. The #8879 residual applies: a write made by an interpreter,
  `dd`, `rsync`, `find -exec`, or an executed script file is not seen, so a
  session that reaches one of those can add a checkout to the list.

### The `[accounts]` section (#9091)

```toml
# Which gh account clones and spawns sessions for a GitHub org.
[accounts]
duettoresearch = "bob-duetto"
```

Each key is a GitHub org (or user) name, matched without case, and each value
is a `gh` login already logged in on this host. Only github.com repositories
are mapped. An SSH host alias is resolved through `~/.ssh/config` first, so
`git@github-bob:duettoresearch/x` maps when `github-bob` names github.com.
gitlab.com, Bitbucket and GitHub Enterprise Server remotes are never mapped.

Precedence, highest first. For a github.com remote, clone, spawn and the
launch preflight read the registry pin through one selection, so a session runs
as the account its base clone was made with. For any other host (for example
GitHub Enterprise Server), clone and preflight do not read the pin, and the
spawn still applies and proves it.

1. An explicit selection: `--account`, `--user` or `--u`, or a
   `<login>@<owner>/<repo>` positional.
2. The registry pin for the repository, written by an earlier flag or by
   `tm projects register --gh-account`. The registry pin wins over this table.
   A record pinned by any field — `gh_account`, `github.account`,
   `github.config_dir` or `github.token_env` — counts, and the table is not
   read. The login is `gh_account`, else `github.account`; a pin naming
   neither runs with the identity it pinned before this table existed.
3. This table, looked up by the repository owner.
4. The ambient identity, exactly as before. An org with no entry stays here.

The table applies at the managed base clone (`tm <owner>/<repo>`, `tm run`,
`tm launch --worktree`, the daemon's in-project spawn) and at every session
spawn whose project pins no account. It is never written to the project
registry, so editing the table changes the next clone or spawn.

This table is read strictly, unlike the rest of the file. A malformed table is
an error that names the problem: a value that is not a string, a blank or
invalid login, two orgs that differ only in case, or `accounts` that is not a
table. A TOML syntax error (an unquoted login is the common case) is an error
when any line of the file names `accounts`: a header `[accounts]`,
`["accounts"]`, `['accounts']`, `[[accounts]]` or `[accounts.<x>]`, or a key
line `accounts = …` or `accounts.<org> = …`. Whitespace, quotes and `#`
comments are ignored in the match, and a matching key line under another table
also counts. In a file where no line names `accounts`, a syntax error reads as
an empty table and logs a warning, the way the rest of the file treats it.

When the table cannot be read:

- `tm run <owner>/<repo>` and `tm launch` refuse before anything is cloned or
  spawned, and print the error, which names `~/.trusty-mpm/config.toml` and the
  parse error.
- A clone refuses with that error and leaves the disk unchanged.
- A spawn gets a `gh` token that authenticates as nobody and logs the error.
- The `tm doctor` row `org_accounts` reports `Fail` with the error. It reports
  `Ok` with the number of mapped orgs, `Ok` "missing" when there is no table,
  and `Warn` for a syntax error read past as an empty table.

Nothing falls back to the machine's active account. A command that names its
account explicitly, or a repository with a registry pin, never reads the table.
When a mapped login has no proven token, the spawn warning names this table and
suggests `gh auth login` for that account, or editing the table.

## Adding this convention to a crate

Crate maintainers implementing this convention in a new trusty-* crate should
see
[config-convention-internal.md](config-convention-internal.md) for the
shared `trusty-common` helper and the `serde_yaml` dependency notes.
