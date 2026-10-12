# trusty-memory: back up, restore and upgrade a palace

This guide covers backing up one palace, restoring it, upgrading across a
palace format change, and rolling back. The feature is tracked in
[#9282](https://github.com/bobmatnyc/trusty-tools/issues/9282). The compatibility
rules behind it are in
[ADR-0066](../adr/0066-trusty-memory-1x-compatibility-contract.md) and
[ADR-0067](../adr/0067-versioned-palace-format-with-n-minus-1-migration.md).

## Make a backup

Run `trusty-memory palace backup <NAME>`, where `<NAME>` is the palace id.

```bash
trusty-memory palace backup my-palace
trusty-memory palace backup my-palace --dest before-upgrade
```

The command writes a consistent backup of one palace. It goes through the
running daemon when one serves this data root. The palace stays live and
writers are not paused.

With no daemon, the command opens the palace directly. That open is not
read-only. It takes the exclusive redb lock on `kg.redb` until the backup ends,
so a daemon started meanwhile cannot open the palace. It never migrates. A
palace in an older format is refused. Start the daemon, which migrates it, and
back up through it.

An MCP client can call the tool `palace_backup` instead. It takes `palace`
(required unless the server was started with `--palace`) and the optional
`dest` described below.

### Where backups go

Backups go under the palace's backup root:

- `<data root>/.trusty-memory/backups/manual/`
- `<data root>/backups/manual/` when palaces live under `<data root>/palaces`
  (the legacy layout)

The default backup name is `<palace>/<UTC timestamp>`.

### Choose a name with `--dest`

`--dest <NAME>` is the relative path of the backup directory to create under
the backup root. The rules:

- Use plain names only.
- An absolute path, `..`, `.`, an empty segment or a symlinked segment is
  refused. No backup is written and the command exits `1`.
- The directory must not exist.

### What a backup holds

A backup copies:

- `kg.redb`
- `chat_sessions.redb`
- `palace.json`
- `identity.txt`
- the deletion journals
- this palace's `drawer_counts.jsonl` lines

It then writes `manifest.json` with row counts and a SHA-256 per file.

The vector index is not backed up. A restored palace is re-embedded (see
[Restore a backup](#restore-a-backup)). Backups are never pruned. Delete old
ones yourself.

## Restore a backup

Restore is offline only. Stop the daemon first:

```bash
trusty-memory stop
trusty-memory palace restore <SNAPSHOT> [--palace <ID>]
```

`<SNAPSHOT>` is a backup directory written by `palace backup`. It holds
`manifest.json`. `--palace` names the palace to restore. It defaults to the id
in the backup's manifest.

Restore fails closed. It refuses when:

- a daemon is serving this data root, or the command cannot tell whether one
  is;
- the backup is of another palace than `--palace`;
- the palace id is invalid.

Restore verifies every checksum and row count before it writes anything. It
writes only into an empty palace directory, so it never replaces a palace. The
target is `<data root>/palaces/<id>` when `<data root>/palaces` exists (the
legacy layout), otherwise `<data root>/<id>`. `TRUSTY_DATA_DIR_OVERRIDE` is
honoured.

### Re-embed after a restore

The backup has no vector index. Start the daemon, then call the MCP tool
`palace_reembed` with `dry_run=false` until it reports `missing: 0`. Recall
works lexically before that. It finds the restored drawers by meaning only
afterwards.

## Recover from a crash during restore

A crash can leave a directory named `.<id>.restore.tmp` next to the target
palace directory. Stop the daemon, then remove that directory. Run the restore
again.

## Upgrade across a palace format change

The supported upgrade step is N-1 to N. Palace format 1 (trusty-memory 0.35.0)
upgrades to format 2. The daemon migrates a palace lazily, when it opens it.
The format 1 to 2 change is on derived data and rewrites no row.

Before you upgrade:

1. Back up every palace you care about with `trusty-memory palace backup`.
2. Install the new trusty-memory and start the daemon.

A palace in a newer format than the binary supports is refused, not opened.

## Roll back

To go back to the older version after an upgrade:

1. Stop the daemon.
2. Restore the backup with `trusty-memory palace restore`. The target palace
   directory must be empty, so move the upgraded palace aside first.
3. Delete `index.usearch.redb` in the restored palace, if present.
4. Start the daemon (of the older version) and call `palace_reembed` with
   `dry_run=false` until it reports `missing: 0`.

## Known limits

- Backup path checks use `lstat` and `mkdir`, which never follow a symlink at
  the last segment. A same-user writer that swaps a segment for a symlink
  between the walk and the re-check can make at most one empty directory
  outside the backup root, and the re-check then refuses the backup. A swap
  after the re-check and before the final rename commits can redirect the
  backup. See ADR-0066 D7, "`palace_backup` `dest`".
- Two first backups that share a parent directory that does not exist yet can
  race, and one can fail. Retry it.
- Restore does not write `drawer_counts.jsonl` back. That file belongs to the
  data root, and the backup keeps this palace's lines.
