# 0067. Version the palace format; refuse newer, migrate N-1 behind a verified backup

- **Status:** Accepted
- **Date:** 2026-10-06
- **Accepted:** 2026-10-06 (Bob, ruling d7)
- **Amended:** 2026-10-15
  ([#9918](https://github.com/bobmatnyc/trusty-tools/issues/9918)): the N-1
  rule is replaced by a chained migration from every supported older format.
  Changed in place: D3 rules 2–4, D4, D6, D7, Reversibility Cost, Consequences,
  Alternatives and Related Decisions;
  see [Amendment 2026-10-15](#amendment-2026-10-15-chained-migration-9918)
- **Amended:** 2026-10-20 ([#10094](https://github.com/bobmatnyc/trusty-tools/issues/10094)):
  the explicit `palace migrate` command wraps the chain; see
  [Amendment 2026-10-20](#amendment-2026-10-20-explicit-palace-migrate-command-10094)
- **Scope:** crate `trusty-memory` (engine store layer after
  [#9271](https://github.com/bobmatnyc/trusty-tools/issues/9271); today
  `trusty-common` `memory_core::store`), the palace directory layout under
  the data root, the export format
- **Reversibility Cost:** High — once a 1.x release stamps format versions on
  user palaces, every later release must honour those numbers and keep the
  migration step from each one to the next (amended 2026-10-15, #9918)
- **Decision Drivers:** owner ruling 2026-10-06 (Bob): the on-disk palace
  format is part of the 1.0 contract, and memory sync lands in 1.x through
  the versioned format plus a migration; the triple-key marker that treats a
  newer version as already migrated
  (`crates/trusty-common/src/memory_core/store/kg_redb/migrate.rs:103`); the
  `*.v2-incompatible` recovery that replaces a store with an empty one;
  chunked embeddings
  ([#9275](https://github.com/bobmatnyc/trusty-tools/issues/9275)), which
  need several vectors per drawer
- **Supersedes / Superseded by:** —

Issue: [#9274](https://github.com/bobmatnyc/trusty-tools/issues/9274) (E6).
Contract: [ADR-0066](0066-trusty-memory-1x-compatibility-contract.md)
surface 3.

## Context

A palace is a directory under `<data_root>/palaces/<id>/`. One live palace
(`trusty-tools`, read 2026-10-06) holds `palace.json`, `identity.txt`,
`kg.redb`, `index.usearch.redb`, `recall.redb`, `chat_sessions.redb`,
`l1_cache.json`, `dream_stats.json`, `last_used`, legacy `kg.db`,
`recall.db` and `index.usearch`, three `*.v2-incompatible` files, a
`kg.redb.pre-4810.bak` and a `kg.redb.pre-compact.bak`. The maintenance
journal is `maintenance_deletions.jsonl`
(`crates/trusty-common/src/memory_core/maintenance_log.rs:40`). BM25
snapshots live under `<palace>/bm25/`
(`crates/trusty-memory/src/bm25_index.rs:16`).

The version markers that exist today are partial and disagree in kind:

| Marker | Where | Behaviour |
|---|---|---|
| `schema_version` | `palace.json` (`crates/trusty-common/src/memory_core/store/palace_store.rs:71`) | Always written as `1` (`:94`) and never checked on read. |
| `KG_TRIPLE_KEY_SCHEMA_VERSION` | `kg_schema` table in `kg.redb` (`crates/trusty-common/src/memory_core/store/kg_store.rs:233`) | `>=` counts as migrated, so a newer palace is opened and written as if current. Migration is fail-open: on error the palace opens un-migrated. |
| `ROOM_SCHEMA_VERSION`, `WING_SCHEMA_VERSION` | marker rows in `kg.redb` (`crates/trusty-common/src/memory_core/store/rooms.rs:36`, `crates/trusty-common/src/memory_core/store/wings.rs:46`) | Record which shape wrote the rows. Nothing gates on them. |
| `SHARE_FORMAT_VERSION` | each exported record (`crates/trusty-common/src/memory_core/share/record.rs:26`) | Refuses a newer version on import and accepts older ones. This is the correct rule. |

Two further hazards come from older binaries. First, the `PalaceJson`
struct has no catch-all field, so any binary drops an unknown `palace.json`
field when it rewrites the file. Second, a pre-1.0 binary rewrites
`schema_version: 1` unconditionally. So a marker in `palace.json` alone does
not survive an older binary touching the palace.

The redb 2 → 4 upgrade added a third path (#702). A writer-intent open of an
unreadable file renames it to `*.v2-incompatible`, creates an empty database
in its place and reports the palace as degraded
(`crates/trusty-common/src/memory_core/store/concurrent_open.rs:334`). For
`kg.redb`, which holds every drawer, that is apparent total data loss
recovered only by hand. #4911 already stopped it under read-only intent.

## Decision

We will give every palace one integer **format version** and apply four rules
to it.

### D1. Where the version lives

Both places, with defined roles:

- **Authoritative:** a `palace_format` key in the existing `kg_schema` table
  of `kg.redb`. It is committed in the same redb write transaction as
  each step of a migration, so the version and the data cannot disagree.
- **Mirror:** a new `format_version` field in `palace.json`. It is read
  before any store opens, so a newer palace is refused without opening redb.
  It is not `schema_version`, which pre-1.0 binaries overwrite with `1`.
- **Missing or disagreeing:** a missing marker means format 0. If either
  location is newer than this binary supports, the palace is refused. If the
  mirror is lower than the marker (an older binary rewrote `palace.json`),
  the marker wins and the mirror is rewritten.
- **From 1.0.0,** `PalaceJson` keeps unknown fields on rewrite, so a later
  1.x field survives an earlier 1.x binary.
- **Existing per-feature markers** (triple key, rooms, wings) are frozen at
  their current values. They are read only to migrate format 0. Later
  changes bump the palace format, not a per-feature marker.

**Format 0** is any pre-1.0 palace. **Format 1** is the 1.0.0 layout: triple
key v1, rooms v1, wings v1, both markers stamped. The 0 → 1 migration runs
the existing at-open migrations under the rules below, then stamps the
version.

### D2. What the format covers

| File | Class | On a format change |
|---|---|---|
| `palace.json`, `identity.txt` | Primary | Migrate |
| `kg.redb`: `drawers`, `triples`, `rooms`, `room_keys`, `wings`, `wing_keys`, `payloads`, `kg_schema` | Primary | Migrate |
| `kg.redb`: `drawers_by_fact_key`, `triples_by_object`, `active_subject_counts` | Derived index in a primary file | Rebuild inside the migration transaction |
| `chat_sessions.redb` | Primary | Migrate |
| `maintenance_deletions.jsonl` (and `.1`) | Primary audit journal; append-only | The line schema is frozen. Never rewritten. A new field is additive. |
| `<data_root>/drawer_counts.jsonl` (amended for [#9283](https://github.com/bobmatnyc/trusty-tools/issues/9283)) | Primary audit journal; one line per palace per UTC day, written by the daemon | The line schema is frozen at `v: 1`; a new field is additive. A reader refuses a line with a newer `v`. The only rewrite is the daemon's 90-day prune, under the file lock, via a temp file and a rename, and it keeps every line it cannot read verbatim. Never blocks a palace open. |
| `index.usearch.redb` (`vectors`, `vector_keys`, `deleted_vectors`, `vector_id_seq`) and the in-memory HNSW graph | Derived: content plus embedder model | Re-index, or migrate when a rebuild is too costly (D6) |
| `<palace>/bm25/`, `l1_cache.json`, `dream_stats.json`, `last_used` | Derived | Discard and rebuild |
| `recall.redb` | Auxiliary telemetry | Migrate if readable. Never blocks an open. |
| `kg.db`, `recall.db`, `index.usearch`, `index.usearch.keymap.json` | Legacy, already migrated | Out of format. Left untouched. |
| Export JSONL (`SHARE_FORMAT_VERSION`) | Interchange | Versioned on its own under the same refuse-newer rule |

Each derived file carries a tag naming the format version that built it. A
derived file whose tag does not match is rebuilt, not migrated or refused.
The project pin file (`.trusty-tools/trusty-memory.yaml`) is not part of a
palace, but it carries its own `schema_version` (`ProjectPin`,
`crates/trusty-common/src/palace_resolve.rs:100`; current value
`PIN_SCHEMA_VERSION = 1`, `:60`) and follows the same refuse-newer rule. A
binary that meets a pin file whose `schema_version` is newer than it
understands refuses to act on it: it does not rewrite the file or drop its
fields, and it reports a clear error naming the file and both versions. Today
the reader (`read_project_pin`, `crates/trusty-common/src/palace_resolve.rs:340`)
never checks the value, and the only writer
(`write_project_pin`, `crates/trusty-memory/src/project_root/pin_file.rs:46`)
re-serialises the whole struct, which drops unknown fields; both change before
1.0.0. ADR-0066 surface 2 points here.

The sync connector's state directory sits beside the palaces, not inside
them (DOC-80 §3), and is outside this format.

### D3. Rules

1. **Refuse newer, fail closed.** A palace whose format is newer than this
   binary's N is not opened, under any intent. No byte under the palace
   directory changes. No quarantine, no lock-file write, no `last_used`
   touch. The error names the palace, both versions, and "upgrade
   trusty-memory to a release that reads format <v>". Other palaces open
   normally. Fixes the `>=` check at `migrate.rs:103`.
2. **Auto-migrate every supported older format, in order** (amended
   2026-10-15, #9918). At open, a writer-intent open migrates a palace at any
   format from `OLDEST_MIGRATABLE_FORMAT` up to N by running each step
   k → k+1 in ascending order, behind one verified backup taken before the
   first step. A read-only or observer open of an older palace is refused,
   and the error tells the caller to start the daemon so it migrates the
   palace. A palace older than `OLDEST_MIGRATABLE_FORMAT` (0 today, so no
   palace) is refused `FormatTooOld`, and the error tells the operator to
   migrate it with a release that reads its format. The upgrade guide
   ([#9282](https://github.com/bobmatnyc/trusty-tools/issues/9282), E14) keeps
   the table of release lines.
3. **Back up before migrating, and abort if the backup fails.**
   - **What:** every primary file in D2 is copied to
     `<data_root>/backups/format-migration/<palace_id>/<from>-to-<to>-<UTC timestamp>/`.
     Derived files are not copied. A chain takes one backup: `<from>` is the
     format before its first step and `<to>` is N (#9918).
   - **Verify:** the copy is fsynced and each file is checked by length and
     content hash against the source. The existing `ensure_verified_backup`
     (`migrate.rs:298`) checks length only, and it reuses any earlier backup
     file of the same length.
   - **Abort:** any copy or verify failure aborts the migration before the
     first write, and the palace stays at its format and refuses to open.
   - **Retention:** the two newest migration backups per palace are kept.
     `doctor` reports their total size. Nothing else deletes them.
   - **Replace the sidecars:** the in-directory `*.pre-4810.bak` pattern is
     not used for format migrations.
4. **Crash-safe means re-runnable.**
   - **Order:** a migration runs as ordered steps, and each step is
     idempotent.
   - **Commit point** (amended 2026-10-15, #9918): each step k → k+1 writes
     the `palace_format` marker k+1 in the same redb transaction as its
     `kg.redb` change. The `palace.json` mirror for k+1 is written just
     before that transaction, by atomic rename, so an older binary refuses
     the palace from the first changed byte. While the migration record
     exists, a mirror above the marker means "resume", not a mismatch.
   - **After a crash:** the marker still says the format of the last
     committed step, so the next writer open resumes the chain from there.
     It starts from the files on disk, never from a staged copy, and keeps
     the backup it already verified. A failed step never advances the marker.
   - **Failure is fail-closed:** a failed migration leaves the palace
     refused, never opened un-migrated. The palace is excluded from recall
     and `doctor` reports it, while the other palaces keep working. This
     changes the fail-open behaviour at `migrate.rs:22`.

Additive at-open backfills that need no version bump (the ADR-0027 pattern:
a new table that older 1.x binaries may ignore) stay fail-open. A change
needs a format bump when an older 1.x binary would misread the new data or
break one of its invariants by writing.

### D4. How a migration is tested

Each format N keeps a checked-in fixture
`crates/trusty-memory/testdata/palace-format/v<N>/`. The fixture is a small
palace written by the last release that wrote format N, built with the mock
embedder and never regenerated by new code. Every release that writes N+1
runs these tests:

- the v<N> fixture migrates, drawer and triple counts plus sample contents
  are unchanged, and the backup is byte-identical to the fixture;
- a synthetic v<N+2> copy is refused, and a hash of every file is identical
  before and after;
- a backup into an unwritable directory aborts, and the fixture bytes are
  unchanged;
- a fault hook kills the migration after each step, and the next open
  completes it;
- (amended 2026-10-15, #9918) every format below N+1 has a fixture and a
  `kg.redb` step, and each fixture migrates to N+1 through the chain. A
  length check fails a format bump that adds no step or no fixture. Format 0
  has a hand-written builder, not a checked-in blob.

### D5. The `*.v2-incompatible` quarantine

The recreate-empty path is retired for primary files. An unreadable primary
file is now handled under rule 1: the palace is refused, its bytes stay where
they are, and the error says which file and why. For a derived file the
existing recovery stays (rename aside, rebuild), because rebuilding loses
nothing. Existing `*.v2-incompatible` files are left in place. They are
never deleted or imported automatically, and `doctor` lists them with their
size. Recovering one is a manual step that the E14 guide documents.

### D6. Planned 1.x migrations under this ADR

- **Chunked embeddings**
  ([#9275](https://github.com/bobmatnyc/trusty-tools/issues/9275), E7). The
  change is format 1 → 2 on derived data. `vector_keys` may hold k rows per
  drawer: `{uuid}#{n}` for chunk n, the bare uuid for chunk 0. The key
  grammar makes every existing row a valid one-chunk drawer, so the
  migration rewrites no row and does not re-embed. It stamps format 2 at the
  first writer-intent open, before any store opens and so before any
  `{uuid}#{n}` row can exist. The stamp is a fence: a 1.x binary that reads
  format 1 would skip, orphan or, in its dream rebuild, discard chunk rows,
  and the stamp makes it refuse the palace (`FormatTooNew`) instead. A
  format-0 palace runs the 0 → 1 step first, then this one (amended
  2026-10-15, #9918).
  Re-embedding the drawers over the window is separate work that follows
  this stamp; until it runs, such a drawer keeps its one truncated vector.
  The backup covers primary files only, so it stays small.
  - **Rollback:** restore the backup (Consequences, Restore), then delete
    the palace's `index.usearch.redb`. The vector index is derived and is not
    backed up; left in place after a restore it can hold chunk rows under a
    format-1 palace, which a format-1 binary misreads. Then run the
    `palace_reembed` tool with `dry_run: false` to embed the drawers into a
    fresh index. In 0.33.0 `dry_run` defaults to `true`, which re-embeds
    nothing.
  - **trusty-memory 0.32.0 and 0.32.1** gate at format 0, so they already
    refuse a format-1 or format-2 palace (`FormatTooNew`). **Binaries older
    than 0.32.0** carry no format gate, so no stamp can fence them off. Do not
    open a format-2 palace with one.
- **Memory sync** (1.x, DOC-80). The change is N → N+1 on primary data. It
  adds the typed drawer fields (OQ-4), the outbox table, tombstones and
  suppression markers. It needs a bump, not a backfill, because an older 1.x
  binary that writes a drawer without the outbox append in the same
  transaction would break sync without any error. The binding stays out of
  `palace.json` (DOC-80 OQ-3). The project UUID that ADR-0051 is amended to
  carry lands in the same bump.

### D7. Decisions recorded at review

Decided (Bob, 2026-10-06, "adopt all"), each as recommended in the Architect
review:

- **Retention.** Decided (Bob, 2026-10-06): keep the two newest migration
  backups per palace, with no age-based expiry; only an operator deletes a
  backup. A successful open does not prove the migrated data is correct.
  Silent drawer loss is detected only later, by count history
  ([#9283](https://github.com/bobmatnyc/trusty-tools/issues/9283)), and an age
  rule would delete the only restore point first. `doctor` reports the total
  size (D3 rule 3).
- **N-2 path.** ~~Decided (Bob, 2026-10-06): no `palace migrate --chain`
  command in 1.x; the E14 table of release lines is the N-2 path. The owner's
  N-1 rule keeps one migration per binary. A chain would keep every past
  migration in the binary and multiply the D4 fixture matrix, which is why
  "auto-migrate any older format" is rejected below.~~ Replaced 2026-10-15
  (#9918): the writer open runs the chain itself, with no separate command.
  A step is never removed while a supported format needs it. Dropping a
  format needs a superseding ADR, `OLDEST_MIGRATABLE_FORMAT` raised in the
  same PR, and the `FormatTooOld` message naming the last release that reads
  the dropped format.
- **Read-only access to a refused palace.** Decided (Bob, 2026-10-06): a
  failed-migration palace stays fully offline in the binary that failed. The
  backup is a complete palace at the format the migration started from
  (#9918), so an operator who needs its data restores it (Consequences) and
  reads it with the release line that writes that format. A second read path into a backup directory would be a new surface with
  its own tests, used only in a failure case.
- **`recall.redb` class.** Decided (Bob, 2026-10-06): `recall.redb` is
  auxiliary. It holds hit/miss telemetry
  (`crates/trusty-common/src/memory_core/analytics.rs:1-15`), a failure to open
  it already leaves the palace usable
  (`crates/trusty-common/src/memory_core/retrieval/handle.rs:525-541`), and
  recall ranking does not read it. E13 measures recall against a checked-in
  known-answer corpus ([#9281](https://github.com/bobmatnyc/trusty-tools/issues/9281)),
  not against this log.
- **Memory id minting for sync.** Decided (Bob, 2026-10-06): `memory_id`s are
  minted eagerly, inside the sync N → N+1 migration and under the verified
  backup. Every drawer then has a `memory_id` once format N+1 is stamped,
  which the D4 fixture test can assert. Lazy minting would make publish a
  writer of primary data outside any migration. The DOC-80 draft (PR #9176)
  already requires a daemon-side map from `memory_id` to the local drawer id,
  so the eager write fills a table that must exist anyway.

## Consequences

**Easier:**

- A downgrade, or a second machine on an older release, can no longer
  corrupt a palace in silence. It gets a refusal that names the fix.
- One marker and one rule set replace three per-feature markers, so a new
  format change needs no new gate logic.
- E7 and memory sync each have a defined landing path, a test shape and a
  rollback, which is the backup.

**Harder:**

- Once a palace migrates, a downgrade within 1.x is not supported. Restore
  means using the backup and losing the writes made since.
- Every release carries one migration step per past supported format, and
  one fixture per format (D4) (amended 2026-10-15, #9918).
- **Accepted: a failed migration takes that palace offline.** The palace is
  refused, never opened un-migrated, where today it opens degraded
  (`migrate.rs:22`). This is fail-closed, in line with the 1.0 data-loss
  rule. The cost is that palace's availability until an operator repairs or
  restores it. The other palaces keep working. The operator recovers in one
  of two ways, which the E14 guide documents:
  - **Re-run:** fix the cause that `doctor` reports (disk space,
    permissions, an unreadable file), then restart the daemon. The next
    writer-intent open resumes from the format `kg.redb` holds, on the files on disk (D3 rule 4).
  - **Restore:** copy the primary files back from
    `<data_root>/backups/format-migration/<palace_id>/<from>-to-<to>-<UTC timestamp>/`.
    The palace is then at the format the migration started from again, and
    the next open migrates it, or a release from the line that writes that
    format reads it.
- Each migration release adds a fixture to `testdata/` for good.
- The 0 → 1 migration touches the whole estate (116 directories under
  `palaces/` on the owner's machine) on the first 1.0 open, so its backup
  disk cost is paid once and up front.

### Open questions for review

None remaining at review (2026-10-06).

## Alternatives considered

- **Version only in `palace.json`.** Rejected: pre-1.0 binaries rewrite it
  to `1` and drop unknown fields, and the field cannot commit atomically with
  redb data.
- **Version only in a redb marker.** Rejected as the sole location: refusing
  a newer palace would require opening redb, which takes a lock and on a
  redb format change can itself fail before the marker is read.
- **Keep per-feature markers.** Rejected: three markers already disagree in
  semantics (gate vs record), and each new change would add a marker and a
  gate.
- **Auto-migrate any older format without a bound.** Rejected. The
  2026-10-15 amendment (#9918) migrates every format from
  `OLDEST_MIGRATABLE_FORMAT` up, one step and one fixture per format, so the
  test matrix grows by one fixture per format bump.
- **Migrate into a staged copy and swap the directory.** Rejected for now:
  a directory swap is not atomic with a live daemon holding locks on files
  inside it. Idempotent steps with a redb commit point give re-runnability
  at lower cost.

## Related Decisions

Vetted against `docs/adr/INDEX.md` and prior decisions on 2026-10-06:

- **ADR-0066 (trusty-memory 1.x compatibility contract):** Extends. This ADR
  is ADR-0066's surface 3. After the 2026-10-15 amendment its "auto-migrate
  N-1" wording for surface 3 reads as this ADR's D3 rule 2: every supported
  older format migrates, in order.
- **ADR-0027 (Rooms, wings and closets):** Consistent. Its additive,
  at-open, fail-open backfill stays legal for changes that need no bump
  (D3). A format migration may re-encode rows but never reclassifies,
  renames or drops a drawer.
- **ADR-0028 (Memory recall tiers):** Consistent. `fact_key` and
  `drawers_by_fact_key` are part of format 1. Its rule that no drawer row is
  deleted or rewritten in meaning holds for every migration.
- **ADR-0051 (Palace id stays hyphen-joined):** Consistent. Its optional
  `owner`/`project` fields and the planned project UUID are `palace.json`
  changes that follow D1's unknown-field rule and, where needed, a bump.
- **ADR-0007 (Tool contract versioning):** Consistent. The format version is
  a monotonic integer, in line with ADR-0007's level model. It is separate
  from tctl's `contract_version`.
- **ADR-0068 / 0069 / 0070 (memory sync; Proposed, PR
  [#9176](https://github.com/bobmatnyc/trusty-tools/pull/9176), unmerged):**
  Consistent. Sync's palace-side changes land as one N → N+1 migration
  (D6). The connector state directory and the sync wire format are outside
  this format.
- **ADR-0029 (MSRV and edition policy):** No interaction.

## Amendment 2026-10-15: chained migration (#9918)

**Why:** trusty-memory 0.35.0 refused every format-0 palace `FormatTooOld`
when an upgrade skipped 0.33.x, and no release after 0.33.x carried the
0 → 1 step (PR [#9739](https://github.com/bobmatnyc/trusty-tools/pull/9739)
removed it). The N-1 rule stranded those palaces. This amendment replaces
D3 rule 2 and the D7 "N-2 path"; the edits above are marked with the date.

**What the binary does** (`crates/trusty-memory/src/engine/store/`, PRs
[#9920](https://github.com/bobmatnyc/trusty-tools/pull/9920) and
[#9921](https://github.com/bobmatnyc/trusty-tools/pull/9921)):

- **Steps.** `kg_redb::migrate::STEPS` holds step k → k+1 at index k: the
  0 → 1 triple-key rewrite restored from 0.33.0, and the 1 → 2 stamp. The
  test `the_step_table_has_one_step_per_older_format` fails when
  `PALACE_FORMAT_SUPPORTED` rises without a new step.
- **Classification.** `check_format` returns `FormatNeedsMigration` for any
  format from `OLDEST_MIGRATABLE_FORMAT` (0) up to N-1, and `FormatTooOld`
  only below that bound, which no palace reaches today.
- **One backup, one record, one lock per chain.** The backup directory is
  `<from>-to-<N>-<UTC>`, taken before the first step. The migration record
  names it and is removed after the last step.
- **Each step.** The `palace.json` mirror is written first, by atomic rename.
  The `kg.redb` step then reads both markers inside its write transaction,
  fails before any write unless they name the format it moves from, and
  commits its table work and both markers together. The 0 → 1 step needs no
  `palace_format` row and a triple-key marker that is absent (legacy keys,
  rewritten) or v1; the 1 → 2 step needs format 1 and triple key v1. A failed
  `kg.redb` step is `FormatMigrationFailed` naming that step, and the marker
  stays at the step's starting format.
- **Resume.** The next writer open starts the chain at the format `kg.redb`
  holds, behind the backup the record names, after that backup verifies. A
  record of a 0 → 1 move left by an interrupted 0.33.x migration is resumed
  to N behind its own backup instead of refused.
- **Read-only and observer opens** of an older palace, or of one with a
  migration record, are refused; the read-only refusal is
  `FormatNeedsMigration`.

**Release lines:** trusty-memory 0.32.x and older wrote format 0, 0.33.x
wrote format 1, and 0.35.0 and later write format 2.

**Later format bumps** append one step and one fixture (D4) and keep every
older step.

## Amendment 2026-10-20: explicit `palace migrate` command (#10094)

The explicit command `trusty-memory palace migrate [<palace> | --all]
[--dry-run | --verify] [--yes]` now wraps the chain above. It adds no second
chain: it runs the same migration as the writer open, behind the same verified
backup, then checks the result. It goes through the daemon when one runs. D7's
"no command" line was already marked superseded (2026-10-15, #9918); this
amendment records that the command now exists. Usage and rollback:
[backup-restore-upgrade.md](../trusty-memory/backup-restore-upgrade.md).
