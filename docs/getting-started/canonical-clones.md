# Let `tm` Create Your Project Clones

Run `tm run <owner>/<repo>` and let trusty-mpm clone the repository. Do not clone it by hand into a flat directory such as `~/code/<repo>`, and do not run `tm project init` on an old checkout.

## The canonical layout

`tm` keeps every managed project at one path:

```text
~/trusty-mpm-projects/<owner>/<repo>/                  # the base clone
~/trusty-mpm-projects/<owner>/<repo>/.worktrees/...    # per-session worktrees
```

For example, `bobmatnyc/trusty-tools` lives at `~/trusty-mpm-projects/bobmatnyc/trusty-tools`.

## Why `tm` owns the clone

- **One path per repository.** The `<owner>/<repo>` key gives the same path on every machine. `tm` can derive the location from the repository name instead of from a path you chose.
- **Right account.** `tm` clones as the `gh` account pinned for the project, else the account its `[accounts]` org map names for the owner, else your ambient `git` credential. The session then runs as that same account. A hand clone always uses the ambient credential.
- **Safe sessions.** The base clone stays clean. Each session works in its own worktree under it, and `tm` installs the cross-branch push guard when it clones.
- **Wrong remotes fail loud.** If a checkout at the canonical path has a different `origin`, `tm` stops. It never re-points a remote.

A flat hand clone has none of this. Nothing ties it to an `<owner>/<repo>` key, so `tm run <owner>/<repo>` will not reuse it.

## New project

```bash
tm run <owner>/<repo>
```

`tm` clones the repository (or checks the existing clone), prepares the session, and starts it in a tmux pane. The session appears in `tm ls` and `tm sessions`. A full repository URL works too, for hosts other than GitHub.

A dirty checkout does not block the run. `tm` skips the fast-forward, prints a warning that names the path, and starts the session anyway.

## Existing flat or legacy checkout

Leave the old checkout where it is. `tm` does not move or adopt it. Do not run `tm project init` on it: the [project control-plane spec](../specs/tm-project-control-plane.md) marks that command deprecated in favour of `tm projects register`.

1. Push or stash anything you want to keep in the old checkout.
2. Run `tm run <owner>/<repo>`. This creates the canonical clone next to your other managed projects.
3. Work from the canonical clone from now on.
4. Delete the old checkout when you no longer need it.

### If you already ran `tm project init` on it

`tm project init` changes four things in the checkout's git metadata and in the trusty-mpm state. Each one can be reversed:

| Change | Reverse it with |
|---|---|
| `git config --local maintenance.auto false` and `git config --local gc.auto 0` | `git config --local --unset maintenance.auto` and `git config --local --unset gc.auto` |
| `/.trusty-mpm-worktree` and `/.trusty-mpm/` appended to `$(git rev-parse --git-common-dir)/info/exclude` | Delete those two lines from that file |
| Each tree's in-tree `.trusty-mpm-worktree` marker moved to `<common-dir>/worktrees/<name>/trusty-mpm-worktree` (for the main checkout, `.git/trusty-mpm-worktree`) | Move each file back to `<tree>/.trusty-mpm-worktree` |
| Every existing worktree recorded in `~/.trusty-mpm/project-registry/worktrees.json` | `tm project unadopt <path>...` |

`tm project unadopt` edits the ledger only. It never edits or deletes a worktree folder and never touches git metadata. Run it with `--dry-run` first to see which records it would remove. A later `tm project init` on the same checkout adopts the worktrees again.

Two cases look similar but are different:

- **A no-origin local repository.** `tm launch` runs the session in that checkout itself. It is not refused. Use this for a scratch repository that has no remote.
- **An old per-session layout.** Earlier versions kept full clones in per-session subdirectories under `~/trusty-mpm-projects/<owner>/<repo>/`. If that directory has no `.git`, `.base`, or `.worktrees` entry, `tm` renames it to `<repo>.old-layout-backup-<timestamp>` before it clones, and keeps your data there. If it has one of those entries, `tm` stops with an error and moves nothing. Remove or relocate the directory yourself.

## Change the root

The default root is `~/trusty-mpm-projects`. To move it, set `projects_root` in `~/.trusty-tools/install.toml`, set `workspace_root_template` in `~/.trusty-tools/trusty-mpm/config.yaml`, or export `TRUSTY_MPM_WORKSPACE_ROOT`. The order is `TRUSTY_MPM_REPOS_ROOT`, then `TRUSTY_MPM_WORKSPACE_ROOT`, then `install.toml` `projects_root`, then `workspace_root_template`, then the default: environment, then install file, then template, then default. A broken `install.toml` stops the clone with an error instead of using the default. See [config-convention.md](../reference/config-convention.md) for the precedence rules.

## Next steps

- [Install and Run trusty-mpm](install-and-run-tm.md)
- [Config convention](../reference/config-convention.md)
