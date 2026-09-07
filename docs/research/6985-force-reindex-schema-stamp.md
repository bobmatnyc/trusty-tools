# Preserve applied migrations across forced corpus replacement

## Why

A forced reindex creates a fresh `index.redb.tmp` and promotes that file over
`index.redb`. Its staging seed preserves contributed graphs, but omits the
live corpus's migration stamp. A subsequent cold open consequently sees
schema version zero and replays applied migrations, including M005, which can
change duplicate-text vector coverage after an otherwise successful recovery.

## What

Inside the existing blocking staging task, copy the live corpus's exact
nonzero `schema_version` on the force path before installing staging. An
unversioned corpus remains unversioned; a version-four corpus remains at four.
Only the migration runner may advance the stamp after applying a migration.

Reuse the existing corpus metadata reader and writer. Read/write errors follow
the existing force fallback to the live corpus, without promoting staging.
Incremental staging already copies the stamp in `copy_all_from`. Contributed
graph copying and the force path's empty derived-row starting point remain
part of the contract. Force runs neither consume nor write resume checkpoints.
Migration and reindex entry points already share the per-index semaphore, so
this snapshot cannot race a migration that advances the live stamp.

## Test

Exercise actual staging, promotion and cold redb reopen for absent, prior and
current versions on force and incremental paths; inspect raw metadata to prove
an absent stamp was not fabricated. Verify force staging starts without old
chunks, while incremental staging carries them. Cover abort, staging-open
failure and a per-call metadata read failure, preserving the live stamp and rows.

Drive the full forced reindex with two files containing identical text, reopen
both durable redb and HNSW, and run the exclusive migration entry point. The
current stamp and vector coverage for both file IDs must survive that restart.
Capture the regression failure before changing production code, then run the
focused corpus, migration, resume and contributed-graph suites. This persistence
change requires independent review and the repository's rung-five gates.
