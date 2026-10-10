# Runbook: Rename a trusty-memory palace

Change a palace's id with `tm memory rename`. The palace keeps its drawers,
rooms, wings, knowledge-graph triples and vectors. The old id stays as an alias
of the new one, so existing callers keep working.

Tracked in [#9544](https://github.com/bobmatnyc/trusty-tools/issues/9544).
Shipped in [#9634](https://github.com/bobmatnyc/trusty-tools/pull/9634)
(`palace_rename` verb and `tm memory rename`),
[#9624](https://github.com/bobmatnyc/trusty-tools/pull/9624) (the registry
primitive) and [#9611](https://github.com/bobmatnyc/trusty-tools/pull/9611)
(write locks and chat sessions keyed on the canonical id).

## Preconditions

1. Install trusty-memory 0.32.1 or later and `tm` (trusty-mpm) 1.8.2 or later.
   A daemon without the verb answers -32601, and `tm` prints "the trusty-memory
   daemon at <socket> predates palace_rename; restart it on a release that has
   it, then retry". Restart the daemon on 0.32.1 or later.
2. Stop the writers that take no palace lock: knowledge-graph writes
   (`kg_*`), room and wing changes, `memory_forget`, and dream runs. The rename
   locks the palace against drawer writes, not against these. If one runs
   during the rename, the after-count can differ from the before-count and the
   rename answers -32603 even though the move succeeded (see Limitations).
3. Check the source is a real palace id, not an alias. `palace_list` shows the
   palaces; the data root's `palace_aliases.json` lists the aliases.
4. Check the target. If it exists, it must be empty. Empty means no drawers,
   no chat sessions and no unaccounted legacy data.
5. A palace stored in a newer on-disk format than this daemon reads is refused
   with `FormatTooNew` as -32006 and is not moved. Upgrade trusty-memory.

## Command

```bash
tm memory rename <OLD> <NEW> [--replace-empty] [--json] [--memory-socket <PATH>]
```

| Argument or flag | Meaning |
|---|---|
| `<OLD>` | Current palace id. Must be the palace, not an alias. |
| `<NEW>` | New id. Must match `[a-z0-9][a-z0-9-]{0,62}` and differ from `<OLD>`. |
| `--replace-empty` | Replace an existing empty palace at `<NEW>`. Default off. |
| `--json` | Print the daemon's JSON answer instead of the one-line summary. |
| `--memory-socket <PATH>` | trusty-memory socket. Default: the derived socket. |

`tm` runs the protocol handshake first, then calls `palace_rename` with a
120-second budget. It exits non-zero on any refusal and prints the daemon's
reason.

The MCP and RPC equivalent is the `palace_rename` tool:

| Parameter | Type | Meaning |
|---|---|---|
| `palace_id` | string, required | Current id. Must be the palace itself. |
| `new_id` | string, required | New id. |
| `replace_empty` | boolean, default `false` | Replace an empty palace at `new_id`. |

## Behaviour

- The daemon holds the write locks of both ids for the whole rename. A lock
  held longer than the write-lock timeout gives -32006 "is busy ... retry the
  rename when it is idle". Retry when the palace is idle.
- It counts drawers, triples, vectors, rooms and wings before the move and
  again after it. A difference returns -32603 naming both counts. The rename is
  not rolled back.
- A non-empty target is refused with -32006 ("already exists and is not
  empty"). An empty target is refused with -32006 ("it is empty; pass
  replace_empty to replace it") unless `--replace-empty` is set.
- With `--replace-empty`, the empty target is moved, not deleted, to
  `<data root>/.trash/<NEW>-replaced-<UTC timestamp>/`. The answer carries the
  path in `trashed_target`. A failed move puts the target back.
- A source that is an alias, a target that is an alias of another palace, an
  invalid target id, and a missing source are refused. The first three give
  -32006. A missing source gives -32004.
- After the rename, `<OLD>` resolves to `<NEW>` for reads and writes. Every
  other alias that pointed at `<OLD>` is retargeted to `<NEW>`. The aliases are
  stored in `<data root>/palace_aliases.json`.
- `palace_create` with the alias id is refused with -32006 ("palace id ... is
  an alias of palace ...; remove the alias before creating a palace with that
  id").
- Re-running the same rename after a crash finishes it. The answer shows
  `"resumed": true`.
- The rename does not rewrite knowledge-graph subjects written by
  `kg_bootstrap`, backups under `backups/format-migration/<OLD>`, or project
  pin files on disk. All of them still name `<OLD>`, which resolves through the
  alias.

## Verify

Run these after the rename. Use the new id for `<NEW>`.

1. Counts. Compare the `counts.before` and `counts.after` values from
   `tm memory rename ... --json`. They must match. Then run `palace_info` with
   `palace=<NEW>` and check drawers, triples, vectors, rooms and wings.
2. A read through the old id. Run `palace_info` with `palace=<OLD>`. It must
   return the data of `<NEW>`. A `memory_recall` or `kg_query` with
   `palace=<OLD>` also works.
3. Alias presence. In `<data root>/palace_aliases.json`, the `aliases` map has
   `"<OLD>": "<NEW>"`. The `--json` answer lists each alias key it changed
   under `alias_changes`. The directory `<data root>/<OLD>/` must not exist.
4. If you used `--replace-empty`, confirm `trashed_target` exists on disk.

## Rollback

Run the reverse rename:

```bash
tm memory rename <NEW> <OLD>
```

The reverse rename is allowed because `<OLD>` is an alias of `<NEW>`. It moves
the palace back and leaves `<NEW>` as an alias of `<OLD>`. No alias is keyed by
a live palace id.

The rename does not restore a replaced empty palace. To restore it, move the
directory from `<data root>/.trash/<NEW>-replaced-<timestamp>/` back to
`<data root>/<NEW>/` after the palace is renamed away and the id is free.

To remove an alias, the code has a library function
(`PalaceAliasStore::remove_alias` in trusty-common) but no `tm` command and no
MCP tool. This runbook does not give a hand-edit procedure for
`palace_aliases.json`.

## Known limitations

- `palace_delete` and `dream_status` addressed by the old id fail closed after
  a rename. Use the new id.
- The count check can answer -32603 when an unlocked writer (`kg_*`, room and
  wing changes, `memory_forget`, dream) runs during the rename. The palace is
  already moved. Check the counts with `palace_info` before you retry.
- A write that waits on a palace while renames move its id gives up after four
  re-keys with -32603 ("changed identity 4 times while this write waited ...
  retry when it is idle"). Retry the write when the rename has finished. The
  rename itself gives -32006 when its own lock set keeps changing.
- Chat sessions stranded under an alias directory are tracked in
  [#9612](https://github.com/bobmatnyc/trusty-tools/issues/9612).
- `palace_unalias` frees drawers after a vector id collision. It does not
  remove a palace alias.

## Example (do not run as written)

Rename the `supervisor` palace to `architect`, replacing the existing empty
`architect` palace. This shows the shape of the command only.

```bash
tm memory rename supervisor architect --replace-empty
# renamed palace supervisor -> architect; supervisor now resolves to architect
# replaced empty palace moved to <data root>/.trash/architect-replaced-<UTC timestamp>
```
