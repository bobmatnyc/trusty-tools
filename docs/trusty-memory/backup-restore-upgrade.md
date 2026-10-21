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

Restore is offline only. Stop the daemon that serves this data root first.

`trusty-memory stop` has no flag to pick a daemon. Without
`TRUSTY_DATA_DIR_OVERRIDE`, it stops every trusty-memory daemon you run,
including the one launchd supervises on macOS. Every MCP client that uses
trusty-memory loses its memory tools until the daemon is back. It spares the
per-session stdio bridges. With `TRUSTY_DATA_DIR_OVERRIDE` set, it stops only
the daemon serving that data root's socket.

```bash
trusty-memory stop
trusty-memory palace restore <SNAPSHOT> [--palace <ID>]
```

After the restore, run `trusty-memory start`. Under launchd on macOS, run
`trusty-memory service start` instead. The hint that `palace restore` prints
names `trusty-memory start` in every case.

`<SNAPSHOT>` is a backup directory written by `palace backup`. It holds
`manifest.json`. `--palace` names the palace to restore. It defaults to the id
in the backup's manifest.

Restore fails closed. It refuses when:

- a daemon is serving this data root, or the command cannot tell whether one
  is;
- the backup is of another palace than `--palace`;
- the palace id in the backup's manifest is invalid;
- the backup is in a newer palace format than this binary supports.

Restore verifies every checksum and row count before it writes anything. It
writes only into an empty palace directory, so it never replaces a palace. The
target is `<data root>/palaces/<id>` when `<data root>/palaces` exists (the
legacy layout), otherwise `<data root>/<id>`. `TRUSTY_DATA_DIR_OVERRIDE` is
honoured.

### Re-embed after a restore

The backup has no vector index. Start the daemon, then call the MCP tool
`palace_reembed` with `dry_run=false`. In that call, `missing` is the count
before the repair, so it can still be above zero next to a matching `repaired`.
Read `repaired`. Then call `palace_reembed` again, with `dry_run=true` or
`false`, and confirm it reports `missing: 0`. Repeat until it does. Recall
finds the restored drawers only after that.

## Recover from a crash during restore

A crash can leave a directory named `.<id>.restore.tmp` next to the target
palace directory. Restore never reuses an existing one, so the next restore
fails until you remove it. Stop the daemon, then remove that directory. Run
the restore again.

## Upgrade across a palace format change

### Which release writes which format

| Release | Palace format |
|---|---|
| 0.32.x and older | 0 |
| 0.33.x | 1 |
| 0.35.0 and later | 2 |

