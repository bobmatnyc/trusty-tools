# Architect watch set: CLI and dashboard management

## Epic plan

After `tm fleet init`, the owner adds watched projects by hand-editing the Architect's
`CLAUDE.md`. This epic replaces that with one machine-readable list, `local/watch.json`,
managed by `tm fleet project add | remove | list`, and later from the Architect dashboard.

### Outcomes

- O1: The owner adds or removes a watched project with one command, picking from registered projects, without editing `CLAUDE.md`.
- O2: The Architect reads the watch set from one list on every pass; the Architect and the owner change it through the same command.
- O3: An existing Architect keeps supervising the projects in its hand-written `CLAUDE.md` list after upgrading, with no manual step.
- O4: The owner adds or removes a watched project from the Architect dashboard.

### Ratified decisions

- D1: The watch set lives in `local/watch.json` in the Architect directory. The `local/projects/*.md` record files are not the list.
- D2: An entry is keyed by the registry key from `~/.trusty-mpm/project-registry/projects.json`. The path and session alias come from `~/.trusty-mpm/project-paths.json` when the list is read and are not stored.
- D3: Commands are `tm fleet project add | remove | list`. There is no pause or enable/disable.
- D4: `add` refuses a project missing from the registry: exit non-zero, nothing written, and the message names the missing registration and the command that registers it.
- D5: `remove` keeps `local/projects/<key>.md` and marks it removed.
- D6: The CLI is the only writer of `watch.json`. The dashboard queues a request; a deterministic applier runs the CLI for it.
- D7: The `CLAUDE.md` `## Watch set` section becomes one line pointing to `tm fleet project list`. The `local/state.md` `## Watch set` section stays as the per-pass record of live sessions; only its opening sentence changes.
- D8: Per-project goals are not part of an entry. Goals stay in `local/goals.md`.
- D9: The first `tm fleet project` command run with no `watch.json` imports the hand-written `CLAUDE.md` `## Watch set` entries, then never again. The CLI never edits `CLAUDE.md`; the Architect replaces the section with the pointer.

### Ordering

The epic is blocked by #10855, which drops the unused `<goal>` field from the same
`CLAUDE.md` template line. Phase 1 later replaces that whole list with a pointer and
rebases over it. Phase 2 cannot start until Phase 1 is released and installed: the
dashboard applies every change by running the installed `tm fleet project add/remove`,
and it reads the `watch.json` format Phase 1 fixes. #10885 edits the hook text in
`tm-architect-setup.md:70-73`; Phase 1 owns that file's command text, and whichever
lands second rebases.

### Phase: CLI watch list, migration and Architect rule text

#### Scope

- `tm fleet project add <key|dir>`, `remove <key>` and `list [--json]`, backed by `local/watch.json` (`{"version": 1, "projects": [...]}`). Writes use `trusty_common::file_lock::with_exclusive_lock` plus write-and-rename.
- The registry is read from disk (`ProjectStore`), not the daemon, so the commands work with the daemon down. The session alias comes from `ProjectAliasStore`; the live session count comes from tmux `list-sessions`.
- `add` writes `local/projects/<key>.md` from the template when it is absent, then adds the list entry, in that order.
- `list --json` fields per project: `key`, `path`, `path_exists`, `registered`, `session_alias`, `alias_mismatch`, `live_sessions`, `record_present`.
- Migration (D9): when `watch.json` is absent, the first `tm fleet project` command reads the Architect `CLAUDE.md` `## Watch set` lines (`` - `tm-<project>`: `<path>` ``, with or without a trailing goal). It maps each line to a registry key by its path in `project-paths.json`, else by stripping `tm-` and matching the registry key. It adds every match, prints one line per line it skipped and why, and then creates `watch.json` even if nothing matched.
- Architect text: r-067 says "run `tm fleet project add`". `tm-fleet-check` step 1 runs `tm fleet project list`, and the Architect replaces a still-populated `CLAUDE.md` `## Watch set` with the pointer once the list shows the migrated entries. Update every other place that says `CLAUDE.md` holds the watch set: `architect.md:25`, `architect-workflow.md:113,124`, `supervisor/identity.md:13`, `architect-project-provision.md:105`, `python/trusty-architect/README.md:241-242`, `tm-architect-setup.md:34,70`. The `CLAUDE.md` template's `## Watch set` becomes the pointer; the `local/state.md` template's opening sentence points at `watch.json`. Both template copies change (crate assets and `python/trusty-architect`).

#### Acceptance criteria

1. `add` of a project missing from the registry exits non-zero. A test asserts `watch.json` is byte-identical when it existed, and still absent when it was absent and `CLAUDE.md` lists nothing.
2. `add` run twice for the same project leaves one entry and reports that nothing changed.
3. Two `add` commands run concurrently for different projects leave both entries.
4. Writes happen record first, then list. If the record write fails, `add` exits non-zero and the list is unchanged. If the list write fails after the record, `add` exits non-zero and a re-run completes it. Error-arm tests fail against a version that only warns (Fail-Open Check). `list` shows `record_present: false` for a record deleted by hand.
5. `remove` appends `- Watch: removed <ISO-8601>` under the record's first heading and drops the entry. `remove` of a key not in `watch.json` exits non-zero and writes nothing.
6. `list --json` returns the eight named fields; a project with a missing path is still listed, with `path_exists: false`; a registry key that differs from its session alias gives `alias_mismatch: true`.
7. Migration: with no `watch.json` and a fixture `CLAUDE.md` listing two registered projects and one unregistered one, the first `list` creates `watch.json` with the two, prints a skip line naming the third, and leaves `CLAUDE.md` byte-identical. A second `list` imports nothing. Removing every entry leaves an empty `watch.json`, and the next command imports nothing.
8. `tm fleet init` seeds the new `CLAUDE.md` pointer and the `local/state.md` wording; the bundle tests that read those templates pass; `scripts/check_architect_subproject.sh` and `tm generate capabilities --check` pass.
9. Live check, in a temp Architect directory passed with `--dir` under a `TRUSTY_MPM_ROOT` that points at a fixture registry: migration from a hand-written list, then add two projects, refuse one unregistered project, remove one, and `list --json` matches.

