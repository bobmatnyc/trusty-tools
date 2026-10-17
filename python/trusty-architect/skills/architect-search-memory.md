---
name: architect-search-memory
description: Architect skill. Load before any trusty-search or trusty-memory call that writes, reindexes, renames, removes or restarts an index, before a palace write, and when recording or recalling a ruling. Covers index-id resolution, destructive verbs, cold-parked indexes, the serve-process rule, memory placement and rulings. Architect only.
user-invocable: true
metadata:
  version: "0.1.0"
category: monitoring
tags: [architect, trusty-search, trusty-memory, rulings]
effort: medium
---

# architect-search-memory — Search, Memory and Rulings

Load this before any trusty-search or trusty-memory write, reindex or
destructive verb, and before you record or recall a ruling. Your own palace
and your own search index are both named `architect`.

## trusty-search

<!-- rule:r-118 -->
**Index ids are not project names.** Call `list_indexes` before passing an
`index_id`; a guessed id returns `404 unknown index`. Record the id list in
`local/instance.md`.

<!-- rule:r-119 -->
**Prefer the CLI from the target repo.** From the repo:
`(cd <repo> && trusty-search search "<query>" --json)` detects its index.
From this directory: `trusty-search query "<query>" -i <index-id>`. A
`TRUSTY_INDEX` variable in your environment can override `--indexes <id>` and
search the wrong index; pass `-i`. The MCP `search` tool stays the fallback.

<!-- rule:r-120 -->
**Destructive commands.** Run `trusty-search index remove`, `reindex`,
`quantize` and every other destructive verb with `env -u TRUSTY_INDEX` and an
explicit `-i <id>`, after checking `index_status` for that id and that its
`root_path` matches. A `TRUSTY_INDEX` in the environment has overridden a
positional path and deleted the wrong index, and `reindex` and `quantize`
can rebase a live index onto another root.

<!-- rule:r-122 -->
**Cold-parked search index.** After a restart, `index_status` can say
"cold-parked by TRUSTY_MAX_RESIDENT_INDEXES"; the first search then returns
`408 query_timeout` while the index reloads, and `index_status` reads ready
seconds later. Retry once; do not reindex.

<!-- rule:r-123 -->
**trusty-search `serve` processes are MCP stdio children, not daemons.** Each
Claude session runs one. The daemon is `trusty-search start --foreground`
with parent pid 1. Never kill a `serve` process to "clean up" a second
daemon; check its parent with `ps -o ppid=`.

## trusty-memory and rulings

<!-- rule:r-065 -->
- **Memory placement.** A memory about a watched project goes into that
  project's own trusty-memory palace. Your palace, `architect`, holds only
  facts about operating the Architect and the owner's goals.

<!-- rule:r-011 -->
- **Rulings and lessons go to recall when made.** Store every owner ruling
  with `memory_remember` in the same pass it is made, in the right palace,
  tagged `ruling`. The drawer holds the ruling id, the date, the scope, the
  owner's words, what it supersedes and what it excludes. Store every lesson
  or incident with its trigger (when it applies), its limit (when it does
  not) and its risk. A success with no conditions is not a precedent.
  Before recommending a risky or irreversible action (killing a process, a
  daemon or system change, a delete, a cutover), run `memory_recall` for that
  action and cite what came back.

## Related skills

- `architect-operating-rules`: the search order for lookups.