Every release from 0.35.0 reads format 2. From 0.36.0 (unreleased; ships with
trusty-mpm 1.9.0), a binary migrates a palace at format 0 or 1 in one chain,
with no intermediate release (ADR-0067, amendment 2026-10-15). 0.35.0 migrates
only format 1: a format-0 palace needs the two-step path in
[Upgrading from 0.32.x or older to 0.35.0](#upgrading-from-032x-or-older-to-0350-formattooold).
A palace in a newer format than the binary supports is refused, not opened.

### Automatic migration

The daemon migrates a palace when it makes its next writer open of it, which
is at daemon start for every palace. Before the first step, it writes a
verified copy of the palace's primary files and aborts the migration if that
copy fails. You do not need to run anything.

A read-only open of an older palace (the CLI, `trusty-memory serve --stdio`) is
refused with `FormatNeedsMigration` until the daemon has migrated it. The
format 1 to 2 step is on derived data and rewrites no row.

### Migrate by hand with `palace migrate`

`trusty-memory palace migrate` runs the same chain as the daemon, as a step you
can preview and check. It is not in trusty-memory 0.35.x. It ships in 0.36.0.

```bash
trusty-memory palace migrate my-palace --dry-run   # preview; writes nothing
trusty-memory palace migrate my-palace             # backup, convert, check
trusty-memory palace migrate --all                 # every palace
trusty-memory palace migrate --all --yes           # confirm more than one change
```

- `--dry-run` prints one row per palace: its status (`current`,
  `needs migration`, `interrupted`, `unverified` or `refused`), the format
  found, the target, the size, the space the backup needs, the free space and
  the planned backup directory. It then prints how many palaces would change.
- A plain run takes the verified backup, runs each format step, then runs the
  post-checks below. It prints the backup directory, the drawer and triple
  counts before and after, and `checks:` or `FAILED:` lines.
- A palace already at the current format is left alone. No backup is taken.
- An interrupted migration resumes from the format `kg.redb` holds.
- `--all` prints the dry-run table first and checks the total free space. It
  refuses without `--yes` when more than one palace would change, and changes
  nothing. It continues past a palace that fails and names each one
  that failed or was refused.

The post-checks verify that:

- the format status is `current`;
- the `kg.redb` markers (`palace_format` and `triple_key`) and the
  `palace.json` `format_version` agree at the expected value;
- no `format_migration.json` is left;
- the drawer and triple counts equal the counts before the migration.

A failed check makes the command exit non-zero. The message names the palace,
the gap and the verified backup, and states that nothing was deleted.

The command goes through the running daemon when one serves this data root.
With no daemon, it opens the palaces itself, offline. A daemon's answer is
final: a failed call is never retried offline.

After `cargo install`, a daemon that was already running is the old release. It
does not know `palace_migrate`, and the command says so. Restart the daemon on
the new release (`trusty-memory stop`, then `trusty-memory start`), which
migrates its palaces at its next writer open. Or stop it and run the command,
which then migrates offline.

`trusty-memory doctor` prints a `needs migration` warning for each palace that
is behind. The warning names the command and the backup root.

### Palaces that tm owns

The Session Manager and portfolio palaces belong to the `tm` daemon. Do not run
`palace migrate` on them. Restart the owning daemon (`tm daemon run`). It
migrates them at its next open.

### Where the migration backups live

The backup directory is
`<data root>/.trusty-memory/backups/format-migration/<palace>/<from>-to-<to>-<UTC>/`,
or `<data root>/backups/format-migration/<palace>/…` in the legacy layout. A
chain from format 0 writes one `0-to-2-<UTC>` directory, taken before its first
step.

- It holds the palace's primary files: `palace.json`, `identity.txt`,
  `kg.redb`, `chat_sessions.redb` and the two deletion journals. It also holds
  a `MANIFEST` file with the length and SHA-256 of each.
- The two newest complete backups per palace are kept. Only you delete a
  backup.
- It does not hold the vector index (`index.usearch.redb`). The index is
  derived data, rebuilt from the drawers. After a rollback, re-embed (see
  [Roll back](#roll-back)).
- It is not a `palace backup`. It has `MANIFEST`, not `manifest.json`, and
  `palace restore` does not read it.

### Check a palace you converted by hand

```bash
trusty-memory palace migrate my-palace --verify
trusty-memory doctor
```

`--verify` writes nothing. It runs the post-checks on any palace and prints
one block per palace. It reads `kg.redb` read-only, or through a temp copy when
another process holds the file (the line ends `(read from a copy)`).

| Line | Meaning |
|---|---|
| `kg_schema palace_format` | The format stamped in `kg.redb`. |
| `kg_schema triple_key` | The triple-key layout marker. |
| `palace.json format_version` | The mirror of the format in `palace.json`. |
| `format_migration.json` | `present` means a migration was interrupted. |
| `backup` | The newest complete migration backup, and whether it still verifies. A missing or failing backup is reported, not counted as a gap. |

Each marker reads `present (<n>)`, `missing`, `mismatched (<n>, want <m>)` or
`not applicable (new, empty KG)`. The last is a palace with no data.

The block ends with `OK: every check passed` or one `GAP:` line per failed
check. With any gap, the command exits non-zero and names the palace and the
gap. Fix the marker, or run `palace migrate <palace>` to finish the job.

`trusty-memory doctor` reads each palace without writing. In its `palace
format` line, a palace behind the binary is a warning, one it cannot read
(held by a daemon, or in need of crash repair) is a warning, and a palace it
refuses (newer, split markers or unreadable) is a failure.

### Upgrading from 0.32.x or older to 0.35.0 (FormatTooOld)

The published trusty-memory 0.35.0 migrates only format 1, so it refuses a
format-0 palace with `FormatTooOld`. Palaces written by 0.32.x or older are at
format 0, so they need the 0.33.0 step. Upgrade in two steps, through 0.33.0,
the newest 0.33.x release:

1. Stop the daemon.
2. Install 0.33.0: `cargo install trusty-memory --version 0.33.0 --locked`.
3. Start its daemon once: `trusty-memory start`. It opens each palace and
   migrates format 0 to format 1. Before it migrates, it writes a verified
   backup under `<data root>/backups/format-migration/<palace>/`.
4. Stop the daemon, then run `trusty-memory doctor`. The `palace format` check
   passes with "every palace reads at format 1". It warns if a palace still
   needs migration. In that case, start the daemon again and let it finish.
5. Install 0.35.0 (or the current release):
   `cargo install trusty-memory --version 0.35.0 --locked`.
6. Start its daemon. It migrates format 1 to format 2.

trusty-memory 0.36.0, which ships with trusty-mpm 1.9.0, migrates format 0
directly and adds `trusty-memory palace migrate` (#10094).

## Roll back

### With a migration backup

Use this after `palace migrate` or the daemon migrated a palace and you need
the old format back. Writes made since the migration are lost.

1. Stop the daemon: `trusty-memory stop`. Close every MCP client first, or it
   restarts the daemon.
2. Pick the backup. It is the newest directory that holds `MANIFEST` and whose
   name starts with `<old format>-to-`, for example `1-to-2-<UTC>` to return to
   format 1. A palace can hold both `0-to-2-*` and `1-to-2-*` directories, so
   the newest one overall can be the wrong one. Copy from it; never move or
   edit it.
3. Copy every file in that directory except `MANIFEST` back into the palace
   directory.
4. Remove `format_migration.json` from the palace directory, if it is there.
5. Delete `index.usearch.redb` from the palace directory. The backup does not
   hold it, so the migrated index stays behind. It can hold chunk rows
   (`{uuid}#{n}`) that an older binary misreads, and orphan vectors for drawers
   written after the migration (ADR-0067 D6).
6. Run the release that matches the old format: 0.33.x for format 1, 0.32.x or
   older for format 0. Start its daemon. It creates an empty index.
7. Call `palace_reembed` with `dry_run=false` until it reports `missing: 0`.
   Recall finds the drawers only after that.

This procedure copies into the existing palace directory, which is why step 5
is needed. The step-by-step section below restores into a fresh directory, so
no index exists there.

The detailed form for a format 2 to 1 rollback, with checksum checks, follows.

### Format 2 to 0.33.0, step by step

A binary refuses a palace in a newer format than it supports. After an upgrade
to format 2, trusty-memory 0.33.0 refuses the migrated palace, and a
`palace backup` taken after the migration is also format 2. To go back to the
older version you need a backup in the older format. For the format 1 to 2
upgrade, that is the migration copy described above.

1. Install the older version
   (`cargo install trusty-memory --version 0.33.0 --locked`). Close every MCP
   client that uses trusty-memory, then run `trusty-memory stop`. A running
   client restarts the daemon.
2. In the migration backup directory, pick the newest `1-to-2-<UTC>` directory
   that holds `MANIFEST`. Never use a `0-to-1-*` directory or one ending in
   `.tmp`. A `0-to-1-*` copy loses every write made under 0.33.0. Copy from the
   directory you pick; never move or edit it.
3. Move the upgraded palace directory out of the palace root, for example to
   `~/trusty-memory-rollback/<id>`. Any directory in the palace root that holds
   `palace.json` is read as a palace.
4. Make a staging directory on the same filesystem, outside the palace root.
   Copy every file except `MANIFEST` into it, and check each file's SHA-256
   against `MANIFEST`. Then rename the staging directory to the palace
   directory.
5. Start the 0.33.0 daemon and call `palace_reembed` with `dry_run=false` until
   it reports `missing: 0`. In 0.33.0, `dry_run` defaults to `true`, which
   re-embeds nothing.

`MANIFEST` is JSON, not a `shasum` list. This command prints each file's
expected hash and compares it. Run it inside the staging directory, with the
`MANIFEST` path in place of `<backup>/MANIFEST`. It exits non-zero on any
mismatch:

```bash
grep -E '"(name|sha256)"' <backup>/MANIFEST | sed -E 's/.*": "([^"]*)".*/\1/' \
  | paste - - | while read n h; do echo "$h  $n"; done | shasum -a 256 -c -
```

A `palace backup` taken before the upgrade, with a trusty-memory release that
has the command, restores with `trusty-memory palace restore` instead. Restore
does not read the migration copy (ADR-0067, Consequences).

A restore never writes `index.usearch.redb`, so none exists in a restored
palace. Writes made after the backup are lost.

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
  data root, and the backup keeps this palace's lines. After a rollback, this
  palace's lines in `drawer_counts.jsonl` are stale.
