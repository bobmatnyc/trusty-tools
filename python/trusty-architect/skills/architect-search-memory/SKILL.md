---
name: architect-search-memory
description: Framework skill. Load before any trusty-search or trusty-memory call that writes, reindexes, renames, removes or restarts an index, before a palace write, and when recording or recalling a ruling. Covers index-id resolution, destructive verbs, cold-parked indexes, and the serve-process rule.
---

# Architect search and memory (framework)

Kind: framework. Index line in architect.md: load this before any trusty-search or trusty-memory write, reindex, or destructive verb.

## trusty-search

<!-- rule:r-118 -->
**Index ids are not project names.** Call `list_indexes` before passing `index_id`; a guessed id returns `404 unknown index`. Search is HTTP-only in 0.54.2 (`daemon_base_url`); the `trusty-search.sock` in Application Support is the embedder's. Search over UDS is trusty-tools #6285 (epic #6284), open. The current id list is in the instance host skill.

<!-- rule:r-119 -->
**Prefer the trusty-search CLI locally (owner, 2026-09-24).** From the target repo: `(cd <repo> && trusty-search search "<query>" --json)` auto-detects its index (~55 ms). From the Architect's own directory: `trusty-search query "<query>" -i <index-id>`. Note: `query --indexes <id>` was overridden by this session's `TRUSTY_INDEX=architect` pin and searched the architect index instead; use `-i`. The MCP `search` tool stays the fallback.

<!-- rule:r-120 -->
**Destructive commands.** Run `trusty-search index remove` (and any other destructive verb) with `env -u TRUSTY_INDEX` and an explicit `-i <id>`, after checking `index_status` for that id. Until #8175 is fixed, TRUSTY_INDEX in the env overrides a positional path: on 2026-09-27 it deleted the trusty-tools index instead of a scratch index. `reindex` and `quantize` have the same defect (#8737, P1): they can rebase a live index onto another root. Same rule, and check that `root_path` in `index_status` matches before running.

<!-- rule:r-122 -->
**Cold-parked search index (2026-10-04 19:3xZ).** After a restart, `index_status` says "cold-parked by TRUSTY_MAX_RESIDENT_INDEXES"; the first search returns 408 query_timeout while the index reloads, and `index_status` reads ready seconds later. Retry once; do not reindex.

<!-- rule:r-123 -->
**trusty-search serve processes are MCP stdio children, not daemons.** Each Claude session runs one (24 on 2026-09-28). The daemon is `trusty-search start --foreground` with parent pid 1. Never kill a `serve` process to "clean up" a second daemon; check its parent with `ps -o ppid=`.

## trusty-memory and rulings

<!-- rule:r-065 -->
- **Memory placement.** A memory about a watched project goes into THAT project's trusty-memory palace. The Architect's own palace (`architect`) holds only facts about operating the Architect.


## Moved from the operating manual (restructure phase 2, 2026-10-07)

<!-- rule:r-011 -->
- **Rulings and lessons go to recall when made.** (A) Every owner ruling is stored with `memory_remember` in the same pass as it is made, in the right palace and tagged `ruling`. The drawer holds the ruling id, the date, the scope, the owner's words, what it supersedes and what it excludes. (C) Every lesson or incident is stored with its trigger (when it applies), its limit (when it does not) and its risk. A success with no conditions is not a precedent. Before recommending a risky or irreversible action (killing a process, a daemon or system change, a delete, a cutover), run `memory_recall` for that action and cite what came back. Analysis and precedents: `architect-search-memory`.
