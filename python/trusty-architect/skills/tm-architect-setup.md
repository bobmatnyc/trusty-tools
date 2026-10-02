---
name: tm-architect-setup
description: Set up the Architect, the one fleet session per user, with `tm fleet init` — its project directory, a local-only git repo, the `supervisor` profile grant and an Opus launch. A second run edits the watched set; it never makes a second Architect.
user-invocable: true
version: "0.1.0"
category: pm-reference
tags: [architect, fleet, setup, pm-recommended]
effort: medium
---

# tm-architect-setup — Set Up the Architect

🔴 **`tm fleet init` and `tm fleet status` ship; `add` and `remove` do not.**
They land in phase P3 of trusty-tools #8436. Run `tm fleet --help` first.
When it reports an unknown command, your `tm` predates `tm fleet`: use
"Without `tm fleet`" below.

## What the Architect is

The Architect is the one fleet session per user. It watches the PM
sessions of the projects you name, relays between them and you, and does not
implement. It runs the `supervisor` profile (trusty-tools #8453): the Architect
instructions, the `trusty-mpm-supervisor` output style, and the Opus tier
alias `opus` as its model.

## What `tm fleet init` does

1. **Directory.** It creates the Architect project at
   `~/trusty-mpm-projects/architect`. `--dir <path>` overrides the location.
2. **One Architect per user.** A second run in the same directory changes
   nothing and says so. `init` refuses when another allow-listed project
   already requests the `supervisor` profile, or when `tm-architect` runs in
   another directory. Editing the watched set is `tm fleet add` (P3).
3. **Local git repo, no remote.** It runs `git init` and adds no remote. The
   repo never inherits an `origin`. A private remote is added only when you
   ask for one. A public remote is never added.
4. **Fleet files.** `init` seeds `CLAUDE.md` with the fleet specifics (the
   user, the watch set, the cadence, the records layout), creates `records/`,
   writes the poller to `scripts/`, and writes the Architect-only skills
   `tm-fleet-check` and `tm-context-refresh` to `.claude/skills/`. It writes
   each file only when it is absent and reports an edited one as skipped, so
   your edits survive every later run. The seeded `CLAUDE.md` declares no
   IDENTITY, ENFORCEMENT or WORKFLOW override blocks, and the project's
   `.claude/settings.json` never sets `TRUSTY_MPM_PM_UNRESTRICTED`.
5. **Profile request.** It writes `profile = "supervisor"` into the project's
   `.trusty-mpm.toml`.
6. **Profile grant.** It always adds the project's canonical absolute path to
   `[supervisor] projects` in the user-level `~/.trusty-mpm/config.toml`, also
   when a PM invokes it. This is an owner ruling on #8436; it departs from the
   #3981 rule that a project must not grant itself the profile. Without this
   entry the launch falls back to the PM profile.
7. **Launch.** It starts the Architect's session detached, as tmux session
   `tm-architect` on the `opus` alias, and records the Architect launch
   stamp on that session. Attach with `tmux attach -t =tm-architect`.
   It then starts the poller, `scripts/fleet-poll.py`, in tmux session
   `tm-architect-poll` unless it already runs; a poller that cannot start
   fails the command. `--no-launch` sets up the files and the grant without
   starting either. The session is not registered with the daemon, so `tm ls`
   does not list it.
8. **No twin mode.** `init` writes no `[supervisor.twin]` grant and never
   launches with `--twin`. Twin mode (#8878) stays a separate opt-in.

## Watched projects

`tm fleet add <dir>` adds a project to the watched set and installs that
project's notification hook (#8392). `tm fleet remove <dir>` removes that
project's hook only and leaves every other watched project unchanged.
`tm fleet status [--dir <path>] [--json]` reports four checks, read-only:
the allowlist entry, the profile request, the running `tm-architect` session
in that directory, and its Architect launch stamp. It exits 1 when any check
fails. The watched set joins the report in P3.

## Without `tm fleet`

For a `tm` that predates `tm fleet`, the operator runs these steps in a
terminal. They produce the files and grant `tm fleet init` writes.

```sh
dir="$HOME/trusty-mpm-projects/architect"      # or another path
mkdir -p "$dir" && git -C "$dir" init
printf 'profile = "supervisor"\n' > "$dir/.trusty-mpm.toml"
```

Add the absolute path to the user-level allowlist in
`~/.trusty-mpm/config.toml`. Extend an existing `[supervisor]` table rather
than adding a second one:

```toml
[supervisor]
projects = ["/absolute/path/to/architect"]
```

Then launch the session: `tm launch "$dir"`.

## Verify

- `tm fleet status` exits 0 and prints `complete`.
- `git -C "$dir" remote` prints nothing.
- `tm doctor`, run in the Architect's directory, shows the `session_profile`
  row saying that a launch here runs the `supervisor` profile.
- The running session shows the `opus` model and the Architect output style (`trusty-mpm-supervisor`).

## Rules

- Never create a second Architect. Edit the watched set of the existing one.
- Never add a public remote to the Architect's repo.
- Never set `TRUSTY_MPM_PM_UNRESTRICTED`. The `supervisor` profile replaces it.
