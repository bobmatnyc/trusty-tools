---
name: tm-architect-setup
description: Set up the Architect, the one fleet session per user, with `tm fleet init` — its project directory, a local-only git repo, the `supervisor` profile grant and an Opus launch. `tm fleet project add|remove|list` edits the watched set; a second `init` never makes a second Architect.
user-invocable: true
metadata:
  version: "0.3.0"
category: pm-reference
tags: [architect, fleet, setup, pm-recommended]
effort: medium
---

# tm-architect-setup — Set Up the Architect

🔴 **`tm fleet init`, `tm fleet status` and `tm fleet project` ship.** Run
`tm fleet --help` first. When it reports an unknown command, your `tm`
predates `tm fleet`: use "Without `tm fleet`" below. When it lists no
`project` command, your `tm` predates trusty-tools #10895: the watched set is
the `## Watch set` list in the Architect's `CLAUDE.md`, edited by hand.

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
   another directory. `init` creates the watch list when it is absent (see
   "Watched projects") and never changes an existing one; `tm fleet project`
   edits it.
3. **Local git repo, no remote.** It runs `git init` and adds no remote. The
   repo never inherits an `origin`. A private remote is added only when you
   ask for one. A public remote is never added.
4. **Fleet files.** `init` seeds `CLAUDE.md` with the fleet specifics (the
   user, a pointer to the watch list, the cadence, the records layout),
   seeds `local/` with the record templates (state, actions, goals, user,
   instance facts, a
   per-project template, the owner model and the mandates schema), writes
   the poller and the relay scripts (`relay.sh`, `posted.sh`) to `scripts/`,
   the voice guide to `docs/architect/`, and the Architect-only skills
   (`tm-fleet-check`, `tm-context-refresh` and the `architect-*` skills the
   operating manual indexes) to `.claude/skills/`. It writes
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

## Notification hook

tm's user-level `Notification` hook (#8392) appends
each PM permission prompt or input request to the Architect's
`inbox/events.jsonl`, so the Architect learns of a blocked session at once
instead of on its next pane poll. It is one hook for every tm session, set by
two keys in `~/.trusty-mpm/config.toml`:

```toml
[notification_hook]
enabled = true
inbox = "/absolute/path/to/architect/inbox"
```

`tm fleet init` writes both keys when the file has no `[notification_hook]`
section and applies the hook to the tm-managed settings. It never edits a
section that is already there: when its `inbox` names another directory,
`init` reports the step skipped. To change the keys yourself, edit the file,
then run `tm doctor --fix --only notification_hook --yes`. `tm doctor`
reports a `notification_hook` row that warns when the hook is enabled but not
applied. Removing an Architect does not remove the section; delete it by hand.

## Watched projects

The Architect's watch list is `local/watch.json` in its directory, changed
only through `tm fleet project` (trusty-tools #10895). Each command takes
`--dir` and reads the project registry from disk, so it works while the
daemon is down.

`tm fleet init` creates the list when it is absent (trusty-tools #11012). It
lists each project with a live `tm-*` tmux session, matched to a registry key
by the session's alias, else by its directory, plus any project the
`CLAUDE.md` import below would add, and writes each project's record first.
It prints one line per session that matches no project, or more than one,
and leaves that session out. With no tmux or no `tm-*` session the list is
empty, and an empty list watches every `tm-*` session. A list that exists,
even an empty one, is never changed; one that does not parse fails `init`
and is left as it was. To seed the list of an Architect set up before
#11012, run `tm fleet init` again.

- `tm fleet project add <key|dir>` writes `local/projects/<key>.md` from the
  record template when it is absent, then adds the project. It refuses a
  project the registry lacks, writing nothing; `tm projects register` adds
  one. A second `add` changes nothing and says so.
- `tm fleet project remove <key>` appends `- Watch: removed <time>` under the
  record's first heading, keeps the record, and drops the project.
- `tm fleet project list [--json]` prints each project's path, session alias,
  live tmux sessions and record, and flags a missing path, a key the registry
  no longer holds, and a key that differs from its session alias.

The first of these run with no list imports the hand-written `## Watch set`
lines of the Architect's `CLAUDE.md`, and prints a line for each it skipped
and why. It never edits `CLAUDE.md`; the Architect replaces those lines with
a pointer to the list once the list shows them.

## Status

`tm fleet status [--dir <path>] [--json]` reports four checks, read-only:
the allowlist entry, the profile request, the running `tm-architect` session
in that directory, and its Architect launch stamp. It exits 1 when any check
fails, and when its `binding` row is `UNBOUND`: the session's `claude` has no
Architect launch record, so every trust-anchor write is denied (#10448). Its `skills` row also warns when an Architect set up before `local/`
still keeps its records in the old directory; move them into `local/`
yourself, because `tm` moves nothing. Its `notification_hook` row reads
`on → <inbox>`, `off`, `enabled but not applied` or
`points elsewhere: <path>`; like `skills`, it never changes the verdict.
`tm fleet project list` reports the watched set.

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

- Never create a second Architect. Change the existing one's watched set with
  `tm fleet project add` and `tm fleet project remove`.
- Never add a public remote to the Architect's repo.
- Never set `TRUSTY_MPM_PM_UNRESTRICTED`. The `supervisor` profile replaces it.