#### Non-goals

- Notification hook setup.
- Dashboard changes.

#### Risk

Normal: one crate plus content and template text, with templates embedded by `include_str!`. Rung 3; changelog fragment in `crates/trusty-mpm/changelog.d/`. Labels: `enhancement`, `trusty-mpm`, `trusty-architect`.

Gate: #10855 merged.

### Phase: Dashboard watch-set management

#### Scope

- The Projects view keeps listing registered projects only, as today, and shows each as Watched, Not watched, Watch pending, or Watch failed, read from `watch.json` and the request queue. Each row shows a path-missing flag and a flag when the registry key differs from the session alias.
- Watch and Unwatch write a `watch_changes` request to the owner request queue; the dashboard never writes `watch.json`. Unwatch asks for confirmation and states that the project record is kept.
- New request kind: a `watch_changes.schema.json` with status `new → applied | failed`, plus `failed → new` (Retry) and `failed → dismissed` (Dismiss). Register it in the seed table, the drift-gate pairs, the dashboard and owner-answer collections, and the pending derivation.
- A deterministic applier (a `tm fleet project` subcommand or a seeded script) runs the CLI for each `new` request and records `applied` or `failed` with the CLI's message. The Architect's pass rule runs the applier.

#### Acceptance criteria

1. A Watch POST leaves `watch.json` bytes and mtime unchanged and creates exactly one `watch_changes` request with status `new`; a pytest asserts both.
2. The applier marks a request `failed`, with the CLI's message, whenever the CLI exits non-zero; a test against an applier that ignores exit codes fails.
3. Two `new` requests for the same project applied in one run produce one entry.
4. After the applier runs, each row's watch state matches `tm fleet project list --json`.
5. Retry returns a failed request to `new`; Dismiss sets it to `dismissed` and the row shows Not watched.
6. `python -m pytest python/trusty-architect/tests`, `scripts/check_architect_subproject.sh` and `tm fleet init` seeding of the new schema pass; one smoke run of the dashboard shows the flags and queues a request.

#### Non-goals

- Editing goals from the dashboard.
- Any write path to `watch.json` other than the CLI.
- Cancelling a pending request.

#### Risk

High: HTTP routes and the dashboard page. Rung 6; changelog fragment in `crates/trusty-mpm/changelog.d/`. Labels: `enhancement`, `trusty-mpm`, `trusty-architect`.

Gate: Phase 1 released, installed on the owner's host with `cargo install`, and its live check passed there.

### Deferred

| Item | Why | Where |
|---|---|---|
| Pause / enable-disable of a watched project | Owner dropped it; `quiet-sessions.txt` covers idle alerts | — |
| Pre-canned or per-project goal text | Goals live in `local/goals.md`; nothing reads a goal on the watch line | — |

## Dashboard mockups

Illustrative only; layout follows the existing Projects view.

Projects view, watch state per registered project:

```text
┌─ Projects ──────────────────────────────────────────────────────────────────────────────┐
│ Project              Repo                                Sessions  Watch                 │
├──────────────────────────────────────────────────────────────────────────────────────────┤
│ mcp-services         duettoresearch/mcp-services         2 live    ● Watched     [Unwatch] │
│ model-usage-monitor  duettoresearch/model-usage-monitor  3 live    ● Watched     [Unwatch] │
│ trusty-tools         bobmatnyc/trusty-tools              2 live    ○ Not watched [Watch]   │
│ concept-lab          duettoresearch/concept-lab          0         ○ Not watched [Watch]   │
│ pegasus-ui           duettoresearch/pegasus-ui           0  ⚠ path missing  ○ Not watched [Watch] │
└──────────────────────────────────────────────────────────────────────────────────────────┘
 Registered projects only (projects.json). Watch state from local/watch.json.
```

After Watch, until the next Architect pass:

```text
│ trusty-tools   bobmatnyc/trusty-tools   2 live   ◐ Watch pending
                                                   queued 14:22Z · applied on next Architect pass
```

After the request is applied:

```text
│ trusty-tools   bobmatnyc/trusty-tools   2 live   ● Watched   [Unwatch]
                                                   added 14:31Z (tm fleet project add)
```

Unwatch confirmation:

```text
┌─ Unwatch model-usage-monitor? ───────────────────────────────────┐
│ The Architect stops supervising its sessions after its next pass. │
│ Its record (local/projects/model-usage-monitor.md) is kept and    │
│ marked removed; watching it again later restores it.              │
│                                              [Cancel] [Unwatch]   │
└───────────────────────────────────────────────────────────────────┘
```

A failed request:

```text
│ concept-lab   duettoresearch/concept-lab   0   ✕ Watch failed   [Retry] [Dismiss]
                                               tm fleet project add: path does not exist
```

Registry key and session alias differ:

```text
│ trusty-tools   bobmatnyc/trusty-tools   0 ⚠   ● Watched   [Unwatch]
                                          sessions are named tm-trusty-tools-fork-*; registry key is trusty-tools
```
