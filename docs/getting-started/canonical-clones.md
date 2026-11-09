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
- **Right account.** `tm` clones as the `gh` account that session will run as. A hand clone uses whatever credential `git` finds first.
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

Leave the old checkout where it is. `tm` does not move or adopt it. Do not run `tm project init` on it: that command is deprecated, and `tm projects register` replaces it (see [tm-project-control-plane.md](../specs/tm-project-control-plane.md)).

1. Push or stash anything you want to keep in the old checkout.
2. Run `tm run <owner>/<repo>`. This creates the canonical clone next to your other managed projects.
3. Work from the canonical clone from now on.
4. Delete the old checkout when you no longer need it.

Two cases look similar but are different:

- **A no-origin local repository.** `tm launch` runs the session in that checkout itself. It is not refused. Use this for a scratch repository that has no remote.
- **An old per-session layout.** Earlier versions kept full clones in per-session subdirectories under `~/trusty-mpm-projects/<owner>/<repo>/`. `tm` moves such a directory aside before it clones, so a stale layout does not block the new one.

## Change the root

The default root is `~/trusty-mpm-projects`. To move it, set `workspace_root_template` in `~/.trusty-tools/trusty-mpm/config.yaml`, or export `TRUSTY_MPM_WORKSPACE_ROOT`. See [config-convention.md](../reference/config-convention.md) for the precedence rules.

## Next steps

- [Install and Run trusty-mpm](install-and-run-tm.md)
- [Config convention](../reference/config-convention.md)
