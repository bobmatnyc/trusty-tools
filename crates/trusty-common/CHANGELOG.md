# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.34.0] — 2026-08-14

### Added

- `search_index::index_drop_stats` (and `IndexDropStats`) report how much
  incremental index work this process has lost and how long ago
  ([#2798](https://github.com/bobmatnyc/trusty-tools/issues/2798)), so a
  saturation episode is readable state rather than only a `warn!` line. The two
  losses are separate numbers because they need different fixes:
  `dropped_batches` counts batches the pool refused at submission (none of the
  batch ran), `truncated_batches` counts batches it accepted and started and
  then cut short at the 30s budget (part of the batch landed, the rest was
  abandoned). Reporting only drops would read `0` throughout an episode in which
  every batch is accepted and then truncated. `0` means that loss has never
  happened; each `seconds_since_last_*` is `None` until the first one of its
  kind. trusty-code's `GET /health` publishes all four. `IndexDropStats` is
  `#[non_exhaustive]`, so the two added fields are not a breaking change.
- `gh::GhCommand` — the workspace's single entry point for invoking the GitHub
  CLI ([#5475](https://github.com/bobmatnyc/trusty-tools/issues/5475)), behind
  the new `gh-cli` feature (implied by `tickets`). It renders `gh <args>` with
  an optional `--repo`, working directory, and environment overlay/removals,
  and runs it blocking, on tokio, or hands back the unspawned
  `std::process::Command` for call sites with their own timeout machinery.
  Every runner returns the full exit/stdout/stderr triple in `GhOutput` and
  never decides that a non-zero exit is fatal — `gh pr checks` reports check
  state through its exit code, so that policy stays at the call site.
  `GhOutput::ok`, `nonempty_stdout`, `json`, and `gh_available` are the shared
  policies on top; a missing binary is classified as `GhError::NotInstalled`
  with the `gh auth login` hint rather than an opaque IO error.
- `tickets`' GitHub backend resolves its `gh auth token` fallback through that
  entry point. Behaviour is unchanged, including the post-trim blank-output
  rejection: `gh auth token` exits non-zero when no account is logged in, but
  with `GH_TOKEN="   "` it exits ZERO printing whitespace, which a status-only
  check would pass on as a credential.
- `PalaceRegistry::open_error_is_absent` answers whether an `open_palace` failure means the palace is genuinely not there. `open_palace` returns `anyhow::Error`, which flattens a missing `palace.json` together with a denied read, a transient `EIO`/`ESTALE`, undecodable metadata, an open-queue timeout, and a redb write-lock conflict — so callers that mapped `Err` to "not found" reported a palace they could not read as one that does not exist. The classification lives next to `open_palace` because that is the only place that knows its failure modes (#5549, ADR-0045).
- `daemon_guard::DaemonAddrLayout` resolves a daemon's base URL from its
  on-disk discovery files
  ([#5670](https://github.com/bobmatnyc/trusty-tools/issues/5670)). The logic
  was private to trusty-search's CLI, so a crate that has to probe that daemon
  without depending on it — `tga` — had no way to ask. `resolve_base_url()`
  prefers the `host:port` address file, proving it reachable over TCP first
  (#117), falls back to the port-only file, and finally to the layout's default
  port; the address file is refreshed when the fallback address answers.
  `DaemonAddrLayout::TRUSTY_SEARCH` is the shipped layout, and
  `discovery_file_path()` / `port_file_path()` expose the two paths on their
  own. Behaviour is unchanged from the trusty-search original, including the
  asymmetric fallback directories: with the isolation env var unset the address
  file sits under `$HOME` and the port file under the platform data-local dir.
- `daemon_guard::write_addr_file_atomic` writes a discovery line via
  tmp-file + fsync + rename, so a reader never observes a torn file. It is the
  one writer behind both the daemon's own write and the resolver's stale-file
  refresh.
- `daemon_guard::spawn_detached(program, args)` spawns any named binary as a detached background process with all stdio null-ed. A guard does not always boot its own binary — `tga audit` has to start `trusty-analyze`, a sibling it resolves by env override or PATH (#5670) — and the alternative was a second copy of the `Command::new(…).stdin(null)…` dance in another crate. `spawn_current_exe` is now that call with `current_exe()` resolved first, so the detached-spawn capability keeps exactly one implementation.

### Fixed

- `search_index::index_files_best_effort` no longer spawns an unbounded number
  of detached OS threads
  ([#2798](https://github.com/bobmatnyc/trusty-tools/issues/2798)). Every
  incremental index batch now goes through one shared pool: at most 4 run
  concurrently, at most 64 more queue behind them. Against a degraded but
  reachable trusty-search daemon — where #2785's retry lets a single file take
  up to ~6.2s — threads used to pile up faster than they drained, with nothing
  pushing back. At saturation a batch is DROPPED rather than blocked or queued
  without limit: blocking would stall the agent task the fail-open contract
  exists to protect. The caller contract is unchanged — still non-blocking,
  still fail-open.
- A batch also stops after a 30s budget. A `write_files` call has no size limit,
  so one large scaffold write is a single job; without the budget it would hold
  a worker for minutes and the queue would never turn over. The files the batch
  had not reached when the budget ran out are ABANDONED — nothing records which
  paths were skipped, so they are never retried from here. They become
  searchable again only when something unrelated covers them: the next write to
  the same file, a full reindex, or trusty-search's own file watcher where one
  is running for that index. The stop is counted as a truncation (see below),
  not only logged.
- `claude_config::write_json_atomic` no longer stages through the fixed
  `<path>.tmp`. Two concurrent writers — `tm launch` and the daemon are the
  real pair, and no in-process mutex can cover them — both truncated and
  filled that one file, so whichever renamed first published whatever bytes
  happened to be in it: the other writer's payload, or half of it. A reader
  watching the target during an 8-writer storm observed 128 spliced snapshots
  before the fix and none after. Staging is now `<path>.tmp.<pid>.<seq>`, a
  name no other live writer can hold (#4077).
- The backup carried the same defect: two `fs::copy` calls into one
  `<path>.bak` interleaved into a torn backup, corrupting the recovery artifact
  at the moment it is needed (1188 spliced snapshots observed pre-fix). It is
  now staged and published by rename like the target.
- A write or rename that fails removes its own staging file, so a failed call
  leaves the target byte-for-byte as it was and drops no litter.
- A failed staging write now reports `fill staging file <path>` instead of
  `publish <path> onto <dest>`, which named a rename that was never attempted.
- Drawer listings no longer let `limit` cut on drawer-UUID order, which hid the newest drawers from `memory_list` ([#4836](https://github.com/bobmatnyc/trusty-tools/issues/4836))
  - `PalaceHandle::list_drawers` and `list_drawers_in_wing` ranked on `importance` alone before truncating. Rust's sort is stable, so drawers tied on importance kept the drawer table's own order — UUID ascending, as the store iterates it — and `limit` cut on that. Importance is effectively bimodal in a live palace (1.0 for curated facts, 0.5 for everything else), so the tie-break decided almost every listing.
  - Measured on the `trusty-tools` palace: `memory_list(tag = "pre-authorized", limit = 12)` matched 94 drawers and returned the 12 with the smallest UUIDs. All nine drawers written that day fell outside the window, including the one the caller was looking for, at index 62 of 94.
  - All four sites now share one comparator, `drawer_listing_order`: `importance` descending, then `created_at` descending, then `id` ascending. Importance stays the primary key, so curated essentials still lead; recency only breaks ties, and the trailing `id` makes the order total so equal drawers cannot swap between calls.
  - The same fix reaches L1 selection, which is what a prompt actually sees. `PalaceHandle::refresh_l1` and `L1Cache::save_l1_cache` carried the same importance-only sort, and there the sort is a selection, not a display order: both truncate to 15, `open_with_intent` hydrates `l1_drawers` verbatim from the persisted snapshot, and `retrieve_l0_l1` seeds `retrieve_l2` from it. A palace with more than 15 drawers at one importance therefore filled L1 without reference to age, so recent memories could not reach a prompt even when recall ranked them well.
  - `dedup_gate` in `trusty-memory` is fixed by the same change. It asked `list_drawers` for "recent drawers" to compare against a 10-minute window, and was handed an arbitrary UUID-ordered slice that mostly predated the window, so near-duplicate detection degraded on any palace larger than its scan limit.
- `OpenIntent::ReadOnlyClient` no longer renames a palace's redb store aside and substitutes an empty database. The #702 incompatible-format recovery — `backup_incompatible_file` then a fresh `Database::create` at the original path — used to run on the first `Database::create` error regardless of intent, so a caller that declared read-only intent could relocate an operator's live store and get back an empty palace. It is now gated on `OpenIntent::Writer`; a read-only-intent open returns an error naming the path, the redb error, and the writer-intent open that can perform the recovery, and leaves the file byte-identical. Writer behaviour is unchanged (#4911).
- The `KgStoreRedb::open_with_intent` and `open_or_get_cached_db` retry loops no longer spend their backoff window on that refusal. Both retried on any error, because every failure they were written for was a transient lock or TOCTOU race; an incompatible on-disk format never resolves by waiting, so retrying it only delayed the failure by 162 ms and logged the same refusal five times (#4911).
- `PalaceRegistry::open` no longer drops a palace it cannot hydrate. It still logs a warning and keeps going, so one bad palace does not fail the whole registry open, but the skip is now recorded and readable via the new `PalaceRegistry::unopenable` / `unopenable_reason`. Previously the palace was simply absent from the registry afterwards — indistinguishable from one that never existed, which turned the refusal above into silent invisibility for the palace it was protecting (#4911).
- New `PalaceRegistry::record_unopenable`, so a host that runs its own hydration walk can file a skip into the same record. `PalaceRegistry::open` is not the path any shipped binary takes — the trusty-memory daemon walks the registry root itself — so without this the record would have stayed empty in production no matter what read it. `register_arc` clears the entry when the palace later opens, so a record cannot outlive its condition (#4911).
- New `PalaceStore::metadata_present`, the single entry point for "does this palace exist on disk". It answers with `try_exists`, so a stat the caller is DENIED reports an error rather than the `false` that `exists()` returned — the #5549 / ADR-0045 defect, in the direction that loses data, since an open-or-create caller reads `false` as permission to overwrite (#4911).
- Memory secret scanner: a `/`-bearing base64 blob is no longer exempted as a slash path. Every `/`-separated run of standard base64 is pure alphanumeric, so the charset-only segment test called it a path and let credentials — including a canonical AWS secret access key — store unredacted (#4977).
- Memory secret scanner: a bare, userinfo-free URL such as a GitHub issue or PR link no longer fails a write. Its path digits used to satisfy the base64 branch's entropy floor; the URL is now decomposed and decided segment by segment, so a URL whose path IS the secret stays blocked (#5513).
- `catchup::session_finder::parse_filename_timestamp` no longer panics on a
  session filename stem containing a multi-byte UTF-8 character
  ([#5294](https://github.com/bobmatnyc/trusty-tools/issues/5294)). Its
  length guards checked byte length, not char count, so a stem like
  `"123é456-142030"` could pass the `len() == 15` / `len() == 8` checks while
  a fixed-byte-offset slice still landed mid-character and panicked. The
  function now rejects any stem whose date/time parts are not all ASCII
  digits before slicing.
- A KG retraction that committed to redb no longer reads back as one that never
  happened ([#5424](https://github.com/bobmatnyc/trusty-tools/issues/5424)).
  `retract_triple`, `cascade_delete_by_drawer`, `retract` and `assert` commit
  before updating the in-memory adjacency; when that second step failed on a
  poisoned lock they returned a bare `kg adjacency lock poisoned` error, and a
  retry then read `Ok(0)` from storage — indistinguishable from "that fact was
  never here", with the opposite remediation. They now return the new
  `memory_core::store::kg::AdjacencyDesync` error: `CommittedButStale` names
  the operation and the rows storage already made durable, and every later
  mutation on the handle stops with `HandleStale` instead of answering a
  plausible `0`. `KnowledgeGraph::adjacency_desynced()` reports the state, which
  is sticky for the life of the handle — redb stays authoritative and the
  adjacency is rebuilt by reopening the palace.
- `PalaceStore::list_palaces` no longer reports a registry it cannot stat as an empty one ([#5532](https://github.com/bobmatnyc/trusty-tools/issues/5532))
  - The absence guard used `Path::exists`, which is `fs::metadata(..).is_ok()` and coerces every stat failure — `EACCES` from an unsearchable parent directory, `EIO`, `ELOOP` — to `false`. The function then returned `Ok(vec![])` without reaching `read_dir`, so the error propagation added in #5488 and #5526 never ran and the destructive callers (`purge_palaces`, `rebuild_palaces`, `merge_palaces`) reported a clean zero-palace run over data they never read.
  - It now uses `try_exists`, which keeps "absent" (`Ok(false)`) distinct from "cannot determine" (`Err`). A data root that does not exist yet — including one whose parents are missing, and a broken symlink — still returns an empty list, so first run is unchanged.
  - A macOS TCC denial is NOT this trigger: measured against real TCC-protected directories, TCC permits `stat` and denies only enumeration, so `read_dir` was always reached and the #5488/#5526 fixes fire as intended.
- `PalaceStore::list_palaces` no longer returns a short list and reports success when it cannot classify an entry. A denied stat on a registry child, a `palace.json` it cannot stat, an undecodable `palace.json`, and a directory entry that fails mid-enumeration now propagate an error naming the offending path instead of dropping that palace from the listing. Genuine absence — a subdirectory holding no `palace.json`, a dangling symlink, an entry unlinked mid-walk — still skips silently. The destructive callers (`purge_palaces`, `rebuild_palaces`, `merge_palaces`) already propagate, so they no longer report a clean pass over a palace they never read (#5543).
- `PalaceStore::load_palace` no longer reports a palace it cannot stat as one that is not there. Its absence guard was `Path::exists`, which is `fs::metadata(..).is_ok()` and so collapses a permission denial into `NotFound` — the one error `list_palaces` treats as benign and skips, so a palace whose permissions changed mid-walk dropped out of the listing the destructive passes act on. A denied or otherwise undeterminable probe now propagates an `Io` error naming `palace.json`; genuine absence still returns `NotFound` and still skips silently (#5549, ADR-0045).
- `PalaceStore::load_identity` keeps its `exists()` guard and now says why: an unreadable `identity.txt` reads as absent, the consumer falls back to a default prompt, and nothing destructive or enumerating branches on the answer. Marked `// intentional fail-open` per ADR-0045 decision 4 (#5549).
- `PalaceRegistry::resolve_palace_alias` probed for `palace.json` with `Path::exists()`, so an alias target it was denied to stat read as a target that is not there. The redirect was dropped and `load_palace` then ran against the alias id's own genuinely-absent directory, returning `NotFound` for the wrong palace — which `open_error_is_absent` classifies as absence, so an aliased palace that exists and merely could not be verified still reached the HTTP callers as 404. The probe is now `try_exists`, with only `Ok(false)` counting as absent; an undeterminable target keeps its redirect and `load_palace` classifies the denial one call later (#5592, ADR-0045).
- `bedrock_region_resolution` no longer fails when `AWS_REGION` is set in the
  ambient environment
  ([#5652](https://github.com/bobmatnyc/trusty-tools/issues/5652)). The test
  asserted that an empty explicit region reaches `us-east-1`, which skips the
  `TRUSTY_AWS_REGION` and `AWS_REGION` tiers that `resolve_bedrock_region` is
  documented to consult first. The precedence walk moved into a pure
  `resolve_region_from(explicit, trusty_env, aws_env)` helper that the test
  drives directly, so every tier is covered without reading or mutating
  process-wide env vars. `resolve_bedrock_region`'s behaviour is unchanged.

### Removed

- The `memory-core-kuzu` feature and the `memory_core::store::kuzu` module it
  gated ([#5695](https://github.com/bobmatnyc/trusty-tools/issues/5695)). No
  workspace member enabled the feature, so it was reachable only through
  `--all-features` — the path `cargo-semver-checks` takes — where it forced a
  cmake source build of `kuzu` and stopped the SemVer gate from running at all.
  Nothing was lost with it: the feature-gated body was a single `warn!()` behind
  a `TODO(kuzu)`, `query()` and `recall()` returned an empty vec whether or not
  the feature was on, and no `use kuzu::` existed anywhere in the tree.
  `KuzuSource`, `KuzuDatabase`, and the unconditional `discover()` /
  `default_roots()` scanners go with the module; they had no caller outside the
  module's own tests and were never re-exported from `store`.
- The `kuzu` workspace dependency, now that nothing declares it.

### Documentation

- Corrected `#4868` issue citations in `launchd_labels`, `launchd`,
  `launchd_activate`, and this crate's module index to `#4919` — the actual
  origin of the launchd-label registry work
  ([#5449](https://github.com/bobmatnyc/trusty-tools/issues/5449)). `#4868` is
  an unrelated trusty-search shutdown-flush-budget fix; three genuine
  backward-references to that real fix (its `ExitTimeOut` plist key) are
  unchanged.

## [0.30.0] — 2026-08-10

### Breaking

- `catchup::session_finder::latest_trusty_mpm_snapshot` no longer resolves across session boundaries ([#5272](https://github.com/bobmatnyc/trusty-tools/issues/5272))
  - it now requires a session id and resolves only snapshots that `sessions-log.jsonl` attributes to it, via the new `session_log::resolve_session_snapshot`. Passing `None` returns `None` instead of the newest snapshot overall
  - `session_log::resolve_latest_snapshot` keeps the session-blind fallback chain but is now used only by the legacy claude-mpm JSON store
  - `catchup::pause::write_pause_snapshot` writes under `sessions/<session-id>/` and records the snapshot path relative to the store root, so one containment-checked join serves both the new and the pre-#5272 flat layout
  - `find_paused_sessions` scans per-session directories in addition to the store root
  - a log entry whose `snapshot` escapes the store (absolute path or `..`) is refused rather than read

### Added

- `credentials::scrub_secrets(text, secrets)` removes known credential values from text this process did not author — a provider's non-2xx response body, a child process's stderr — replacing each occurrence with `[REDACTED]`, longest value first so an overlapping pair cannot leave a tail behind. `credentials::resolved_secret_values()` collects the values to scrub, walking the provider registry through the usual env > `.env.local` > secure-store resolver so a newly registered provider is covered without the caller changing. Values under 8 characters, including the empty string, are skipped: an unset or placeholder credential must not blank the message it appears in. This promotes the scrub that was private to `config keys test`, which now routes through it, so redaction has one implementation. It removes only values the caller already holds — a secret this process never resolved (one a child read from its own config, one quoted inside a provider's error body) still passes through, so scrubbed text is lower-risk, not proven secret-free ([#4321](https://github.com/bobmatnyc/trusty-tools/issues/4321))
- `panic_hook::install_panic_logger` — a process-wide panic hook that emits the
  panic payload, source location, thread name, and a force-captured backtrace
  as one `tracing::error!` before delegating to the previously installed hook.
  macOS `.ips` crash reports carry mangled Rust symbols but not the panic
  message, which left the literal cause of #4764's daemon aborts unrecoverable
  in production (#4764)
- Rooms are a real, persisted, enumerable entity (ADR-0027 T1/T2/T4). Two new
  redb tables in each palace's `kg.db` — `rooms` and `room_keys` — give every
  room a durable id, a first-seen label, and a canonical `(wing, label)` key.
  Ids are now READ from that table rather than recomputed: legacy rooms keep
  the exact `room_to_uuid` value already stamped on their drawers, and new
  rooms mint a UUIDv5, which removes the structured-collision class the old
  16-byte fold admitted for any custom room name of 9+ characters.
  - An additive backfill runs at palace open and registers a row for every
    room the palace's drawers already use. It is insert-only, so it is
    idempotent and a later rename survives every reopen; it is fail-open, so a
    registry problem degrades room listing rather than blocking the palace; and
    it is per-palace on demand, with no sweep across the whole data root.
    **It writes zero `DRAWERS` rows** — existing drawers are named, never
    reclassified or moved.
  - Labels are recovered in four steps: a rainbow table over the nine built-in
    variants, direct inversion of the fold for short labels, a dictionary
    match against the palace's own KG `room:` subjects, and a non-lossy
    `unresolved-<first8>` fallback flagged `resolved: false` for a human to
    rename.
  - `wing_id` is reserved on every room row and defaults to a per-palace
    `DEFAULT_WING_ID`. The Wing entity itself is not implemented (ADR-0027 T9,
    gated on its #3064 consumer).
- Room registry surface backing the new MCP room tools (ADR-0027 T6):
  `create_room` (idempotent — a racing second caller reads the winner's id back
  out of `ROOM_KEYS` rather than minting a second room), `rename_room`,
  `list_room_summaries`, `resolve_room_selector` (a room id or a
  case-insensitive label), and `parse_room_preserving_case`, which classifies a
  caller-typed name through the one `RoomType::parse` while keeping the
  capitalisation a human chose for the stored label.
  - `rename_room` is the ONLY code in the room registry that rewrites a row.
    It touches `ROOMS` / `ROOM_KEYS` only and provably changes zero `DRAWERS`
    bytes; it refuses to take a name already owned by another room, because
    merging two rooms is ADR-0027 D5 and is deliberately deferred.
- Room-scoped recall entry points `recall_in_room` and `recall_deep_in_room`
  (ADR-0027 T7). `retrieve_l2` has enforced a room filter since #3274 but no
  recall entry point exposed it, so a room scope was unreachable from the
  recall path. L0/L1 stay unfiltered — they are the palace's always-on identity
  and essential grounding, not search results.
- `store::room_plan::plan_rooms` — the read-only plan of what a room backfill
  would write (ADR-0027 T10), behind the new `trusty-memory rooms backfill
  --dry-run`. `backfill_rooms` now executes exactly this plan rather than
  re-deriving it, so the audit an operator approves and the write that follows
  cannot disagree. Planning opens read transactions only and is proven not to
  change one byte of `ROOMS`, `ROOM_KEYS`, or `DRAWERS`.
- Wings are a real, persisted, enumerable scope over rooms (ADR-0027 D2, ticket T9, closes [#4809](https://github.com/bobmatnyc/trusty-tools/issues/4809))
  - a Wing is the "who" axis (scope/ownership); a Room stays the "what" axis (topic).
    `engineer`/`Planning` and `pm`/`Planning` are now two distinct rooms, with no
    `Custom("engineer-planning")` name mangling
  - new `WINGS` / `WING_KEYS` redb tables in the palace's `kg.db`, initialised in the
    same transaction as `DRAWERS` and `ROOMS` so the schema is never half-present
  - every palace gets a `default` wing, seeded at open. **Wing is never required of a
    caller**: `wing_id` defaults to the default wing on every write and read, so a
    caller that never mentions a wing behaves exactly as it did before
  - existing rooms land in the default wing by NAMING, never reclassification — every
    `RoomRecord` has carried `DEFAULT_WING_ID` since T1, so the migration writes one
    wing row and changes zero drawer rows and zero room rows (proven byte-for-byte by
    `seeding_the_default_wing_changes_no_room_or_drawer_rows`)
  - seeding is insert-only and probes by id, so it is idempotent and a wing renamed
    between palace opens keeps its new name
  - `RecallScope` (`All` / `Room` / `Wing`) plus `retrieve_l2_scoped`, `recall_scoped`,
    and `list_drawers_in_wing` give wing-scoped recall and listing. A wing scope
    resolves **fail-closed** — an unresolvable scope admits nothing rather than
    everything, because a scope boundary that fails open is a leak (the #3064
    "two agent types cannot accidentally read/write the same room" criterion)
  - `resolve_or_create_room_in_wing` lets a write place a room in a named wing;
    without it a non-default wing could never receive a drawer
  - a wing rename reads the row, checks the new label is free, rewrites the row,
    points the new key at it, and retires the old key — all in ONE redb write
    transaction. No crash can leave the retired label resolving as a stale alias,
    and a concurrent `wing_create` cannot claim the new label between the check
    and the write. A rejected rename writes nothing at all
  - `retrieve_l3` / `recall_deep` gained the same `RecallScope` generalisation as
    L2, so `memory_recall_deep` honours a wing instead of ignoring it
- `retrieval::shared_embedder_initialized()` reports whether the process-wide embedder cell is live, without triggering a cold init ([#4836](https://github.com/bobmatnyc/trusty-tools/issues/4836))
  - lets a caller distinguish "the embedder is genuinely still warming" from "a startup flag was never cleared", instead of degrading on a stale signal
- `Drawer.fact_key` and the `DRAWERS_BY_FACT_KEY` redb index — storage groundwork for ADR-0028 D5's "one slot, one live fact" model, where a Tier C fact claims a namespaced slot (`pr:4818/state`) and writing that slot retires its prior occupant. `KgStoreRedb::drawer_id_for_fact_key()` answers "who holds this slot?" with a point lookup instead of a full drawer scan; the index is maintained inside the same write transaction as the row it describes, on the single-op, batch, and import paths alike, and a delete releases the slot only when the departing drawer still owns it. Purely additive: no existing `DRAWERS` row is rewritten, rows written before the field decode with `fact_key = None` through a new `PreFactKeyDrawerRecord` fallback that preserves the `completed_at` an unlinked chain would have dropped, and nothing writes a non-`None` value until the Tier C write path lands (closes [#4884](https://github.com/bobmatnyc/trusty-tools/issues/4884))
- ADR-0028 Tier C write path — `RememberOptions` now carries `fact_key` and `expires_at`, and `PalaceHandle::remember_with_options` is the single admission point for both. Naming a slot makes a write a Tier C ("current fact") write: writing a slot another drawer already holds atomically retires that incumbent, so `pr:4818/state` can be written fifty times and still occupy one slot. Admission fails closed per D4 — a key that breaks the `<domain>:<id>/<aspect>` grammar, or an `expires_at` that has already elapsed, is refused Tier C and the drawer is written exactly as it would have been before, never admitted-with-a-warning; a slot with no explicit expiry takes a 24-hour default, so an admitted Tier C fact always declares how it ends. The incumbent's retirement and the newcomer's arrival commit in ONE redb transaction (`KnowledgeGraph::upsert_drawers_atomic`), under the existing per-palace write mutex, so a concurrent writer on the same slot can neither leave two drawers claiming it nor retire an incumbent without a replacement landing (closes [#4886](https://github.com/bobmatnyc/trusty-tools/issues/4886))
- `bm25_client::Bm25Client::stats` and `Bm25Stats` — a caller can now ask a BM25
  daemon how much corpus it is serving. Without it an empty search result was
  ambiguous between "the query matched nothing" and "nothing is indexed", so a
  partially-backfilled palace served partial content while looking healthy.
- `sys_metrics::process_rss_mb` — resident memory of an arbitrary pid (macOS
  `phys_footprint`, Linux `/proc/<pid>/status` `VmRSS`). The one entry point
  every trusty-* supervisor uses to enforce a child-memory limit, so #2846's
  declared-but-never-compared `rss_limit_mb` cannot recur per-crate. `None`
  means "cannot measure", never "measured zero".
- **A relevance floor for recall results ([#5037](https://github.com/bobmatnyc/trusty-tools/issues/5037)).** `DEFAULT_RELEVANCE_FLOOR`,
  `FloorOutcome`, and `apply_relevance_floor` in a new
  `memory_core::retrieval::relevance` module. Every retrieval path in `layers.rs`
  ended in `truncate(top_k)` and nothing else, so a query with no good answer
  still returned a full `top_k` of whatever ranked highest — including L1
  drawers scoring `importance * L1_NO_SIMILARITY_PENALTY`, at most `0.15`.
  `apply_relevance_floor` is the one implementation of "below the floor, it is
  not shown", and it returns the count of what it dropped so a caller can say so
  rather than going silent. An item whose score is unknown is kept, never
  dropped.

  The default is `0.35`, picked from measured distributions against the live
  1,332-drawer `trusty-tools` palace rather than guessed: 150 candidates from 15
  off-topic prompts span 0.1500–0.3439 (75 of them at exactly 0.1500, the L1
  penalty), while 57 self-retrieval correct-drawer hits span 0.4844–0.9743 and
  1,200 candidates from real logged hook prompts span 0.4042–0.7527. `0.35` is
  the smallest swept value at which no off-topic candidate survives.

  `recall`/`recall_scoped` are deliberately unchanged: `truncate(top_k)` stays a
  length cap, and gating inside it would change every MCP and CLI recall
  caller's contract. Callers that must not show a weak match apply the floor to
  what comes back.
- `Bm25Client::missing_docs` asks the daemon which of a set of doc ids it does
  not hold — the only call that establishes coverage. `Bm25RpcError` and
  `is_method_not_found` let a caller tell a daemon that predates a method from
  one that is unreachable; both fail closed but need different operator action.
- **`same_origin_cors` and `with_guarded_middleware_same_origin_cors`** — the
  standard daemon middleware stack with the permissive CORS policy swapped for
  one that reflects only same-machine origins: loopback,
  a local webview shell (`origin_is_local_webview`), or the daemon's own
  resolved non-loopback bind ([#5052](https://github.com/bobmatnyc/trusty-tools/issues/5052)).
  `with_guarded_middleware`'s write guard is method-gated, so it never covered
  cross-origin READS; a daemon whose GET surface carries sensitive content opts
  into this variant instead. trusty-agents is the first consumer; the other
  daemons keep `with_guarded_middleware` until each is reviewed on its own.
- `search_index::IndexOptions` and `ensure_project_indexed_with`, so a caller can register a trusty-search index with its vector lane suppressed (`skip_vector: true` — BM25 and KG only, no embeddings) ([#5060](https://github.com/bobmatnyc/trusty-tools/issues/5060))
  - `ensure_project_indexed` is now a one-line wrapper over it; `IndexOptions::default()` reproduces the previous behaviour exactly, so no existing caller changes
  - `POST /indexes` now always carries a `skip_vector` field; `false` is equivalent to omitting it
- `search_index::ensure_project_indexed_reporting` returns the derived index id alongside an `IndexRegistration` saying what the daemon actually did — `Confirmed`, `NotConfirmed`, `DaemonUnreachable`, or `SkippedUnderTest`. The id-only entry points cannot distinguish a confirmed registration from a silent no-op against a down daemon, which let callers log a success they never observed. The fail-open contract is unchanged: the id is still always returned and nothing propagates (refs [#5060](https://github.com/bobmatnyc/trusty-tools/issues/5060))
- `IndexOptions` is now `#[non_exhaustive]`. The daemon already carries a third orthogonal flag this bag does not expose (`skip_kg`), so adding a field later would otherwise break external struct-literal construction in a published crate (refs [#5060](https://github.com/bobmatnyc/trusty-tools/issues/5060))
- `uds::rpc::send_framed_request` — the shared one-request/one-response,
  newline-framed JSON transport over a hardened Unix socket (ADR-0034 §4,
  #5089 step 3). Dials through `connect_hardened`, so the socket's `0700`
  directory and `0600` mode are verified before a byte is written; caps the
  response at `MAX_FRAME_BYTES` and bounds the whole exchange with a
  caller-supplied timeout. Errors are a `UdsRpcError` variant per failure
  point, and none of them may be read as an acknowledgement. The four existing
  hand-rolled clients (`embedder_client/uds.rs`, `bm25_client.rs`, and
  trusty-agents' `ctrl/socket.rs` and `bus`) migrate onto it in a follow-up
- `webhook_hmac` (feature `webhook-hmac`) — the single GitHub
  `X-Hub-Signature-256` verifier. `verify_github_signature` returns a
  three-state `SignatureVerdict` rather than a `bool`, so "no secret is
  configured" cannot be collapsed into "the signature is wrong" and silently
  become permission to proceed — the shape of trusty-analyze's live fail-open
  (ADR-0034 §3). `sign_github_body` is the matching test-harness helper.
  trusty-analyze's and trusty-review's copies are retired in #5089 step 4
- `webhook_relay` (feature `webhook-relay`) — the console→target relay wire
  contract: `RELAY_METHOD`, the borrowed `RelayFrame` the sender writes, the
  owned `RelayRequest` a receiver reads, `Provenance`, and `RelayResponse`.
  It lives here rather than in trusty-console because step 4's receivers are
  trusty-review and trusty-analyze, which cannot depend on the console — a
  contract held by one half only is two copies waiting to drift.
  `RelayResult::ack` is `#[serde(default)]` and `RelayResponse::is_ack` is the
  only predicate a deletion path may consult, so a receiver that answers with
  an empty result object has not acknowledged
- `uds::supervisor::UdsServiceSupervisor`, behind the new `uds-supervisor`
  feature: the on-demand spawn supervisor generalised out of `trusty-memory`'s
  `Bm25Supervisor`, so `trusty-console` can start `trusty-review` /
  `trusty-analyze` at webhook-delivery time without either being resident
  (ADR-0034 §1, milestone `tm 1.3.5` criterion (c)). It carries the whole state
  machine that crate had already hardened: spawn-gate serialisation against
  double-spawn and against a fan-out that satisfies the cap per-caller,
  adoption of a socket another process bound, an LRU cap on live children, an
  RSS ceiling compared against a real measurement, exponential-backoff socket
  probing, `kill_on_drop`, and SIGTERM→SIGKILL with a patience window (#5089)
- Liveness is decided by the socket, never by `try_wait()`. `SocketVerdict` is
  three-state and only ENOENT / ECONNREFUSED mean `NotServing`; a timeout or any
  other errno is `Inconclusive` and leaves the child alone, so load cannot turn
  into a respawn storm. A child evicted while still alive goes onto a `doomed`
  queue drained under the spawn gate — SIGTERM plus socket unlink, in that order
  — rather than being dropped onto `kill_on_drop`'s SIGKILL, because the doomed
  child unlinks the shared socket path as the last step of its own shutdown and
  a concurrent termination could land that unlink after the replacement bound
  the same path (#5085, #5119, #5089)
- `ServiceTimeouts` makes the spawn-probe budget, the child's own shutdown-flush
  budget and the SIGTERM patience per-service values rather than constants. Its
  constructor is a `const fn` whose assert enforces `sigterm_patience >
  shutdown_flush`, so a service that declares its timeouts as a `const` gets the
  same compile-time failure `bm25_supervisor.rs`'s `const _: () = assert!(…)`
  gave — now bound to the value actually passed to the supervisor rather than to
  a free-standing constant (#5089)
- Socket adoption is verified, not assumed: a socket that answers but fails
  `verify_socket_for_connect` is refused with `SupervisorError::UntrustedSocket`
  instead of being adopted silently or falling through to a spawn that dies on
  EADDRINUSE. This is the one path where the target's own `bind_hardened` never
  runs, which ADR-0034 §3 makes the permission bits load-bearing for (#5099,
  #5089)
- Every new public enum and struct is `#[non_exhaustive]` while the crate is
  unpublished at 0.30.0 against a released 0.28.1 (#5089)
- `ServiceTimeouts::try_new` is the fallible sibling of the `const fn`
  constructor, for a service deriving its timeouts from config rather than
  declaring them as a `const`. `#[non_exhaustive]` left no other way to build
  the value, so without it the runtime case could only panic. The type also now
  documents the sourcing rule the assert cannot check: `shutdown_flush` must be
  the supervised binary's own flush constant, imported, not a literal that
  happens to match it today (#5089)
- **New `uds` module: the `0600` socket permission ADR-0031 and ADR-0032 both
  cite as an existing property** ([#5099](https://github.com/bobmatnyc/trusty-tools/issues/5099)).
  No production code in the workspace set permissions on any socket — every
  `set_permissions` / `PermissionsExt` hit was a test fixture — so sockets were
  created at the process umask (commonly `0755`). `uds::bind_hardened` is the
  single entry point every bind site now routes through: it creates the
  containing directory at `0700` via `DirBuilder::mode()` (passed to `mkdir(2)`,
  so the directory is never observable at a wider mode) and narrows the socket
  to `0600` before the caller's first `accept`. `uds::ensure_peer_is_self`
  refuses any connection whose uid is not this process's own, via `SO_PEERCRED`
  on Linux and `getpeereid` on macOS/BSD, which is what makes the permission
  bits an enforced boundary rather than a documented intention. Gated behind a
  new `uds` feature, implied by `bm25-client` and `embedder-client`; adds no new
  dependency (`libc` moves from a macOS-only to a `cfg(unix)` target dependency).
- **`uds::connect_hardened` — the dialer's half of the same contract.** Hardening
  only the bind side left every client trusting whatever sat at the path: a
  daemon predating the change still answers, and `Bm25Supervisor` adopts an
  existing socket rather than spawning, so the daemon's own ownership check may
  never run. `connect_hardened` refuses unless the containing directory is a
  non-symlink `0700` directory owned by this uid and the socket is a non-symlink
  socket owned by this uid at `0600`. `bm25_client` and `UdsEmbedderClient` now
  dial through it.
- **`uds::check_sun_path_budget`** turns the kernel's bare `invalid argument`
  into a diagnostic naming the platform's `sun_path` capacity (104 on macOS, 108
  on Linux), the actual byte length, and the offending path.
- **`UdsSecurityError` is `#[non_exhaustive]`.** It gained two variants across
  two review rounds of this PR alone and will keep growing as the checks
  tighten. The attribute is free while this crate is unpublished at 0.30.0
  against a published 0.28.1, and stops being free once 0.30.0 ships — after
  which each variant would be a break. On an enum it constrains matching only
  (external crates need a wildcard arm), not construction; every in-tree
  consumer converts the error rather than matching it, so nothing changed.
  Matches `DedupError` (#5112) and `IndexOptions` (#5065).
- `tmux::scrollback_option_commands` and `tmux::managed_session_commands` now emit an `alternate-screen` entry alongside `history-limit` and `mouse`, and both take an `alternate_screen: bool` parameter ([#5151](https://github.com/bobmatnyc/trusty-tools/issues/5151)). `DEFAULT_TMUX_ALTERNATE_SCREEN` is `true` — tmux's own factory value — so nothing changes until a consumer passes `false`.
- Two `TmuxCommand` variants for the window option scope: `SetWindowGlobalOption` (`set-option -wg`) and `ShowWindowGlobalOption` (`show-options -wg -v`). `alternate-screen` is a pane option inheriting from the window scope, not a server option like `history-limit`. Measured against tmux 3.6b, `set-option -pg alternate-screen off` and `set-option -s alternate-screen off` both exit 0, a same-flag readback reports `off`, and the pane still enters the alternate screen; only the window scope takes effect. Pairing the two variants keeps a caller's set and its verification probe on the same scope.
- The receive half of the console→target webhook relay: `webhook_relay::Inbox` (a durable, fsync-before-return delivery store that reads the held copy back before treating a repeat as already-owned), `webhook_relay::serve` (the `webhook.deliver` listener, which acknowledges only after the inbox has taken ownership, refuses any other method by name, and distinguishes a supervisor liveness probe from a dropped delivery), and `webhook_relay::WebhookListener` (bind, serve, exit on SIGTERM, unlink before closing). Socket and inbox paths for both targets resolve from `webhook_relay::{review_socket_path, analyze_socket_path, socket_path_for, inbox_root_for}` instead of a literal in each crate, and `webhook_relay::held_count` lets `trusty-console` meter an undrained backlog without touching another service's directory. `uds::bind_singleton_hardened` takes over a socket file only on a proved `SocketVerdict::NotServing`, and that classification moved from `uds::supervisor::probe` to `uds::probe` so both callers share it (re-exported, so no existing path changes). The `webhook-relay` feature now implies `uds`.
- `uds::send_framed_request` now reports a target that hangs up without answering as `UdsRpcError::NoResponse` whichever way the platform delivers it. Closing a Unix socket that still holds unread bytes makes Linux reset the peer where macOS sends a clean EOF, so the same refusal — `webhook_relay::serve` dropping an over-long frame after reading its first bytes — used to arrive as `NoResponse` on macOS and `Read`/`ECONNRESET` on Linux. A reset that arrives after part of a frame has landed is still `Read`, since bytes did come back. Both variants were, and remain, errors that no caller may read as an acknowledgement.
- `webhook_relay::drain_once` reads a receiver's inbox back out and drives a `DeliveryProcessor`, removing an entry only after the pipeline accepted it. Exclusive per-entry claims use an `flock` the kernel releases on process death, so a drainer killed mid-processing leaves the delivery claimable rather than stranded.
- A delivery is recorded in a `<inbox>/processed/` ledger before its entry is removed, and the ledger is consulted before any processor runs — so a drainer that dies between accepting a delivery and unlinking it cannot cause the work to be repeated. The `delivery_id` deduplication the relay contract requires now lives in the drain rather than in each receiver. Markers are pruned after 30 days.
- Processing failures are counted in a durable sidecar, bounded, and quarantined under `<inbox>/quarantine/` rather than deleted; `quarantined_count` exposes that to `trusty-console`.
- `WebhookListener::with_processor` and `run_until_signal_with_processor` run the drain at startup, on every accepted delivery, and on a 30-second backstop (#5192).
- `workspace_layout` — the one resolver for trusty-mpm's managed workspace root and session-worktree base name, so the four crates that hardcoded `~/trusty-mpm-projects` and `.worktrees` independently now read the same configured values (#5203, #5204). A configured worktree base is rejected back to `.worktrees` if it is not a single path component or collides with a reserved name (`worktrees`, `.git`, `.claude`, `.base`), which keeps Claude Code's `.claude/worktrees/` agent store outside trusty-mpm's ownership predicate.
- `docgen` feature (off by default, test-facing): marker-delimited generated
  documentation regions. Renders MCP tool tables and counts from a crate's real
  descriptor function into `<!-- BEGIN GENERATED: <id> -->` regions in markdown,
  then asserts the checked-in copy matches — or rewrites it under
  `UPDATE_DOCS=1`. Rows sort by tool name so no map or source ordering reaches a
  committed file, and the `descriptor_source!` macro makes the cited symbol a
  compile-time reference rather than a hand-typed string. Adds no dependency
  (#5205)

### Fixed

- Updated doc comments and Cargo.toml that still named `open-mpm` as a
  consumer crate to say `trusty-agents` (renamed in #831), and corrected the
  `symgraph::SymbolRegistry` on-disk path from the stale, hardcoded
  `.open-mpm/state/symbol-registry.json` to `.trusty-agents/state/…`,
  matching the rest of trusty-agents's config-dir convention. The registry is
  a regenerable content-addressed cache, so existing installs simply rebuild
  it under the new path. Genuine back-compat (`OPEN_MPM_*` env-var fallbacks
  and the `.open-mpm` legacy-dir migration) is unchanged. `KuzuSource`'s
  `~/.open-mpm/memory` root is left alone too, but it is not back-compat: it
  has zero callers, its feature is enabled by nobody, and the real migrator
  (`trusty-memory`'s `kuzu_migrate`) takes a mandatory `--from <store.redb>`
  instead of discovering that path.
- `search_index`'s two mutating entry points (`ensure_project_indexed`, `index_files_best_effort`) no longer reach a live trusty-search daemon from a `cargo test` process, so a `tempfile` fixture root can no longer be registered in the operator's `indexes.toml` (closes [#4255](https://github.com/bobmatnyc/trusty-tools/issues/4255))
  - new `test_harness::running_under_test_harness()` detects a cargo test binary at runtime, which — unlike `cfg(test)` — also covers `tests/` and `[[bin]]` targets and the cross-process case where the write lands in a different process
  - `TRUSTY_ALLOW_PRODUCTION_STATE=1` is the explicit opt-in for a test that deliberately drives a real daemon; `TRUSTY_TEST_HARNESS=1` forces detection on for a child process a test spawns
  - reads are unaffected — only the writes are gated
- the memory secret detector no longer rejects ordinary Markdown-shaped prose — file:line citations, cargo feature lists, hyphenated English, decorated paths, issue/PR lists, and most bare URLs (refs [#4312](https://github.com/bobmatnyc/trusty-tools/issues/4312))
  - root cause: the base64 branch of `looks_like_secret` fired on any token of 20 chars or more containing a `/` plus one letter, whenever a segment fell outside `is_word_segment`'s deliberately narrow charset. Every Markdown decoration — backtick, `**`, `"`, `'`, `[`, `(`, `|`, `%`, `,` — put prose on the credential path. That is the single cause the four previous per-shape exemptions ([#1667](https://github.com/bobmatnyc/trusty-tools/issues/1667), [#2800](https://github.com/bobmatnyc/trusty-tools/issues/2800), [#4216](https://github.com/bobmatnyc/trusty-tools/issues/4216), #4312) each patched one symptom of
  - the branch is now gated on the charset a real blob is made of, plus an entropy floor: base64 encodes bytes, so an encoded run of that length carries an uppercase letter or a digit, while an all-lowercase run is English. Credential-bearing URLs (`scheme://user:pass@host`) are exempted from the floor by shape rather than entropy, so `postgres://user:password@host/database` is still caught while a bare documentation link is not
  - `find_secret_token` additionally treats the backtick as a token delimiter, so adjacent inline-code spans joined by a bare `/` are no longer read as one token
  - measured against `origin/main` over 18 prose shapes, 14 URL-shaped prose shapes, and 30 credential shapes: prose false positives 17 to 0 and 14 to 4. Detection is tightened in one place — a credential written flush against a backtick was previously missed
  - known limitations, all documented and pinned by tests. A backtick inside a connection-string password splits the token and that credential is missed (machine-generated credentials cannot contain a backtick; user-chosen passwords can). Four URL-shaped prose tokens carrying a digit or capital, including a bare GitHub issue link, are still flagged — exempting bare URLs outright would lose a `…/services/<id>/<id>/<token>` webhook secret, so that shape needs its own change. And requiring `user:pass@` for the URL exemption gives up one class this branch previously caught: a userinfo-free URL whose path secret is entirely lowercase and digit-free now reads as English to the entropy floor. Real webhook tokens are near-universally mixed-case or digit-bearing and stay caught
- Generated LaunchAgent plists now declare `ExitTimeOut`. Without the key
  launchd applies its "system-defined" default, which measures 5 s on macOS —
  SIGTERM, then SIGKILL 5 s later on every `launchctl bootout`, `kickstart -k`,
  logout and reboot. That is shorter than the shutdown work several trusty-*
  daemons do; trusty-search's index flush alone floors at 30 s per index, so it
  was cut off mid-write every time. The rendered window comes from the new
  `shutdown::TERMINATION_GRACE_SECS` (60 s), which `trusty-search stop` and its
  orphan reaper now wait as well, so the window a daemon plans for and the
  window its terminator grants cannot drift apart. **Already-installed agents
  keep launchd's 5 s default until their plist is regenerated** (#4393)
- memory TUI no longer hides palaces whose counts the daemon never measured — `palace_has_content()` now reads the `Option<u64>` accessors instead of the raw zeroed fields, so *unknown* counts keep a palace visible (rendered as `—`) while a measured-empty palace stays filtered out (closes [#4690](https://github.com/bobmatnyc/trusty-tools/issues/4690))
- Secret detector no longer flags dotted or underscored filenames that carry a capital letter (closes [#4739](https://github.com/bobmatnyc/trusty-tools/issues/4739))
  - `Agents.app.bak-20260729-000028` and shapes like it reached the mixed-case branch, which #4723's base64-branch narrowing never covered
  - `is_structural_token`'s segmented-identifier branch now splits on `.` and `_` as well as `-`, and accepts a `Capitalized` segment alongside `lowercase` and `UPPERCASE`
  - Measured over a 36-shape prose battery and a 30-shape credential battery: prose false positives 17 → 2, credential misses 0 → 0
  - CamelCase segments (`TrustyMemory.app.bak-…`) are a stated known bound, not fixed: admitting them would lose delimiter-segmented mixed-case credentials
- `sys_metrics::dir_size_bytes` can no longer abort the calling process. The
  walk was recursive, so descending N levels held N `ReadDir` handles open at
  once; when `std`'s `impl Drop for DirStream` hit a failing `closedir(3)` and
  panicked, the unwind ran the enclosing handles' destructors, a second
  `closedir` failed the same way, and a panic raised during unwinding is a
  non-unwinding panic Rust aborts on unconditionally. The walk is now
  iterative and holds at most one directory handle at a time — removing the
  second destructor from the unwind path — and is additionally wrapped in
  `catch_unwind`, which returns the partial byte total instead of propagating.
  This took down the `trusty-search` daemon 40 times in a week, roughly every
  7 minutes under load (#4764)
- The size walk is now bounded: it refuses to descend past 64 levels and
  abandons a walk that exceeds a 30 s wall-clock budget, reporting the partial
  total in both cases. A best-effort disk figure should never become an
  unbounded sweep of an actively-mutating tree (#4764)
- `install_and_activate` gained a forced variant. A deploy replaces the binary
  behind a byte-identical plist, so the unchanged-unit skip that removes the
  reinstall outage would have let `make deploy` finish without ever activating
  what it built — launchd kept running the old image (#4868)
- Rollback no longer reports success it did not achieve, and no longer takes
  down a daemon it should have left alone. Liveness is captured before the plist
  is overwritten, because launchd keeps a job registered after its plist file is
  deleted — so "no previous plist" never meant "nothing was running". The
  restoring bootstrap's result is now checked, and a failed restore says the
  service is down instead of claiming it was preserved (#4868)
- `launchd_labels` is now the one definition of every trusty-* LaunchAgent's
  label, and each daemon crate, the installer, and `tctl` read it instead of
  restating their own literal. They had drifted: `trusty-search service install`
  wrote and bootstrapped `com.trusty.trusty-search` while the unit launchd
  actually had loaded was `com.trusty.search`, so the install evicted nothing,
  started a second daemon contending for :7878 and the index locks (#2938), and
  left #4393's `ExitTimeOut` in a plist launchd never reads. `trusty-console`
  had the same divergence (`com.trusty.trusty-console` in code,
  `com.trusty.console` loaded). The canonical form is `com.trusty.<member with
  its `trusty-` prefix stripped>`, which every loaded unit on a real host obeys;
  `canonical_label` is that rule as code and the registry table is checked
  against it. Correcting one literal is what was done for #2827, and the defect
  came back — so the second copy is gone rather than corrected (#4868)
- `LaunchdConfig::install_and_activate` replaces the bare `install()` +
  `bootstrap()` pair for service installs. It boots out the service's recorded
  legacy labels and deletes their plists first, so an upgrade cannot leave the
  old unit running beside the new one; skips the reload entirely when the
  rendered plist matches what is installed and the label is already loaded,
  which is where the ~1 minute of release downtime came from; verifies the label
  actually came up rather than trusting `launchctl bootstrap`'s exit code
  (#2498); and restores plus re-bootstraps the previous plist if activation
  fails, so a failed install no longer leaves the service down (#4868)
- A workspace-scanning test now fails on any `com.trusty.*` / `com.bobmatnyc.*`
  label literal in production source that the registry does not own. Codesign
  identifiers (`macos_signing`) are exempt — they are a different namespace and
  renaming one invalidates a binary's designated requirement (#2558) (#4868)
- The launchd-label drift guard no longer skips production code. Four holes, each
  proven by planting a literal that passed: `#[cfg(not(test))]` and
  `#[cfg(any(…, test))]` gate PRODUCTION code but were treated as test items, so
  the scan skipped exactly what it should read (eight such sites exist);
  `strip_comment` blanked any line starting with `*`, so a deref assignment
  `*target = "…"` was read as a comment while the identical `let` form was not;
  `#[cfg(test)] use std::fmt;` puts attribute and item on one line, so consuming
  the NEXT line ate production code; and the codesign exemption was whole-line,
  so a launchd label sharing a line with a `*_IDENTIFIER` assignment was skipped
  along with it. Polarity is now checked rather than token presence, block-comment
  state is tracked across lines, a self-contained attribute line consumes nothing
  further, and only the identifier token adjacent to the marker is exempt. A
  build file may name a legacy label on a `bootout`/`unload` line — evicting an
  old label is the migration, not the drift (#4868)
- Rollback no longer reports success while the service is down. `bootstrap`
  boots out first, so on the "no previous plist but the label was loaded" path
  the running job is already gone by the time rollback executes — deleting the
  plist and returning "nothing was taken down" produced service down, plist
  gone, and a message denying both. That path now keeps the plist just written
  and bootstraps it, because the displaced job had no plist on disk and cannot be
  reconstructed; a failed revival reports the outage instead of swallowing it.
  The effects are injected so the outcome is asserted rather than the plan value,
  which is why the previous tests passed while the goal was unmet (#4868)
- `LaunchdConfig` renders a `WorkingDirectory` when one is set, so a regenerated
  unit can preserve the installed one's (#4868)
- `evict_legacy` is public, so `service uninstall` can remove a unit registered
  under an old label rather than reporting "nothing to do" (#4868)
- recall no longer serves a drawer past its `expires_at`. Expiry used to be consulted only when a palace was opened, and the daemon opens once as `OpenIntent::Writer` and holds that handle for its whole life — so a drawer that expired mid-session kept being injected until the daemon restarted. `retrieve_l0_l1`, `retrieve_l2_scoped`, and `retrieve_l3_scoped` now drop expired drawers on every read (ADR-0028 D4), which also closes a second gap: `l1_drawers` is filled from the L1 cache snapshot, which the open-time sweep never pruned, so an expired L1 drawer survived even a reopen. The read paths filter without deleting — reclamation stays with `purge_expired` and the open-time sweep, so a recall can never fail because a cleanup failed. All three sites plus the sweep now share one predicate, `Drawer::is_expired_at`, instead of hand-copying the comparison (closes [#4885](https://github.com/bobmatnyc/trusty-tools/issues/4885))
- memory secret-scanner no longer rejects ordinary prose, branch names, or short tokens as credentials (closes [#4898](https://github.com/bobmatnyc/trusty-tools/issues/4898))
  - a `+`-joined English phrase (`PM+instructions+subagents`) is recognised as prose instead of base64; `+` no longer disqualifies a token outright, and each segment must be character-class-uniform so encoder output like `j1u7nJd+tvZers+wdZyr` stays flagged
  - a delimiter segment may carry one capital anywhere, not only in first position, so a branch name like `fix-3696-slice1-gapA-emit` is a human identifier again
  - the 20-character length floor now runs before the credential-prefix test, so a 4-character token (`Asia`) can no longer match `AKIA`/`ASIA`; AWS key ids are matched by the all-uppercase shape of their first 20 bytes, which keeps `AKIAIOSFODNN7EXAMPLE-old` and a key-id/secret-key pair flagged while letting `ASIA-PACIFIC-ROLLOUT-NOTES` through
  - added a deterministic generated-encoder corpus as a ratchet on base64 and base64url miss rates
- memory recall ranks by relevance again: L2/L3 scored a candidate `eff_importance * similarity`, but inside one candidate pool `eff_importance` spans ~0.44 where similarity spans ~0.07, so the product ranked by importance and discarded the search. Importance now tilts the score by at most 5% (`IMPORTANCE_TILT`), which is the tiebreaker role its own docs described. Measured on a 1,272-drawer palace with a 400-drawer self-retrieval probe: recall@5 295/400 → 351/400, recall@1 102/400 → 291/400 (closes [#4904](https://github.com/bobmatnyc/trusty-tools/issues/4904))
  - L2 and L3 now share one `rank_score` instead of repeating the expression, so deep recall cannot be left on an older formula
  - every L2 candidate is traced with its score components before the `top_k` truncation, on the `memory_recall_rank` target — the pre-truncation view that tells "never a candidate" apart from "ranked below the cutoff"
- deferred embedding no longer drops failures silently — a drawer can no longer be stored, durable, and permanently unfindable (closes [#4906](https://github.com/bobmatnyc/trusty-tools/issues/4906))
  - the background embed lane retries transient failures with bounded exponential backoff instead of giving up on the first error
  - a final failure writes a durable row to the palace's `embed_failures.json` ledger, so the loss outlives the `warn!` that used to be its only trace — serialised through `json_rmw`, so a burst of concurrent failures keeps every row instead of only the last writer's
  - "no embedder on this host" is separated from "the embedder is here and this drawer failed"; only the second marks a drawer, so a machine with no model downloaded is not reported as having thousands of broken drawers
  - new `PalaceHandle::embed_health()` answers "which drawers have no vector" by set-differencing the drawer table against the vector index, replacing a self-retrieval guess
  - new `PalaceHandle::backfill_missing_vectors()` re-embeds drawers that already lack a vector — idempotent, safe to re-run, and a no-op on a healthy palace (it does not even resolve an embedder when there is nothing to repair)
- one failing embedder test no longer cascades into seven. `ENV_LOCK.lock().unwrap()` poisoned the shared `Mutex<()>` when the ONNX accuracy gate panicked, so the six `resolve_*` tests — pure model/provider/cache-dir resolution that never touches fastembed — failed with `PoisonError` and buried the real cause. Call sites now go through `test_env::env_lock()`, which recovers via `PoisonError::into_inner`; the lock guards no data, and `EnvVarGuard::drop` restores the environment during the panicking test's unwind (closes [#4940](https://github.com/bobmatnyc/trusty-tools/issues/4940))
- `HnswStore` no longer aliases vector ids across two live stores over one palace file, which silently overwrote one drawer's embedding with another's (closes [#5005](https://github.com/bobmatnyc/trusty-tools/issues/5005))
  - the vector-id counter now lives in redb (`vector_id_seq`) and is reserved inside the same write transaction as the insert, so every writer on the file serialises against it; an existing palace has its counter seeded to the file's high-water mark on open, and re-raised on every subsequent open so a rolling upgrade cannot leave it behind
  - `upsert` refuses an id that already has a `VECTORS` row: it allocates past it, or fails with `IdAllocationFailed` — it never overwrites
  - `PalaceHandle::embed_health` and `palace_reembed` now carry an `AliasAudit`: key presence alone reported a false all-clear for this class, and `is_healthy()` is now false when any drawer is aliased
  - an alias audit that could not run is `AliasAudit::Unavailable`, not zeros; `is_healthy()` is false for it, so a failed scan can never be read as a clean palace
  - new `PalaceHandle::repair_aliases`: the operator surface for the repair, which had no caller at all. Dry-run by default; a real run frees the whole collision group and then re-audits, and reports `Repaired` only when that verification ran and came back clean. `Partial` and `Unavailable` are distinct outcomes and neither is a success
  - `UsearchStore::unalias` now returns `UnaliasOutcome`, carrying the keys it freed but could not parse back into a drawer id instead of dropping them — those drawers would otherwise be missing from the operator's re-embed worklist inside a reported success
  - `alias_audit` no longer drops collision-group keys that are not uuids, and `AliasAudit::is_clean` now consults `key_rows` vs `distinct_vector_ids` rather than the id list alone. Two `VECTOR_KEYS` rows on one `vector_id` with non-uuid keys previously reported `is_clean() == true`, `is_healthy() == true`, and a `clean` repair while leaving the collision in place — the counts come straight off the table and no parse can shrink them, so they are the signal that cannot be fooled
- Secret detector no longer flags `::`-joined Rust symbol paths (closes [#5043](https://github.com/bobmatnyc/trusty-tools/issues/5043))
  - `Bm25Index::queryTopK`, `Sha256Hasher::finalizeInto`, `OAuth2Client::refreshToken` and `Utf8Error::validUpTo` all reached the mixed-case branch and blocked memory writes; `check_secret` runs even under `force`, so only `allow_secret_like` got past it
  - Root cause is the CamelCase rule, not the delimiter set: a segment with two capitals fails `is_human_word_segment` however the token is split, so adding `:` to `IDENTIFIER_DELIMITERS` — the fix the issue proposed — changes nothing
  - `is_symbol_path` decomposes on `::` and decides each segment on its CamelCase word structure: at most one digit run and one stray single letter per word, and a longest word of five letters — three when the segment is 8 bytes or shorter, a graduated floor rather than an exemption
  - The relaxation is keyed on `::` because it appears in no encoder alphabet; `-` and `_` are base64url's own symbols and `.` is the JWT separator, so relaxing the case rule for those measurably widens base64url misses on tokens with no colon at all, and still does not fix this issue
  - A segment too short to hold a three-letter word (`io`, `rc`, `rt`, `os`) is decided on case uniformity instead, so ordinary two-letter module names do not flag the path they sit in
  - Generated-encoder ceilings are unmoved; the measured price is credentials a human writes in path syntax (`secretKey::<blob>`, 1017 → 2022 misses per 30k) plus 37 per 20k at one chunk width, pinned as ratchets alongside a second ratchet for `::`-chunked blobs
- Added a consolidated recurrence corpus covering all seven cycles (#1667, #1676, #2800/#4216, #4312, #4739, #4898, #5043) — 26 false positives and 22 credential shapes in two tables, walked by two tests, so the next change sees the whole accumulated obligation instead of six scattered batteries
- `Bm25Stats` no longer carries `#[serde(default)]` on its fields. A daemon-side
  field rename decoded as `doc_count: 0`, which reads exactly like an empty
  palace; it now fails the decode.
- Catch-up no longer returns only the one paused session it cannot date ([#5072](https://github.com/bobmatnyc/trusty-tools/issues/5072))
  - `session_finder::parse_trusty_mpm_session` derived `paused_at` from the `session-YYYYMMDD-HHMMSS.md` filename alone, so a hand-written snapshot such as `session-20260730-bounce.md` had no timestamp. The watermark filter — `s.sort_key().is_none_or(|ts| ts > wm)`, duplicated verbatim in `catchup/mod.rs` and `catchup/json.rs` — reads an unknown key as "newer than the watermark", so that one undatable record was admitted by every watermark while all 99 well-formed snapshots in the same directory were correctly dropped
  - both `PausedSession` arms now fall back to the file's mtime when the recorded timestamp is missing or unparseable: an undated `.md` filename, and a claude-mpm JSON carrying only `session_id` (whose `paused_at` defaults to `None`). Symmetry matters because the filter is now fail-closed — rescuing one arm alone would have retired the other
  - the duplicated predicate is one `session_finder::filter_sessions_since` returning a `FilteredSessions` receipt. A session with no derivable pause instant is withheld rather than admitted unconditionally, and the count comes back in the return value: the watermark advances past a withheld session and never returns for it, so "nothing paused since last catch-up" and "N sessions existed but could not be dated" must not read alike. `full` still returns everything
- **`prepare_socket_dir` followed symlinks, which defeated the whole scheme**
  ([#5099](https://github.com/bobmatnyc/trusty-tools/issues/5099) review finding
  1). `std::fs::metadata` and `set_permissions` both resolve links, so when the
  socket directory already existed as a symlink the owner and mode checks read
  the *target's* inode and the chmod retargeted it. An attacker who pre-created
  `trusty-<uid>` as a link to any directory the running user owns passed the
  ownership check outright — verified on macOS: `mkdir` returned `EEXIST`,
  `metadata()` reported uid 502 / mode 0777 from the target, and
  `set_permissions` chmod'd the target to 0700. Directory verification now uses
  `symlink_metadata` and refuses a symlink before considering owner or mode. The
  residual post-check swap race is not closed — it needs `openat`/`fchmod` on a
  directory fd, and `/tmp`'s sticky bit makes it impractical — and the module
  documentation now states that boundary instead of claiming more than it holds.
- **A non-directory at the socket-directory path got chmod'd to `0700`.**
  `classify_existing_dir` asserted symlink and ownership but never the file
  type, so a regular file owned by this uid was classified `Narrow`, had its
  mode rewritten, and only then failed `ENOTDIR` at `bind` — mangling a file
  this process did not create and naming neither the cause nor what was
  actually there. It now refuses with `NotADirectory`, which reports the type
  `lstat` found, before anything can chmod it. Both `prepare_socket_dir` and
  `connect_hardened` apply the check.
- **A dialer reported "create socket directory …" while creating nothing.** A
  failed `symlink_metadata` on the connect path mapped to `CreateDir`; it now
  uses a dedicated `StatForConnect` variant.
- `HnswStore::search` now ranks by scanning every point when the index holds
  256 or fewer, instead of traversing the HNSW graph. The traversal returned
  only what was reachable from its descent pivot along pruned layer-0 neighbour
  lists, so a genuinely relevant drawer could be absent from the candidate
  pool — and because `hnsw_rs` seeds its level RNG from OS entropy, *which*
  drawer went missing changed on every palace open. Measured against
  brute-force truth on real 384-dim palace embeddings, 2–4% of queries lost a
  true top-5 neighbour at 6–16 points; recall is now exact below the threshold.
  The scan is also faster than the traversal it replaces at these sizes
  (27µs vs 34µs at 64 points, 45µs vs 56µs at 128); at 256 it costs 5% more.
  Palaces above 256 drawers keep the graph path and its recall loss, which is
  larger there, not smaller — 8.4% of queries lost a true top-5 neighbour at
  1024 points, and 1.8% lost the nearest one. That residual is unchanged by
  this fix (#5171)
- `HnswStore::search` now scores a re-embedded drawer against the vector
  `VECTORS` currently holds for it. Because `hnsw_rs` cannot remove a point, a
  re-upsert leaves the old embedding in the graph until the next palace open,
  and the drawer was ranked by whichever copy was closer — so a query matching
  text the drawer no longer holds came back at distance 0.0, which
  `VectorStore::search` reports as similarity 1.0. `palace_reembed` creates
  that state in bulk. The drawer also no longer occupies two result slots
  (#5171)
- `HnswStore::search` selects the exact path on the live drawer count rather
  than the graph's point count, so deletes and re-embeds accumulating within a
  session can no longer push a small palace back onto the approximate path
  (#5171)
- Below the threshold, `HnswStore::search` no longer trims its candidate list
  before re-scoring re-embedded drawers. A drawer that has been re-embedded is
  ranked provisionally by whichever of its two embeddings is nearer, and
  trimming on that optimistic score let a drawer whose SUPERSEDED vector sat
  near the query push a genuinely nearer drawer out of the results entirely —
  the same lost-neighbour symptom this fix exists to remove, reached through
  re-embedding rather than through the graph. `palace_reembed` puts every
  drawer in that state (#5171)
- the dream cycle no longer panics when it caps drawer text that contains multi-byte UTF-8 — CJK, Cyrillic, emoji, or accented Latin (refs [#5187](https://github.com/bobmatnyc/trusty-tools/issues/5187))
  - root cause: `merge_into` capped merged drawer content with `String::truncate(500)`. `truncate` asserts its argument is a char boundary, so whenever byte 500 of the merged string landed inside a multi-byte char it panicked with `assertion failed: self.is_char_boundary(new_len)`, killing a `tokio-rt-worker` inside the shipped `com.trusty.memory` daemon mid-consolidation
  - the same defect was present a second time in the semantic pass: the failure log for a canonical drawer sliced `&content[..content.len().min(80)]`, so an error-path log statement could itself panic the pass
  - both sites now route through one `char_safe_prefix` helper that rounds the cap DOWN to the nearest char boundary via `str::floor_char_boundary`. The cut is char-aligned, never grapheme-aligned — a combining mark can be separated from its base letter, which stays valid UTF-8 and cannot panic
- `PalaceHandle::forget` now returns `ForgetOutcome` (`Deleted` / `NotFound`) instead of `Result<()>`, so a caller can tell a real delete from a no-op. `drawers.retain` never reports whether it matched, so forgetting a drawer id that was never stored was indistinguishable from deleting one (closes [#5231](https://github.com/bobmatnyc/trusty-tools/issues/5231))
  - a failed drawer-metadata delete is now an error rather than a `warn!` when the drawer existed: redb is what `PalaceHandle::open` reloads the drawer table from, so a surviving row resurrects the drawer on the next open. The metadata delete also runs first, so that failure leaves the drawer wholly intact instead of half-deleted. Vector and KG-triple removal stay best-effort — survivors are orphans reclaimed by `palace_compact`, not undead drawers
  - `purge_expired` and the dream `prune_pass` / `content_prune_pass` / `dream_consolidate_room` counters now report drawers actually removed rather than candidates attempted; the content-prune count could also exceed reality by exiting early on its wall-clock budget

### Changed

- AtlasCloud's seeded `default_model` is now `deepseek-ai/deepseek-v4-flash`,
  replacing `openai/gpt-5.6-sol`
  ([#3765](https://github.com/bobmatnyc/trusty-tools/issues/3765)).
  A live probe found AtlasCloud gates its catalog by account PLAN, not only by
  key validity: a Coding-Plan key answers `403 invalid token for coding plan`
  for `openai/gpt-5.6-sol` and most of the catalog even though `GET /v1/models`
  lists them, so "just pick AtlasCloud" failed on a valid key. The replacement
  was verified live on such a key — a real completion and a real OpenAI-style
  `tool_calls` response — and has a 1,048,576-token context with the cheapest
  rates of the callable set. It is Coding-Plan-informed, which the seed comment
  records; `max_context_window` is unchanged at 1,050,000 because it is the
  provider-level fallback, not this model's own window.
- `bin_resolve::resolve_binary` now delegates to an internal
  `resolve_binary_in`, which takes the well-known-directory fallback list as a
  parameter. Behaviour is identical; the seam exists so the fallback branch —
  "find a binary the process `PATH` does not list", the branch a launchd-spawned
  daemon depends on — can be tested without mutating the process-global `PATH`
  (#4125)
- The memory write path resolves a drawer's `room_id` through the new room
  registry instead of hashing a `RoomType`'s `Debug` string, and the room
  filters on `list_drawers` / `retrieve_l2` resolve the same way. Both fall
  back to the legacy fold when a palace has no registry row, so filtering on an
  un-backfilled palace is byte-identical to before. Room selection remains
  caller-supplied-or-`General`: content and tag inference are deliberately not
  implemented, because a mis-inferred room fails invisibly (ADR-0027 D4.4).
- The documented palace model is `Palace -> Wing -> Room -> Drawer`. A *closet*
  is not a hierarchy level — it is the many-to-many keyword -> drawer-ids
  inverted index on `PalaceHandle`, and the field keeps its name (ADR-0027 D3).
- Room-filter resolution is performed before the drawer (and closet) read guards
  are acquired on the `list_drawers` and `retrieve_l2` paths, so the redb read
  transaction no longer runs while a palace lock is held.
- `retrieve_l3` takes an optional `room_filter`, matching `retrieve_l2`
  (ADR-0027 T7). Deep recall was the one retrieval path a room scope could not
  reach, so a caller narrowing to one room silently got every room back. Callers
  pass `None` for the previous behaviour, which is byte-identical; when a filter
  is set the search over-fetches so filtered-out neighbours do not eat the
  `top_k` budget.
- `RememberOptions` gains a `wing_id: Option<Uuid>` field (ADR-0027 T9, [#4809](https://github.com/bobmatnyc/trusty-tools/issues/4809))
  - `None` (the default) resolves the room in the palace's default wing, which is
    byte-identical to the previous behaviour — every in-tree call site constructs
    `RememberOptions` with `..Default::default()` and is unaffected
  - external callers that build the struct with an exhaustive literal will need to add
    the field; note this when cutting the next release of this crate
- A displaced Tier C drawer now has its own `fact_key` cleared, not just its index entry moved. #4884's storage layer deliberately left the field reading the old slot name, which was correct while nothing read it as a liveness signal; with a write path it would let `load_drawers()` show two drawers claiming one slot. `expires_at` is cleared with it — on a Tier C drawer that field IS the retirement condition D4 demanded, and supersession has discharged it, so the demoted record becomes an ordinary permanent Tier E drawer rather than self-destructing at the next sweep
- The expiry sweeps (`PalaceHandle::open_with_intent`, `purge_expired`) skip drawers holding a Tier C slot. Read-time expiry (#4885) already stops an expired current fact being served, which is the demotion D6 asks for; hard-deleting the row on top of that would destroy the corrected record D6 preserves and orphan the supersession pointer #4887 will hang off it
- **MSRV raised to Rust 1.94** (was 1.91). `aws-config` >= 1.9.0 and
  `aws-sdk-bedrockruntime` >= 1.136.0, published 2026-07-08, declare
  `rust-version = "1.94.1"`; because those are unpinned caret ranges in the
  workspace manifest, `cargo install` **without `--locked`** re-resolves into
  them and then refuses to build on rustc below 1.94.1 — the reported
  `cargo install trusty-code` failure on rustc 1.91.1. Users on rustc
  1.91-1.93 must `rustup update` before installing any `trusty-*` crate. See
  [ADR-0029](../../docs/adr/0029-msrv-1-94-and-edition-policy.md)
  ([#4928](https://github.com/bobmatnyc/trusty-tools/pull/4928))
- `PalaceHandle::embed_health` states its liveness filter as
  `!expired || tier_c` instead of `!(expired && !tier_c)`. Same drawers, same
  order — De Morgan on the guard clippy flagged as `nonminimal_bool`, which
  broke `cargo clippy -p trusty-common --features memory-core --all-targets
  -- -D warnings` on `main`. The new form also reads as the doc comment already
  described it: keep a drawer unless it is expired and not Tier C
- **BM25 and embedder sockets move into a per-uid directory**
  ([#5099](https://github.com/bobmatnyc/trusty-tools/issues/5099)).
  `bm25_client::socket_path_for_palace` and `UdsEmbedderClient::default_path`
  resolved to `$TMPDIR`, falling back to `/tmp` when `TMPDIR` was unset — mode
  `1777` and owned by root on a Linux host, so the socket was reachable by every
  local user and the directory could be neither narrowed nor trusted. Both now
  resolve under `uds::scratch_socket_dir()` — `<$TMPDIR or /tmp>/trusty-<uid>` —
  which the daemon holds at `0700`. **A running daemon must be restarted to be
  found at the new path**; the client and daemon resolvers changed together.
  The extra path segment costs 11 bytes of the kernel's `sun_path` budget (104
  on macOS, where `$TMPDIR` alone is ~50), narrowing the usable palace-name
  length from roughly 37 characters to roughly 26.

## [0.28.0] — 2026-08-03

### Added

- **`inference::streaming::StreamAssembly` — rebuild a `ChatResponse` from
  streamed events (issue #4425).** The inverse of the existing
  `buffered_stream`: push `ChatStreamEvent`s in arrival order, then
  `into_response()` yields the same response the buffered `chat()` path would
  have returned for that turn — text concatenated, tool-call fragments merged
  by `index`, finish reason and usage carried from the terminal `Done`. It is a
  pure synchronous accumulator taking no callback, so the caller keeps its poll
  loop and may `await` arbitrary work (rendering a delta to a UI) between
  pushes. Without it, every consumer adopting `chat_stream` had to hand-roll
  the same three-part bookkeeping — the reason trusty-code's streaming
  migration looked like an agent-loop rewrite rather than one swapped call.
- **`InferenceError::MissingConfig(String)` (issue #4425).**
  `MissingCredential` can only name a `ProviderId` — it carries no
  operator-actionable message and cannot describe a missing NON-credential
  setting (an unset region, an unconfigured model slug). trusty-code's
  migration onto this enum would otherwise have had to flatten those messages
  into `Unsupported`, whose display text ("unsupported inference capability:
  OPENROUTER_API_KEY not set") actively misleads. Classifies as an alarm and
  never as retryable, matching `MissingCredential`. **Breaking for exhaustive
  matches** over `InferenceError` (the enum is not `#[non_exhaustive]`);
  in-tree consumers were verified to build.
- **`PromptTokensDetails` and `UsageBlock` are re-exported at the `inference`
  root.** A consumer that owns a `ChatResponse` owns its wire usage block;
  reading it previously meant reaching into `inference::types::usage`.
- **`InferenceAdapter::capabilities_for(model)` — the model-aware capability
  accessor (issue #4425).** `capabilities()` assumes one adapter serves one
  provider, but a ROUTING adapter picks its backend per request from the model
  slug (trusty-code's `OpenAiCompatClient` spans OpenRouter / Fireworks /
  Together / AtlasCloud; its `DispatchingLlmClient` adds Bedrock). Such an
  adapter could only answer with one hard-wired provider's profile, which is
  silently wrong for every other backend it serves — the OpenRouter-only usage
  directive, `cache_control` support, the tool dialect, and the context-window
  fallback tier all differ. `capabilities_for` defaults to `capabilities()`, so
  every single-provider adapter is unaffected; a routing adapter overrides it to
  resolve through the same gate its `chat`/`chat_stream` use.
  **`context_window`'s default now derives its provider tier from
  `capabilities_for(model)`** rather than `capabilities()` — a behaviour change
  only for adapters that override `capabilities_for` (none did before this
  release). Not a breaking change: both are defaulted trait methods.
- **`trusty_common::supervision::launchd_supervision()` answers in THREE states**, because "launchd does not run this PID" and "launchd could not be asked" have opposite consequences and must never be conflated. An unspawnable `launchctl`, a non-zero exit, or a table with no parseable rows reports `Unknown` — never a confident `Supervised`. `is_launchd_supervised()` is kept as the boolean adapter for the post-upgrade self-restart decision and maps `Unknown` to `false`, the conservative direction there ([#4469](https://github.com/bobmatnyc/trusty-tools/issues/4469)).

- **The `launchctl` query is bounded by a 5-second timeout.** It runs on the daemon-start path, so an unbounded wait would let a wedged or saturated launchd hang startup indefinitely — an availability bug introduced by a diagnostic. The child is killed and reaped on expiry, and the timeout surfaces as `Unknown`, which every consumer already handles ([#4469](https://github.com/bobmatnyc/trusty-tools/issues/4469)).
- **`claude_config::quarantine_path`** — computes a unique, timestamped
  quarantine name (`<path>.corrupt-<UTC stamp>`) for a corrupt config file
  ([#4206](https://github.com/bobmatnyc/trusty-tools/issues/4206)). Two
  independent trusty-mpm writers previously renamed a malformed `.claude.json`
  to the same fixed `.claude.json.corrupt`, so a second quarantine silently
  destroyed the first one's bytes. Purely additive: no existing behaviour
  changes.

- `json_rmw`: cross-process locked read-modify-write for whole-file JSON
  documents — the single implementation of the load → mutate → save critical
  section that `trusty-mpm`'s `projects.json`, `trusty-gworkspace`'s
  `tokens.json` (#3502) and the epic #4207 worktree registry all need.
  `json_rmw::update` takes an exclusive advisory lock on a `<path>.lock`
  sidecar, re-reads the document under that lock (never trusting a caller's
  stale copy), applies the mutation, and publishes atomically via a
  per-writer-unique temp file + `fsync` + `rename` + directory `fsync`. Never
  fails open: a failed lock, read, parse or write returns `Err` with the
  document byte-for-byte unchanged, and only a genuinely absent file starts
  from `Default`. Adds `fd-lock` as an unconditional dependency.

- **`project_index_id` — project-derived trusty-search index identity (#4207).**
  New `ProjectIdentity` (origin + root + operator) with a pure, deterministic
  `index_id()`, plus `derive_project_index_id()` and
  `resolve_operator_identity()`. Unlike
  the basename rule in `index_id` (which collides for unrelated checkouts sharing
  a directory name) and the session-worktree UUID (which binds service identity to
  ephemeral writer isolation), this id *partitions*: the canonical content-tree
  root is a hashed component, so sibling clones, linked worktrees, and differing
  accounts derive distinct ids by construction. Derivation only — nothing is wired
  into `ensure_project_indexed`, `trusty-search serve`, or the daemon's resolution
  path; registry reconciliation and migration of existing indexes are separate
  slices of #4207. No behaviour change for any existing caller.

  Derivation reads no environment variable of its own, but it is NOT fully
  hermetic (corrected, #4269): `resolve_operator_identity` shells out to `git
  config`, so two callers on one tree CAN derive different ids when `HOME` or
  `GIT_CONFIG_GLOBAL` differ and the repo sets no local `user.email` — see
  `project_index_id.rs`'s own note. The launchd daemon and a shell CLI are
  precisely that pair, since the daemon runs under a plist environment while CLI
  invocations inherit the shell's. Set a repo-local `user.email` to pin it. The
  `index_id()` docs enumerate exactly which inputs are mutable — `origin` moves on
  the first commit, on `git remote add origin`, and on a new root commit — each
  pinned by a test, so the migration slice inherits a true guarantee rather than an
  assumption of permanence.

---
- **`inference::bedrock` streams natively via `ConverseStream` (issue #4426).**
  `BedrockAdapter` now overrides `InferenceAdapter::chat_stream` with AWS's
  real streaming operation instead of inheriting the trait's buffered fallback,
  so a `bedrock/*` turn arrives token-by-token rather than as one delta emitted
  after the model already finished. The event handling is ported from the
  implementation proven in production on `chat::bedrock_impl` (#3767) and
  extended: `ContentBlockDelta::Text` → `ChatStreamEvent::Delta`, tool-use
  block starts and partial-JSON argument fragments → `ChatStreamEvent::ToolCall`
  (which the `chat::ChatProvider` path never supported), `MessageStop` +
  `Metadata` folded into the single terminal `Done` carrying the finish reason
  and token tally, and a mid-stream failure surfacing as a terminal `Err` and
  never as a `Done`. Both transports build their request from ONE shared
  conversion (`build_converse_parts`), so streamed and buffered turns cannot
  disagree about messages, sampling, tool config, or the reported
  `finish_reason`. Consumers that delegate `chat_stream` to the shared adapter —
  trusty-code's `BedrockChatClient` — get this with no change of their own.
- 13 credential environment variables that a census of production source found
  in use but that the registry could not name, so no consumer could route them
  through the env → `.env.local` → store precedence even if it wanted to:
  `GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_APP_PRIVATE_KEY`, `GITHUB_WEBHOOK_SECRET`,
  `JIRA_TOKEN`, `JIRA_API_TOKEN`, `LINEAR_API_KEY`, `BITBUCKET_TOKEN`,
  `BITBUCKET_APP_PASSWORD`, `BRAVE_API_KEY`, `GOOGLE_OAUTH_CLIENT_SECRET`,
  `SLACK_APP_TOKEN`, `TAGENT_API_TOKEN`. `SLACK_APP_TOKEN` was the sharpest gap
  — it sat unmapped between its two mapped siblings `SLACK_BOT_TOKEN` and
  `SLACK_USER_TOKEN`. The registry now covers all 23 censused names, pinned by
  `registry_covers_the_full_census`, which fails in both directions so neither
  the table nor the census can drift alone. Registering a name grants nothing
  and migrates no call site: authorization is
  [#4566](https://github.com/bobmatnyc/trusty-tools/issues/4566) and the
  consumer migration is
  [#4571](https://github.com/bobmatnyc/trusty-tools/issues/4571). Three of the
  new entries (`JIRA_TOKEN`, `JIRA_API_TOKEN`, `LINEAR_API_KEY`) are what makes
  [#4478](https://github.com/bobmatnyc/trusty-tools/issues/4478) question (b)
  answerable.
- `credentials::CredentialRef` — the opaque, durable, **non-secret** handle that
  names a credential without carrying it (closes
  [#4565](https://github.com/bobmatnyc/trusty-tools/issues/4565), epic
  [#4040](https://github.com/bobmatnyc/trusty-tools/issues/4040), DOC-45
  `C-2.1`–`C-2.7`). `credential_ref` had zero hits under `crates/*/src/**`
  despite being an acceptance bullet of the closed-as-completed #2808, so
  anything that needed to *refer* to a credential had to *hold* one — which is
  why `McpService.env` is a map of literal API keys in a hand-editable TOML. A
  ref is safe in a git-tracked file, a config row, a log line, an audit record,
  and a model-visible tool result: its grammar is lowercase-kebab segments,
  ≤ 64 bytes, at most one `/`, which no realistic API key, JWT, OAuth token, or
  PEM body can satisfy. Pinned by
  `realistic_credentials_are_rejected_by_the_grammar` against specimens shaped
  like every credential format the registry names. Stable across rotation, and
  shape-agnostic: one type, one entry point, and no code path that exists only
  for OAuth or only for a plain API key.
- `credentials::Secret<T>` — the wrapper a resolved credential comes back in.
  Its `Debug`/`Display` render a constant that is not merely redacted but
  *independent of the value* (the impls carry no `T: Debug` bound, so they
  cannot read it), and it implements neither `Serialize`, `Deserialize`,
  `Clone`, `Deref`, nor `PartialEq`. Each omission is a closed leak path; the
  absent `Serialize` is the load-bearing one, because every config struct in the
  workspace derives it, so a `Secret` cannot compile inside one. Three
  compile-time assertions (the `assert_not_impl` coherence trick, inlined rather
  than adding a dependency) fail the build if any of the three traits is ever
  added.
- `credentials::resolve(&CredentialRef, &Principal, &Scope) -> Result<Secret<String>, CredentialError>`
  — the single resolution entry point, called where the credential is consumed
  rather than at config load (`C-3.3`, `C-8.4`). `resolve_client` is the same
  entry point with the value consumed in place, handing back an
  already-authenticated handle so the caller never sees the string (`C-3.4`,
  DOC-63 `S-5.1`). A ref naming a provider absent from the registry fails with
  `Missing` carrying a remediation that names the registry (`C-2.7`).
- `credentials::CredentialError` — `Missing` / `Denied` / `Expired` /
  `ZeroScope` / `ScopeUnavailable`. Every variant is recoverable, carries the
  `CredentialRef` and the `Principal`, renders an actionable remediation, and
  can hold no secret material by construction. The fifth variant is DOC-45
  `C-5.5`'s deliberate addition to #4040's stated four, kept distinct from
  `ZeroScope` by `C-5.6` because "widen the grant" is advice that cannot be
  followed for a provider that has no such scope.
- `credentials::Principal`, `ServiceId`, `Scope`, `Access` — the vocabulary
  `resolve`'s final signature is written in. `Principal` is a closed,
  `#[non_exhaustive]` enumeration carrying `Operator` and `Service` only:
  DOC-45 `C-1.3` is PROVISIONAL pending owner question Q-B and says in terms
  not to implement the `Assistant` variant until it is answered, so #4566 adds
  `Assistant` and `SubAgent` without a breaking change.
- `SecretString::into_secret` — the one-line migration from the older inference
  wrapper to the canonical `Secret<String>`.
- **`KnowledgeGraph::top_degree_subgraph` and `KnowledgeGraph::expand_neighbors`
  (issue #4670).** Two progressive-exploration primitives over the already-
  resident `petgraph::StableGraph`, backing the palace graph view's bounded
  first paint and click-to-expand. `top_degree_subgraph(limit)` returns the
  highest-degree entities (ties broken by name, so repeated calls are
  byte-identical) plus the induced edges among them, in O(V log V + E).
  `expand_neighbors(entity, direction, max_hops)` runs a direction-aware
  (`ExpandDirection::{In, Out, Both}`), hop-bounded BFS and returns the reached
  nodes — origin first, each carrying its graph-wide degree rather than its
  degree within the fragment — plus every traversed edge. Both emit edges as
  `Triple`s so callers need no second wire format. Neither touches disk.

### Fixed

- `semantic_consolidation::inference_available_false_without_key` no longer
  fails on any machine that exports a real `OPENROUTER_API_KEY` (refs [#4407](https://github.com/bobmatnyc/trusty-tools/issues/4407), [#3451](https://github.com/bobmatnyc/trusty-tools/issues/3451)).
  The test asserts the inference gate stays closed with no key configured, but
  `inference_available("", false)` falls back to reading that variable from the
  process environment — so "absent from the ambient shell" was an unstated
  precondition, and its `#[serial]` group excluded concurrent test writers while
  doing nothing about the environment the suite inherits. It now CLEARS the
  variable for its body via an `EnvVarGuard::clear` (restored on drop), which is
  what it always claimed to test.
- **`is_launchd_supervised()` was an environment-variable heuristic an unsupervised child could satisfy, so it self-reported as supervised** ([#4469](https://github.com/bobmatnyc/trusty-tools/issues/4469)). It returned `true` whenever `XPC_SERVICE_NAME` was set and `TERM_PROGRAM` was not — a condition every child inherits for free — with a `getppid() == 1` fallback that any orphan whose parent exited also satisfies. A `tm daemon` child could therefore pass the [#4230](https://github.com/bobmatnyc/trusty-tools/issues/4230) callee-side guard and publish `supervised: true` on `/health`, making the documented `curl /health | jq '.supervised'` restart verification unsound in exactly the case it exists to catch. The answer now comes from launchd itself: the new `trusty_common::supervision` module matches `std::process::id()` against the PID column of `launchctl list`. It is an EXACT PID match, never an ancestor walk — `Terminal.app` is itself a launchd job, so walking the tree would reintroduce the same false positive.
- **The `memory_remember` secret scanner no longer false-positives on
  slash-separated issue/PR-number lists (issue #2800).** A checkpoint
  enumerating tickets as `#2763/#2774/#2780/#2782/#2790` was rejected as a
  "likely secret/credential token": the `/` separators set the base64-symbol
  flag and the digits set the digit flag, so the base64-blob branch of
  `looks_like_secret` fired on a token containing no letters at all. Observed
  live twice; agents worked around it by rewording or dropping detail, silently
  degrading session-checkpoint fidelity. `looks_like_secret` now allowlists
  tokens built only from `#`, `/`, and ASCII digits, alongside the existing
  git-SHA carve-out. The exemption is charset-scoped and strictly narrower than
  the SHA allowlist — a single alphabetic character, or a `+`, takes a token
  back onto the normal heuristic path, so every credential shape the module
  already blocked stays blocked.

- **`project_index_id` documentation corrected**
  ([#4269](https://github.com/bobmatnyc/trusty-tools/issues/4269), amended under
  [#4288](https://github.com/bobmatnyc/trusty-tools/issues/4288)). The module
  stated without qualification that `root` is immutable and recommended that the
  wiring/migration slice "reconcile on `root`". Four routine actions move it —
  `mv proj`, a GitHub repo rename, a repo transfer, and `git remote remove
  origin` — and reconciling on it would reintroduce the silent-orphan class the
  identity work exists to remove. The immutability claims are now qualified
  ("under ordinary git operations"), the four movers are documented, and the
  recommendation points at a git-maintained anchor instead. The CHANGELOG's
  hermeticity claim is likewise corrected: two callers CAN derive different ids
  when `HOME`/`GIT_CONFIG_GLOBAL` differ and the repo sets no local
  `user.email`. Documentation only — no code change.
- `bin_resolve::is_ephemeral_build_path` now also rejects any path under a system temp root (`/tmp`, `/private/tmp`, `/var/tmp`, macOS `/var/folders`, and the live `std::env::temp_dir()`), matched as component-wise path prefixes rather than substrings. Previously the guard enumerated only `target/debug`, `target/release` and the two worktree layouts, so a scratch binary under an agent harness's temp scratchpad read as an ordinary installed path and was accepted as stable (#4485).
- A model slug's provider prefix no longer reaches a direct provider on the wire
  (closes [#4493](https://github.com/bobmatnyc/trusty-tools/issues/4493)).
  `provider_for` CONSUMES the `<prefix>/` marker to route — so with an
  `OPENAI_API_KEY` present, `openai/gpt-4o-mini` routed to OpenAI-direct — but the
  slug then travelled into the request body unchanged, and `api.openai.com` 400s
  on a model id it does not publish. The new `ProviderId::wire_model_id` removes
  exactly one leading marker, and only when it names that provider, so a slash
  inside a real model id survives (`accounts/fireworks/models/…`,
  `meta-llama/…`) and a nested vendor segment does too
  (`atlascloud/openai/gpt-5.6-sol` → `openai/gpt-5.6-sol`). OpenRouter is exempt
  and still transmits the full `vendor/model` slug verbatim — it routes by that
  slug and serves first-party models under a genuine `openrouter/` vendor. The
  two adapters that had each hand-rolled this strip for one provider
  (Bedrock, Anthropic-direct) now delegate to the shared rule.
- `PalaceRow` now distinguishes an unknown count from a zero one ([#4682](https://github.com/bobmatnyc/trusty-tools/issues/4682))
  - `GET /api/v1/palaces` returns `cached: false` with all counts zeroed for any palace whose handle is not resident (2,180 of 2,183 on a live daemon); those zeros mean *unknown*, not *empty*
  - rows parsed from an uncached entry are flagged `counts_unknown`, and the new `vectors()` / `drawers()` / `kg_triples()` / `nodes()` / `edges()` accessors return `Option<u64>` so a renderer cannot print a placeholder as a measurement
  - the dashboard memory panel, the `trusty-memory monitor palaces` CLI, and the `/ui` web dashboard render `—` for those counts and sum only measured ones
  - a daemon that omits `cached` entirely (pre-#4640) is still trusted, so counts do not regress to `—` against an older daemon
  - `MemoryClient::fetch_palace()` hits `GET /api/v1/palaces/{id}` to fetch live counts for a single palace, alongside the new `parse_palace_detail()` and `format_opt_count()` helpers

### Changed

- `credentials` is now a top-level module, `trusty_common::credentials`, rather
  than a submodule of `inference` (closes
  [#4564](https://github.com/bobmatnyc/trusty-tools/issues/4564), epic
  [#4040](https://github.com/bobmatnyc/trusty-tools/issues/4040), DOC-45). The
  resolver was never inference-specific — four of its ten registry entries were
  Slack, Slack-user, Telegram and `claude-code` tokens, and its own doc comment
  already said *"Not limited to inference providers"* — so the path was actively
  misleading consumers into adding a raw `std::env::var` read instead of finding
  it. `trusty_common::inference::credentials` remains as a `#[deprecated]`
  re-export for one release; every in-tree consumer moved to the new path in the
  same change. The `credentials` cargo feature is unchanged and still builds
  without `inference-client`.
- The provider registry moved to `credentials::registry` and is now a
  `REGISTRY` table rather than a `match`, so it can be enumerated and asserted
  complete. `env_var_for` keeps its signature and its case-insensitive lookup;
  `registered_providers()` is new.
- `inference::types::SecretString` is documented as superseded by
  `credentials::Secret`. Its behaviour is unchanged; note that its
  four-character head preview does **not** meet DOC-45 `C-8.2`, and collapsing
  the two types is left to a follow-up because it requires changing the existing
  test that pins that preview's shape.

Note: this change adds **no authorization**. `resolve` accepts a `Principal` and
checks no grant against it — grants, the ACL, default-deny and the code-owned
floor are [#4566](https://github.com/bobmatnyc/trusty-tools/issues/4566). The
signature is final so no consumer is migrated twice.
- **`bin_resolve::is_under_system_temp` is now public (issue #4638).** Promoted
  from a private helper of `is_ephemeral_build_path` so trusty-code's turn
  recorder can ask the one question it needs — "is this project root under a
  system temp root?" — without duplicating the #4485 temp-root prefix,
  `TMPDIR`, and macOS symlink-alias logic. Behavior is unchanged; this is a
  visibility change only.
  - Callers deciding whether a PROJECT root is durable must use this rather
    than `is_ephemeral_build_path`, whose `EPHEMERAL_PATH_SEGMENTS` half also
    flags `.claude/worktrees/` — an ephemeral BINARY location but a durable
    project checkout that carries the repo's git remote. A new test
    (`is_under_system_temp_is_true_for_temp_and_false_for_worktrees`) pins that
    distinction so the two predicates cannot quietly converge.

## [0.27.0] — 2026-07-27

MINOR, not patch — deliberately. `ChatEvent::Usage` (added below) is a new
variant on an enum that was neither `#[non_exhaustive]` nor private, so every
downstream exhaustive `match` over it stops compiling with E0004; five
in-workspace consumers had to gain an arm. Shipping that as 0.26.3 would have
let `^0.26` re-resolve the ALREADY-PUBLISHED, arm-less consumer sources onto
the new variant and hard-fail `cargo install` — the exact failure that forced
`trusty-analyze` 0.7.3 to be yanked on 2026-07-27. `^0.26` excludes 0.27.0, so
published consumers keep resolving to 0.26.2 and stay installable.

### Changed

- **`chat::ChatEvent` is now `#[non_exhaustive]`.** Downstream `match`es must
  carry a wildcard arm. This is itself the breaking change that the MINOR bump
  covers, and it is an ADDITION to that bump rather than a substitute for it:
  it does nothing for consumers published before it landed, it only stops the
  *next* variant addition from repeating the break.

### Added

- **`chat::BedrockProvider` now streams via `ConverseStream` instead of
  buffering a full `Converse` reply into a single delta (issue #3767).**
  `chat_stream` drives Bedrock's binary event-stream framing: `ContentBlockDelta`
  text fragments become incremental `ChatEvent::Delta`s, mid-stream failures
  (throttling, validation, transport errors) emit `ChatEvent::Error` and
  return `Err` — mirroring the OpenAI-compatible SSE pump's #3757 dual-channel
  failure contract — and the terminal `Metadata` event's token tally becomes
  a new `ChatEvent::Usage` (Bedrock reports usage exactly once, never
  per-delta, so it needed its own event to avoid being silently dropped).
  `BedrockProvider` also gained `with_sampling` (parity with
  `OpenRouterProvider::with_sampling`, #3758) so a Bedrock-routed streamed
  turn honours the same temperature/token-ceiling/stop-sequences as the
  blocking path.
- **`chat::ChatEvent::Usage(ChatUsage)`** — a new event variant carrying
  prompt/completion/cache token counts for providers that report usage
  out-of-band from text deltas. `ChatUsage` is re-exported from the crate
  root alongside `ChatEvent`.

---
## [0.26.2] — 2026-07-26

### Added

- **`chat::SamplingParams` — sampling parity for streamed turns** (issue
  #3758): the OpenAI-compatible streaming request wire sent only
  `model`/`messages`/`tools`, so a streamed reply silently ran on provider
  defaults while the blocking path for the same turn sent the caller's
  configured temperature, token ceiling, and stop sequences — a documented
  contract (`LlmConfig::stop_sequences` claims to be forwarded on the
  OpenRouter path) that the streaming path was not honouring.
  `OpenRouterProvider::with_sampling` / `OllamaProvider::with_sampling` attach
  `temperature`, `max_tokens`, and `stop` to every request. All three fields
  are omitted when absent (an empty `stop` array is never sent — some servers
  reject `"stop": []`, the same trap an empty `tools` array has), so a caller
  that does not opt in produces a byte-identical request body to before.

### Fixed

- **`PalaceRegistry::open_palace` no longer queues callers indefinitely
  behind sustained redb writer-lock contention** (issue #3992): the
  per-palace `open_lock` that serialises concurrent opens of the same
  palace id was acquired with an unbounded `Mutex::lock()`. Each individual
  `try_open_or_snapshot` retry under `OpenIntent::Writer` was already
  bounded (~1.55s), but a failed Writer open is never cached, so every
  caller queued behind the lock repeated that bounded dance from scratch —
  the Nth queued caller waited for N-1 earlier callers' own attempts to
  finish first, with no ceiling on the total. Reproduced live: 5 concurrent
  openers against a genuinely held lock made the 5th caller wait 9.86s, and
  this exact linear-with-queue-depth pattern is how a single
  `memory_remember` call was observed to hang for 1800s under sustained
  multi-process contention. `open_palace` now acquires the lock via
  `try_lock_for` with a new, independently-tunable
  `memory_core::timeouts::open_queue_timeout()` (default 60s,
  `TRUSTY_OPEN_QUEUE_TIMEOUT_SECS`), returning a clear error instead of
  hanging once the deadline elapses.

- **`default_model_matches_sentence_transformers_reference` no longer
  misreports a wrong-model load as an fp32 accuracy failure** (issue #3711):
  the merge-gate flake that failed at `mean_cosine=0.990649` vs `>= 0.999` was
  never numeric drift — a correctly loaded fp32 `all-MiniLM-L6-v2` measures
  1.000000 against the reference vectors, and float re-association across
  runners/microkernels moves that by ~1e-6, so a sub-0.999 result is only
  reachable by the INT8 `all-MiniLM-L6-v2-int8` variant (reproduced locally at
  0.995703 by loading INT8 under the pre-fix test, with the identical
  misleading "accuracy" message). Two paths could substitute INT8 unnoticed:
  the test read `TRUSTY_EMBEDDER_MODEL` through `FastEmbedder::new()` while
  holding no `ENV_LOCK`, so the sibling
  `resolve_default_embedding_model_int8_opt_in` could have `int8` set
  process-globally at that instant; and `FastEmbedder::with_cache_size`'s
  two-model robustness net silently loads INT8 when the fp32 primary fails to
  initialise (plausible on a disk- and memory-tight CI runner). The test now
  pins `TRUSTY_EMBEDDER_MODEL` unset under `ENV_LOCK` for the whole
  construct-and-embed window — which also serialises the `TRUSTY_DEVICE` write
  the CPU-fallback path performs — and asserts `model_name()` reports the fp32
  variant *before* the cosine gate, so a substituted model fails as "the fp32
  primary never loaded" with the tracing warning to check. The 0.999 threshold
  is deliberately unchanged and now carries its justification in
  `REFERENCE_MIN_MEAN_COSINE`'s doc comment, stated as deficit-from-perfect
  ratios: the 1e-3 budget is ~1000x larger than fp32's ~1e-6 drift, while INT8
  misses it by ~10x (#3486's 0.9897) or ~4x (the 0.995703 reproduced here) —
  a single order of magnitude, which is exactly why the wrong-model
  discriminator is the `model_name()` assertion rather than the threshold.
  Loosening it would have deleted the test's diagnostic value while leaving
  both causes in place. Not `#[ignore]`d.

- **The `embedder` module tree now has ONE env lock, not two** (issue #3711):
  `provider_tests.rs` declared its own independent `tokio::sync::Mutex`
  `ENV_LOCK`, separate from `mod.rs`'s `std::sync::Mutex`, so its
  `TRUSTY_DEVICE` / `TRUSTY_EMBEDDER_MODEL` mutations did not serialise against
  the tests in `mod.rs` — two locks guard nothing from each other, leaving the
  same wrong-model race open (on macOS, or the moment either `#[ignore]` there
  is lifted) and making `mod.rs`'s "one shared lock across all env-touching
  tests in this binary" claim false crate-wide. Both are now hoisted to a
  shared `embedder::test_env` module holding the single lock and RAII guard;
  the four tests in `provider_tests.rs` became synchronous so they can take it,
  building an explicit current-thread runtime where they need one.

- **Chat SSE pump surfaces stream failures instead of ending them as `Done`**
  (issue #3757): `chat::openai_compat`'s shared pump never emitted
  `ChatEvent::Error`, so a mid-stream `{"error": {...}}` frame, a stream cut off
  mid-frame, and a 200 answered with a non-SSE body ALL arrived at the consumer
  as a clean `ChatEvent::Done` — a partial answer rendered as a complete one,
  and `trusty-agents`' blocking fallback never engaged. The pump now mirrors the
  three guards `inference::streaming` shipped in #3751: an in-band error probe
  that reads a numeric (`429`) or string (`"insufficient_quota"`) `code`;
  incomplete-frame detection at EOF (bytes still buffered mid-frame is a
  truncation, while a clean boundary without `[DONE]` remains a normal finish,
  since not every provider sends the sentinel); and a `Content-Type` guard.
  Failures are reported BOTH as `ChatEvent::Error` and as an `Err` return,
  because consumers are split between watching the channel (trusty-memory,
  trusty-analyze) and joining the pump task (trusty-agents, trusty-search,
  trusty-mpm's activity monitor).

  Three refinements from review: the frame decoder distinguishes a FAILED stop
  from a normal one, so an error frame returns `Err` instead of the `Ok(())` an
  earlier revision returned (which silently broke the both-channels contract
  for consumers that only join the task); a present-but-null `"error": null`
  key — a real wire shape for providers that always include the field — is not
  treated as a failure; and the truncation detector now decodes the residual
  tail first, so a final frame missing only its trailing newline (including a
  bare `data: [DONE]`) still completes cleanly rather than flipping a working
  stream to an error and costing the caller a second blocking LLM call.

- **A non-SSE streaming response degrades instead of vanishing** (issue #3757):
  a gateway that strips streaming and returns a buffered 200 previously decoded
  to zero deltas plus `Done`, dropping the whole answer. Such a body is now
  replayed as its `message.content` + `message.tool_calls`; a body that still
  carries `data:` frames under the wrong `Content-Type` is decoded as SSE
  anyway, so a mislabelled-but-valid stream keeps working; and only a body with
  nothing renderable becomes an error.

- **A chunk boundary splitting a multi-byte UTF-8 character no longer discards
  the chunk** (issue #3757): the pump decoded each raw chunk with
  `str::from_utf8` and skipped the entire chunk on failure, silently losing
  text whenever a character straddled a socket read. It now buffers bytes and
  decodes per complete line.

- **`catchup::session_finder::extract_section` no longer absorbs trailing
  header text** (issue #3901): a substring match on `## <header>` treated a
  hand-written header like `## Next Steps (all Bob's call — none required)`
  as a match for `"Next Steps"`, silently prepending the trailing annotation
  into the parsed section body — a real corruption reproduced from this
  project's own `.trusty-mpm/sessions/session-20260721-020826.md`. The
  matcher now skips past the entire header LINE (through its own newline)
  before capturing the body, regardless of what follows the header text on
  that line, so the trailing annotation never leaks in. An earlier draft of
  this fix instead rejected any header with trailing text outright
  (fail-closed, returning `None` for the whole section) — code review caught
  that this was a worse regression than the bug it fixed: annotated headers
  (`## In Progress (BACKGROUND AGENTS DIE AT PAUSE — …)`, `## Next Steps
  (RESUME PLAYBOOK)`) were this project's own standard hand-written
  convention for weeks before the native `pause.rs` writer existed, not an
  isolated incident, and `render_session` silently omits a `None` section
  with no warning. Simulated against this project's own 50-file
  `.trusty-mpm/sessions/*.md` archive (07-06 through 07-24, 300 section
  extractions): the fail-closed draft regressed 64 from present-content to
  `None`; the shipped line-skip fix regresses zero and additionally corrects
  64 sections that previously carried leaked annotation-text prefixes.
  Regression tests added, including a representative sample of the corpus's
  annotated-header styles.

---
## [0.26.1] — 2026-07-24

### Added

- `update::perform_upgrade_captured` (#3830): a sibling of `perform_upgrade`
  that CAPTURES the `cargo install` subprocess's stdout/stderr instead of
  inheriting them, for a caller that may be driving its own live terminal
  display concurrently (trusty-installer's `LiveChecklist`) — an inherited
  child writing straight to that terminal corrupts an active
  `indicatif::MultiProgress` redraw. `perform_upgrade` itself is unchanged
  (still inherits, for trusty-memory's/trusty-search's own `upgrade`
  commands, which intentionally want cargo's live build output visible).

### Fixed

- **`room_filter` silent no-op in `retrieve_l2`** (#3274): a caller-supplied
  `room_filter` was accepted but never applied — an empty if-body silently
  dropped it, so results always included every room regardless of the
  filter. Now enforced: `drawer.room_id` is compared against
  `room_to_uuid(&filter)`, the same deterministic hash `list_drawers` already
  uses to filter by room (no real Room table is wired yet, but the mapping
  was already available at this call site).
- **`update::tests::cache_fresh_returns_some_when_newer` deflaked (issue
  #3689).** `check_throttled_skips_when_no_update_check_set` /
  `check_throttled_skips_when_ci_set` mutate `NO_UPDATE_CHECK_ENV`/`CI_ENV`
  for the duration of their own `check_throttled(...).await` (deliberately
  outliving the brief `ENV_LOCK` critical sections around the set/remove
  calls), so a concurrently-scheduled `cache_fresh_returns_some_when_newer`
  could observe the stray var and flip an expected `Some` to `None`. All
  three tests now share `#[serial(update_check_throttled_env)]`, matching
  the crate's existing named-group convention (#3608/#3629).

---
## [0.26.0] — 2026-07-23

### Added

- **`InferenceAdapter::chat_stream` + OpenAI-compat token streaming** (epic
  #3696, Gap B — OpenAI-compat lane): real incremental token streaming for
  every OpenAI-dialect provider (OpenRouter foremost) through the unified
  inference adapter. Adds the `chat_stream` trait method (with a buffering
  default impl that replays a completed `chat()` response, so every adapter is
  stream-callable), the provider-neutral event model (`ChatStreamEvent`,
  `ToolCallDelta`, `StreamCompletion`, `ChatStream`), and the
  partial-UTF-8-safe `SseDecoder` + `decode_event_stream` / `buffered_stream`
  adapters in the new `inference::streaming` module. `OpenAiCompatAdapter`
  overrides `chat_stream` with a real SSE transport (`stream:true` +
  `stream_options.include_usage`), yielding ordered text/tool deltas followed
  by one terminal event carrying the finish reason + usage. Handles
  keep-alives, split-mid-token / mid-codepoint chunks, CRLF line endings,
  `[DONE]`, and cancellation (dropping the stream aborts the request).
  Fail-loud on the failure modes a naive decoder silently completes: in-band
  error chunks with either a numeric OR a string `code`
  (`"insufficient_quota"`) surface as a terminal `Err`; a stream truncated
  mid-frame at EOF surfaces an incomplete-frame `Err` (not a clean `Done`); a
  non-2xx `stream=true` handshake surfaces as an `Err` the caller can retry
  non-streaming; and a 2xx response whose body is NOT `text/event-stream` (a
  buffering gateway/LB) degrades gracefully — the buffered body is parsed and
  replayed — rather than being dropped as zero deltas. The non-streaming
  `chat()` path is unchanged, and `ChatRequest` gains no new field (the
  `stream` flag is injected only on the streaming wire body).
- **`TmuxCommand::StartServer` / `TmuxCommand::ShowGlobalOption`** (trusty-mpm
  issue #3386): two new shared `tmux` command variants — `start-server`
  (idempotent server-existence guarantee) and `show-options -g -v <name>`
  (read back a global option's current value) — for consumers that need to
  confirm the tmux server exists (and that a `set-option -g` actually landed)
  before relying on either, since `set-option -g` itself has no
  auto-start-server behavior in tmux.

---
## [0.25.0] — 2026-07-23

### Added

- **`session_naming::dedupe_by_ordinal`** (issue #3692): auto-suffix-on-collision
  name deduplication shared by every trusty-mpm name-allocation site (`rename`,
  `adopt_existing`, `create`'s final safety net) — picks the smallest free `-N`
  ordinal, incrementing an existing trailing ordinal in place rather than
  double-suffixing. Lives in the new `session_naming::dedupe` submodule (split
  out to keep `session_naming` under its 500-SLOC production cap).

- **`Bm25Index::upsert_document_reporting`** (issue #3684, trusty-search
  #3683 slice 1): identical cap/tokenize/insert semantics to
  `upsert_document`, but returns whether the document was accepted or
  dropped by the corpus cap instead of relying on a process-wide log-once
  latch — for bulk rebuild callers (idle-evict rehydrate, warm boot) that
  want a fresh per-rebuild dropped-count. `upsert_document` itself is
  unchanged behaviorally; it now delegates to this method internally.

---
## [0.24.2] — 2026-07-22

Patch release closing unpublished source drift under the already-published
0.24.1 (issue #3366 defect class): five commits landed on `main` *after*
0.24.1 was published to crates.io without a version bump, so the live 0.24.1
tarball does not contain them — most notably the `search_index::ensure_project_indexed`
signature change below, which blocked `trusty-mpm` 0.20.0 from publishing
against the live registry. This release carries all five.

### Fixed

- **Flaky env-mutation-race unit test fixed (#3608), plus an audit sweep
  for the same bug class (#3607/#3608 cross-reference #2718):**
  - `update::tests::verify_installed_binary_finds_binary_via_path` (#3608)
    mutated `PATH` via `unsafe { std::env::set_var(...) }` OUTSIDE any
    `ENV_LOCK` critical section, so both that mutation and the awaited
    `verify_installed_binary` call ran unguarded against the other
    `verify_installed_binary_*` tests in the same file. Now wraps the PATH
    mutation in its own brief `ENV_LOCK` section (matching the file's
    existing convention) AND tags all four `verify_installed_binary_*`
    tests `#[serial(update_verify_installed_binary_env)]` so the whole
    async body — including the `.await` a `std::sync::Mutex` guard can't
    span without tripping `clippy::await_holding_lock` — is isolated from
    sibling tests touching the same `HOME`/`CARGO_HOME`/`PATH` vars.
  - Audit follow-up: `OPENROUTER_API_KEY` was mutated (or read as an
    implicit fallback) by tests in three places using three UNCOORDINATED
    lock groups — `inference::credentials::{resolver,dotenv}::tests` under
    the named `#[serial(dotenv_credential_env)]` group,
    `memory_core::semantic_consolidation::tests::resolve_openrouter_api_key_falls_back_to_env`
    under a bare (unnamed, and therefore different) `#[serial]`, and
    `memory_core::dream::tests`'s five `EnvVarGuard`-based tests under NO
    lock at all. `memory_core::semantic_consolidation::tests::inference_available_false_without_key`
    also implicitly depends on the var being absent (via
    `inference_available`'s env fallback) with no lock whatsoever. All of
    these now share the single `dotenv_credential_env` serial group.

- **`EmbedderSupervisor`'s give-up latch never tripped under a
  crash-loop-after-successful-probe pattern (issue #3635).**
  `consecutive_failures` — the crash-storm counter `should_give_up()` depends
  on — was reset to `0` unconditionally on any successful respawn probe, so a
  subprocess that reliably answered its startup probe and then immediately
  crashed again could never escalate past `max_restarts`: every crash was
  wiped by the next "successful" respawn before it could count. The
  supervision loop now applies the same sustained-health gate
  `consecutive_wedge_restarts` already used (`wedge_counter_should_reset`,
  #1450 HIGH follow-up) to `consecutive_failures` too — evaluated
  immediately after each trigger resolves (not merely at the top of the loop
  before the blocking wait, which would lag a full crash-cycle behind a
  genuine recovery window) — so mere respawn-probe success no longer resets
  it; only `config.wedge_reset_secs` of observed health since the last crash
  does. A transient single crash followed by genuine sustained health still
  resets normally.
  - Test: `supervisor_gives_up_after_max_restarts_flips_has_given_up` (now
    passes deterministically via the crash-storm counter itself, not by
    accident via the sibling wedge counter),
    `supervisor_transient_crash_then_sustained_health_resets_counter_no_premature_give_up`.

- **De-flaked `supervisor_transient_crash_then_sustained_health_resets_counter_no_premature_give_up`
  (test-only follow-up to #3635, no product-code change).** The test added
  above observed the first (deliberate) crash by polling `pid_slot` for a
  zero-crossing under a bounded wall-clock timeout — racing the same
  scheduler-dependent `child.wait()`/reader-task-EOF timing #3635 itself
  documents, and intermittently timed out on a slower/more-contended CI
  runner. It no longer tries to observe that intermediate transition at
  all: since the mock is deterministic by construction (first invocation
  always crashes after its own probe; every later invocation is healthy),
  the test now retries a real `embed_batch` call in a loop until one
  succeeds — a genuine successful response, not a timing-dependent
  observation of an intermediate signal, absorbing any amount of internal
  crash-detection/respawn latency.

- **`search_index::ensure_project_indexed` no longer hardcodes
  `allow_sensitive_path: true` (issue #2914).** The parameter is now explicit
  per caller: trusty-code's task-start caller still opts in (its `directory`
  binding can legitimately live under an OS-temp prefix, issue #2747), but
  trusty-mpm's session-launch caller now passes `false`, since a real session
  workspace is never an OS-temp path — closing the ephemeral test/self-heal
  index leak into the production trusty-search index set.

- **CoreML is now opt-in, not the default, execution provider on Apple
  Silicon (issue #3493 P0 part 2).** CoreML measurably degraded embedding
  accuracy (~0.99 mean cosine similarity vs a genuine
  `sentence-transformers` reference, vs 1.000000 on the CPU EP), silently
  erasing the fp32 default-model accuracy fix from #3486. `FastEmbedder`
  now defaults to CPU on every platform, including Apple Silicon; set
  `TRUSTY_DEVICE=gpu` to explicitly opt back into CoreML acceleration
  (`TRUSTY_COREML_COMPUTE_UNITS` still selects the CoreML variant once
  opted in). `TRUSTY_DEVICE=cpu` keeps working as before. Local
  measurement on Apple Silicon showed no consistent throughput cost from
  the CPU default (CPU and CoreML(ANE) landed within noise of each other,
  ~127-141 texts/sec on a 300-text batch).
- **`default_model_matches_sentence_transformers_reference` is no longer
  `#[ignore]`d (issue #3493 P0 part 2).** This is the correctness gate that
  would have caught the CoreML-default accuracy regression; CI now
  pre-seeds its fp32 `Qdrant/all-MiniLM-L6-v2-onnx` model alongside the
  existing quantized-model pre-seed so it runs on every PR without
  HuggingFace-download flakiness.

---
## [0.24.1] — 2026-07-21

Patch release closing unpublished source drift under the already-published
0.24.0 (issue #3366 defect class): two commits landed on `main` *after*
0.24.0 was published to crates.io (from `831103dd`) without a version bump,
so the live 0.24.0 tarball does not contain them. This release carries both.

## [0.24.0] — 2026-07-21

### Added

- **`EmbedderSupervisor::terminated_signal()` (epic #3524 slice 6 PR-4
  follow-up, code-critic BLOCK on trusty-search PR #3584).** A new
  `Arc<AtomicBool>` accessor, captured the same way `child_pid_slot` is
  captured from `spawn_stdio`'s return tuple — before
  `start_supervisor_task()` consumes `self`. The supervision loop sets it to
  `true` at the exact instant, and ONLY the instant, it decides to give up
  permanently (`should_give_up`: exhausted `max_restarts` or a wedge-restart
  storm) — never on a clean exit, an intentional cooperative `shutdown()`,
  or an ordinary respawn. Gives callers (trusty-search's swap-back watchdog)
  a DEFINITIVE "this supervised process is unrecoverably dead" signal
  instead of inferring it from `child_pid_slot == 0`, which is also `0`
  during an ordinary intentional shutdown and therefore ambiguous on its
  own.
  - Test: `supervisor_terminated_signal_fires_after_exhausting_max_restarts`
    (a real, always-crashing mock child with `max_restarts: 1`),
    `supervisor_terminated_signal_stays_false_on_intentional_shutdown` (a
    real cooperative `shutdown()` must never set it).
- **`monitor::search_client::resolve_search_url` regression coverage (issue
  #3545 follow-up, trusty-search PR #3602 review).** No production behavior
  change in this crate — `resolve_search_url` already discovered a daemon
  registered via `write_daemon_addr("trusty-search", …)`. The trusty-search
  PR #3602 review found that trusty-search's CLI daemon-discovery fix had
  stopped calling `write_daemon_addr` at all, silently starving this
  resolver (and its trusty-search/trusty-installer consumers) of any
  writer. trusty-search restored a writer for the default
  (`TRUSTY_DATA_DIR`-unset) instance; this crate adds
  `resolve_search_url_discovers_non_default_registered_address`, a
  regression test pinning that `resolve_search_url` correctly picks up a
  non-default-port address written that way, so a future regression here
  fails loudly instead of silently breaking `trusty-search monitor
  status`/`monitor indexes`/`monitor tui`'s `[r]` reindex hotkey.
- **`ExecutionProvider::Mps` + `resolve_expected_python_provider` (epic #3524
  slice 6, PR 2/5, refs #3530, #3493 P1)** — a new `Mps` variant on
  `embedder::ExecutionProvider` for the opt-in Python/MPS embedding sidecar
  (`trusty-embedderd-py`), plus a pure `resolve_expected_python_provider()`
  function mirroring the sidecar's own device resolver
  (`trusty_embed_sidecar.model.resolve_device`): `TRUSTY_DEVICE=cpu` → `Cpu`;
  else aarch64-macOS → `Mps`; else an `embedder-cuda` build → `Cuda`; else
  `Cpu`. Lets the parent `trusty-search` process predict the sidecar's
  provider without an RPC round-trip, the same pattern
  `resolve_expected_provider` already uses for the ORT sidecar — fixes
  `/health` reporting `CoreML` for a sidecar that never touches ONNX Runtime.
- **`FastEmbedder::model_name()` (issue #3530 — the `(Q)` observability
  bug)** — `FastEmbedder` now stores the RESOLVED `EmbeddingModel` variant at
  construction (tracking the primary→CPU-retry→fallback-model chain in
  `with_cache_size`) and exposes it via `model_name()` — one of
  `"all-MiniLM-L6-v2"` (fp32 default) or `"all-MiniLM-L6-v2-int8"` (the
  `TRUSTY_EMBEDDER_MODEL=int8` opt-in). Lets `trusty-search` and
  `trusty-embedderd` report the true loaded model instead of the stale
  hardcoded `"AllMiniLML6V2Q"` string that predated the fp32-default flip
  (#3486 / #3493 P0).
- `update::verify_installed_binary_at_path`: health-gates a binary at a KNOWN, CONCRETE path via `--version`, never resolving by name. Complements the existing name-based `verify_installed_binary` (which intentionally searches `$CARGO_HOME/bin`/`~/.cargo/bin` then `~/.local/bin` then `$PATH`) for callers that already know exactly where they just placed a binary — a name-based re-resolution afterward is shadowable by a stale earlier-priority/earlier-PATH copy of the same name (trusty-installer#3554).
- **`EmbedderClient::last_reported_device()`** — a new default-`None` trait
  method for a real (wire-reported) backend device readback, as opposed to
  the build-features/env-predicted `ExecutionProvider` (epic #3524 slice 5,
  issue #3493 P1). `StdioEmbedderClient` overrides it, capturing the optional
  `device` field the Python/MPS sidecar now echoes in its response frames;
  every other transport keeps the `None` default unchanged.
- **`SupervisorHandle::has_given_up()`** — a new non-blocking readback of
  whether `EmbedderSupervisor`'s supervision loop has permanently given up
  respawning its sidecar (crossed `max_restarts` on either the crash-storm or
  wedge-storm counter — see `should_give_up`), exposed via a second one-way
  `watch` channel alongside the existing shutdown signal (PR #3560 review,
  HIGH fix). Lets a caller (trusty-search's `FallbackEmbedderAdapter`)
  observe the supervisor's actual give-up decision instead of reconstructing
  an independent, faster-firing proxy at the request layer.

### Fixed

- **`EmbedderSupervisor` could silently stop supervising, and never give up,
  after a wedge-triggered crash whose respawn attempt then failed** (CI
  follow-up, PR [#3560](https://github.com/bobmatnyc/trusty-tools/pull/3560)):
  `supervision_loop`'s top-of-loop check treated an empty `child_slot` as
  unconditional proof of an explicit cooperative shutdown and returned
  immediately. That is only true for the shutdown path — the `Unhealthy`
  (wedge-kill) branch also empties `child_slot` before attempting a respawn,
  and if that respawn itself failed, the next iteration saw the same empty
  slot and silently exited without ever incrementing `consecutive_failures`
  or reaching `should_give_up`/`gave_up_tx`. A daemon could end up with a
  dead sidecar, no supervision, and no fallback signal ever firing. Added
  `RestartTrigger::RespawnFailed` so an empty slot with no shutdown in flight
  is now treated as one more crash-cycle instead of a silent early return.

## [0.23.7] — 2026-07-21

Publishes the `catchup::{pause, generate_catchup_json}` API to crates.io.
This module was merged to main under PR [#3544](https://github.com/bobmatnyc/trusty-tools/pull/3544)
without a corresponding version bump, so it landed in the `0.23.6` source tree
but is absent from the `0.23.6` package already published on crates.io — the
published `0.23.6` predates this module. That mismatch blocked publishing
`trusty-mpm`, whose `core::catchup` re-exports these symbols. `0.23.7` exists
solely to give the catchup API a real, publishable version.

### Added

- **`catchup::generate_catchup_json` + `catchup::pause` module** ([#3543](https://github.com/bobmatnyc/trusty-tools/issues/3543), [#3544](https://github.com/bobmatnyc/trusty-tools/pull/3544)): a structured (JSON) sibling to `generate_catchup_context`'s markdown digest (`CatchupJson`/`PausedSessionJson`/`RecentMemoryJson`), plus a new `pause::write_pause_snapshot` writer that emits the exact session-snapshot section shape `session_finder` already parses, and a `git::capture_git_status` helper (branch/last-commit/uncommitted-summary) for its `## Git Context` section. Backs trusty-mpm's new `session_context_catchup` / `session_context_pause` MCP tools.

## [0.23.6] — 2026-07-20

Release cut of the two embedding-performance fixes below
([#3500](https://github.com/bobmatnyc/trusty-tools/pull/3500),
[#3511](https://github.com/bobmatnyc/trusty-tools/pull/3511); refs #3486 / #3493)
so the shared embedder-inference path (`trusty-embedderd`, `trusty-search`)
picks them up on rebuild.

### Fixed

- **Performance (PR [#3500](https://github.com/bobmatnyc/trusty-tools/pull/3500)):** `FastEmbedder`'s ORT intra-op thread default is now
  platform/execution-provider-conditional instead of an unconditional `1`.
  The `1` pin was introduced by PR #1668 to fix a real CUDA deferred-embed
  deadlock (AL2023 Linux + CUDA EP + dynamically-loaded ORT,
  code-intelligence #1542) but applied to every build, including the
  CoreML/CPU-EP path used on Apple Silicon where no such deadlock exists —
  throttling macOS embedding throughput ~3.5× for no safety benefit (issue
  #3493 P0). `default_ort_intra_threads()` now resolves to `1` only when the
  `embedder-cuda` feature is compiled in (the only build that can register
  the CUDA EP); every other build resolves to
  `std::thread::available_parallelism()`, matching fastembed's own
  per-session default. `TRUSTY_ORT_INTRA_THREADS` remains the operator
  override for either default. `DEFAULT_ORT_INTRA_THREADS` (a constant) is
  replaced by the `default_ort_intra_threads()` function — the only
  consumer was this crate's own resolver, so no downstream crate references
  it.
- **Performance / accuracy (PR [#3511](https://github.com/bobmatnyc/trusty-tools/pull/3511)):** `FastEmbedder`'s default embedding model is now
  `EmbeddingModel::AllMiniLML6V2` (fp32, fastembed's own natively-shipped
  non-quantized variant), replacing the previous default,
  `AllMiniLML6V2Q` (INT8, dynamically quantised). INT8 was measured to be
  both ~2.1× slower (its dequant/requant ops are themselves expensive on the
  CPU EP this model actually runs on — CoreML rejects the INT8 op set
  outright) and less accurate (0.9897 mean cosine similarity vs a genuine
  `sentence-transformers` reference, vs 1.000000 for fp32 — see issues
  #3486 / #3493 P0). INT8 remains available via
  `TRUSTY_EMBEDDER_MODEL=int8` (or `quantized` / `q`) for operators who need
  the smaller ~23MB on-disk footprint (fp32 is ~87MB) more than speed or
  accuracy; the existing two-model fallback-on-init-failure safety net is
  preserved, just parameterised by the resolved default instead of
  hardcoded. `EMBED_DIM`, `RequireStaticInputShapes`, and all existing batch
  caps are unchanged.

## [0.23.5] — 2026-07-20

### Added

- `BM25Index::score_query_all_with_filter` — same ranking as `score_query_all`,
  but evaluates a caller-supplied `Fn(&str) -> bool` predicate on each
  candidate `doc_id` BEFORE the internal `top_k` truncation, not after
  (trusty-search issue #3401: a scope filter applied only on the already-
  truncated result set can silently drop a genuinely matching, lexically
  relevant document). Purely additive — `score_query_all`'s signature and
  behaviour are unchanged, so existing callers (`trusty-bm25-daemon`) are
  unaffected.

## [0.23.4] — 2026-07-19

### Added

- new `banner` module — `TRUSTY_SPLASH_ART` (the compact block-robot "TRUSTY" wordmark art) and `shade_bucket` (glyph → amber/rust RGB triple), extracted from `tm`'s previously binary-local splash renderer so both `tm` and `trusty-agents`' REPL can render the identical trusty branding without drifting apart again (closes [#3326](https://github.com/bobmatnyc/trusty-tools/issues/3326)). Zero extra dependencies — pure `&str` + `match`.
- register a `local`/OpenAI-compatible inference provider (Ollama by default, `http://localhost:11434/v1`) in the unified inference registry — no external credentials required (`credential_env: None`, Bedrock precedent); base URL overridable via `OLLAMA_HOST`, optional bearer credential via `TRUSTY_LOCAL_API_KEY`; slug prefixes `local/` and `ollama/` both resolve to it (closes [#3247](https://github.com/bobmatnyc/trusty-tools/issues/3247))
- add a `claude-code` → `CLAUDE_CODE_OAUTH_TOKEN` mapping to `inference::credentials::env_var_for`, so trusty-agents' `claude` CLI OAuth routing can resolve through the shared 3-tier credential resolver (part of [#3248](https://github.com/bobmatnyc/trusty-tools/issues/3248))
- **Security:** shared same-origin (CSRF) write guard behind the `axum-server`
  feature — `server::origin_guard` (`SelfOrigins`, `guard_write_origin`,
  `origin_is_loopback`, `origin_matches_self`) lifted verbatim from
  trusty-console's proven implementation (#3268/#3269/#3280), plus
  `server::with_guarded_middleware` which composes it router-wide into the
  standard middleware stack. Lets every trusty-* daemon adopt the guard with a
  one-line change instead of re-implementing it (architecture review tranche 1,
  [#3304](https://github.com/bobmatnyc/trusty-tools/issues/3304)).
- surface index readiness + working-context budget as events (UI Phase-1) ([#2861](https://github.com/bobmatnyc/trusty-tools/pull/2861)) ([`c5d75fc`](https://github.com/bobmatnyc/trusty-tools/commit/c5d75fc86259ac370a07504efd34671c70db9de7))
- adopt DOC-38 policy + sld-lint gate (closes #2853, #2854) ([#2863](https://github.com/bobmatnyc/trusty-tools/pull/2863)) ([`580c9a7`](https://github.com/bobmatnyc/trusty-tools/commit/580c9a7d08e873d9706c6b05cfe83eafb2befbfa))

### Fixed

- EmbedderSupervisor shutdown reachable + no respawn on intentional shutdown ([#3023](https://github.com/bobmatnyc/trusty-tools/pull/3023)) ([`dd5f212`](https://github.com/bobmatnyc/trusty-tools/commit/dd5f212900abff69573121e826028e941188b79a))
- embedder reader-death detection + wedged-sidecar restart ([#2978](https://github.com/bobmatnyc/trusty-tools/pull/2978)) ([`25c56d0`](https://github.com/bobmatnyc/trusty-tools/commit/25c56d0564a281e42719cdd0ea18f03099c47749))
- validate consolidation model at startup, fail loud once instead of per-cycle retry ([#2977](https://github.com/bobmatnyc/trusty-tools/pull/2977)) ([`afcbfce`](https://github.com/bobmatnyc/trusty-tools/commit/afcbfce4638e68d660c2ce21b926051da645b2ff))

### Changed

- single shared tmux library; route trusty-mpm + trusty-agents through it ([#3017](https://github.com/bobmatnyc/trusty-tools/pull/3017)) ([`383b9f4`](https://github.com/bobmatnyc/trusty-tools/commit/383b9f475e781ef6049900f1630875e8ebf68264))
- `sld` module (behind the new lightweight `sld` feature — `regex` + `serde_yaml` + `thiserror`, deliberately NOT the heavy `intent-source`/`tickets` stack): the language-agnostic **Spec-Linked Documentation (DOC-38)** reference grammar — the `SPEC-{SUBSYSTEM}-{NN}~{rev}` id grammar + §2.2 reference regex, the per-extension comment-syntax table, fenced-code-aware inline `# Spec References` block parsing, `spec_refs:` YAML-frontmatter parsing, and `{#SPEC-…}` heading-anchor scanning/resolution. Consolidates `revision_of`/`base_id` here as the single source both this grammar and `intent_source::spec_resolve` share (the `intent-source` feature now enables `sld`), so the new `trusty-sld-lint` gate and the ISR parse ONE grammar (DOC-38 §10 F1) ([#2854](https://github.com/bobmatnyc/trusty-tools/issues/2854))
### Changed

- re-cut to escape crates.io collision with PR #2209's source-deficient 0.22.0; carries PR #2221's chat-session/consolidation hardening (#1712/#1713/#1714)

### Fixed

- auto-fall back to CPU when CoreML embedder init hangs; stop leaking blocked ORT threads ([#2127](https://github.com/bobmatnyc/trusty-tools/pull/2127)) ([`f7dc2dd`](https://github.com/bobmatnyc/trusty-tools/commit/f7dc2dd20524ee9d1a9c6146245aaacc5d1e7b2b))
- reach trusty-memory over discovered JSON-RPC, never a hardcoded port ([#2040](https://github.com/bobmatnyc/trusty-tools/pull/2040)) ([`e0f41c5`](https://github.com/bobmatnyc/trusty-tools/commit/e0f41c51f1baa7ddf0e427cb5c7e86cbe9bba5fa))
- verify_installed_binary checks ~/.local/bin and $CARGO_HOME ([#2042](https://github.com/bobmatnyc/trusty-tools/pull/2042)) ([`e0d2c7b`](https://github.com/bobmatnyc/trusty-tools/commit/e0d2c7bc8dc2c06cd6a004b777454dd129dc7b5b))
## [0.19.0] — 2026-07-03

### Added

- unify session start with protected-path routing, rename sessions->session, bare tm shortcut (closes #1916) ([#1920](https://github.com/bobmatnyc/trusty-tools/pull/1920)) ([`0f40c01`](https://github.com/bobmatnyc/trusty-tools/commit/0f40c01085d15d6ec5f7f2424593640ad11da23e))
- wire trusty-mpm into console reverse proxy ([#1850](https://github.com/bobmatnyc/trusty-tools/pull/1850)) ([`970d297`](https://github.com/bobmatnyc/trusty-tools/commit/970d297bf9448cf74b3117445401524bd17b20e4))
- detach returns to tm picker + daemon/clone cwd hardening ([#1795](https://github.com/bobmatnyc/trusty-tools/pull/1795)) ([`3b0e723`](https://github.com/bobmatnyc/trusty-tools/commit/3b0e7231e85ca8fbc53dbd55bb4968d4d96e811c))

### Fixed

- decouple recall/remember from embedder warm-up (closes #1970) ([#1972](https://github.com/bobmatnyc/trusty-tools/pull/1972)) ([`bb322d4`](https://github.com/bobmatnyc/trusty-tools/commit/bb322d4678f8e167691688e77190b44d9c08627a))
- palace-level alias resolution for claude-mpm parity (owner-repo -> bare palace) ([#1945](https://github.com/bobmatnyc/trusty-tools/pull/1945)) ([`af7f904`](https://github.com/bobmatnyc/trusty-tools/commit/af7f90499402971ac65aed5b104cde251e182599))
- warn on skipped malformed claude-mpm session in catchup ([#1762](https://github.com/bobmatnyc/trusty-tools/pull/1762)) ([#1769](https://github.com/bobmatnyc/trusty-tools/pull/1769)) ([`e0b2e7c`](https://github.com/bobmatnyc/trusty-tools/commit/e0b2e7c47cc426d5dd19df37c08d54b53bd436e3))

### Changed

- extract DOC-28 catch-up engine behind catchup feature (PR1, #1762) ([`addfdbb`](https://github.com/bobmatnyc/trusty-tools/commit/addfdbb04ed78028887a0e782afe7cfe83c10b46))
## [0.18.0] — 2026-06-25

### Added

- `DrawerType::Task` variant (index 5) — privileged drawer type that is exempt from
  dream-cycle eviction and semantic consolidation while `completed_at` is `None` (closes #1722)
- `Drawer::completed_at: Option<DateTime<Utc>>` field — setting this re-enables cleanup
  for Task drawers after work is finished (closes #1722)
- Serialization-safety guarantee for `DrawerType` postcard indices; backward-compat test
  asserts every variant encodes to its expected byte index (closes #1722)
- Task drawer protection end-to-end: `DrawerType::Task.is_protected()` is exercised by
  `task_drawer_survives_dream_cycle` integration test via the `task_add`/`task_list`/
  `task_complete` MCP tools (refs #1722)
- `DrawerType::Task serialization safety — fix index order and add backward-compat test (closes #1722) ([`4646c3e`](https://github.com/bobmatnyc/trusty-tools/commit/4646c3e535a1e1b67aae33ec429f7f0c860e3aca))
- chat session manager MVP — force palaces, chat-session MCP tools, room-scoped consolidation, Task drawers (closes #1700, #1701, #1702, #1703) ([#1710](https://github.com/bobmatnyc/trusty-tools/pull/1710)) ([`dcb31f7`](https://github.com/bobmatnyc/trusty-tools/commit/dcb31f7e6743dda227e79cb8d8a7116440868d10))
- pin trusty-memory palace slug in managed-session MCP injection (closes #1605) ([#1652](https://github.com/bobmatnyc/trusty-tools/pull/1652)) ([`d15c96d`](https://github.com/bobmatnyc/trusty-tools/commit/d15c96dc846e805f2ddf6549d157d2719afd4e9a))

### Fixed

- accept key=value secret-filter tokens with slash-path values (closes #1676) ([#1678](https://github.com/bobmatnyc/trusty-tools/pull/1678)) ([`b236744`](https://github.com/bobmatnyc/trusty-tools/commit/b236744ad5e4ca931f777815fb4ff41e3a6d7b7b))
- stop secret filter false-flagging path/slug technical tokens (closes #1667) ([#1669](https://github.com/bobmatnyc/trusty-tools/pull/1669)) ([`16b5eee`](https://github.com/bobmatnyc/trusty-tools/commit/16b5eeea015e143ccbeb05f9f0c9fe4224d625c6))
- pin ORT intra-op to 1 + disable spinning to break CUDA deferred-embed deadlock ([#1668](https://github.com/bobmatnyc/trusty-tools/pull/1668)) ([`1b65d16`](https://github.com/bobmatnyc/trusty-tools/commit/1b65d16e94f4a4e6020af194c87a9a4a8d45428b))

### Documentation

- correct stale SQLite references to redb in comments and README ([#1704](https://github.com/bobmatnyc/trusty-tools/pull/1704)) ([`63645b3`](https://github.com/bobmatnyc/trusty-tools/commit/63645b3d3028940299dd6f9a4b09310ac5ee5f00))
# Changelog — trusty-common

## [0.17.0] — 2026-06-17

### Added (refs #1373)

- **`index_id` module: `derive_index_id` + `resolve_project_root`.** The single
  source of truth for deriving a trusty-search index id from a project path
  (the path basename, preserved verbatim for backward-compatibility) and for
  walking up to the git root. Both trusty-search (`detect_project`, serve pin)
  and trusty-mpm (register-and-pin at session launch) call these so they cannot
  drift. Re-exported at the crate root as `trusty_common::derive_index_id` and
  `trusty_common::resolve_project_root`.

## [0.16.0] — 2026-06-16

### Changed (refs #1361, PR #1371)

- **SLD spec-resolver hardening (C4).** Strengthened the Spec-Linked
  Documentation spec resolver so traceability survives realistic doc drift:
  - **Block-scoped `# Spec References` parsing** — references are now collected
    from a delimited block rather than scanned line-by-line across the whole
    file, so stray matches outside the references block no longer pollute the
    resolved set.
  - **Revision-tolerant section matching** — section lookups tolerate revision
    suffixes / minor heading reformatting, so a spec section still resolves when
    its heading has been lightly edited.
  - **Drift flagging** — references that no longer resolve to a live spec section
    are surfaced as drift rather than silently dropped, giving CI a signal to act
    on instead of a false pass.

## [0.15.3] — 2026-06-16

### Changed (closes #1326)

- **`StdioEmbedderClient` reader: down-level benign `timed_out_id=None` timeout to `debug!`** — when the timeout fires with no in-flight request (empty pending map, a periodic idle re-arm while the embedder is healthy), the log is now `debug!` instead of `warn!`. The `warn!` path is preserved for `timed_out_id=Some(id)`, where a real in-flight request actually stalled. Eliminates ~2,800 spurious WARN lines/day during normal operation.

## [0.14.0] — 2026-06-04

### Changed (closes #753)

- **`StdioEmbedderClient` rewritten as multi-flight pipelined client** — the
  previous implementation held a single `Mutex` for the entire
  write→wait→read round-trip, allowing only one batch in flight at a time.
  The new implementation splits into: (1) a write-only stdin `Mutex` held
  only for `write_all + flush`, (2) a dedicated reader task that owns stdout
  and dispatches responses via a FIFO `VecDeque<oneshot::Sender>` (no id
  lookup needed — the sidecar never re-orders responses), and (3) an
  `inflight` semaphore capping concurrent requests at `TRUSTY_EMBED_INFLIGHT`
  (default 2, max 4). Crash/restart: EOF or IO error drains all pending
  oneshots with an error so callers return immediately.
- New env var: `TRUSTY_EMBED_INFLIGHT` — controls the semaphore depth.

## [0.13.0] — 2026-06-04

### Added (closes #747 Fix C)

- **`sidecar_batch_size` helper + `SupervisorConfig::sidecar_batch_size` field** —
  `EmbedderSupervisor` now accepts an optional resolved ONNX batch size in its
  config and forwards it as `TRUSTY_EMBED_BATCH_SIZE` to the `trusty-embedderd`
  child process at spawn time (and on crash-restart). Previously the sidecar
  always defaulted to 32 chunks per ONNX call regardless of what the parent
  daemon had computed via memory-tier autosizing, leaving significant throughput
  on the table (e.g. a Medium-tier host with CoreML computed 256 while the sidecar
  ran at 32). A CoreML memory-safety cap (`min(resolved, coreml_cap)`) is applied
  to prevent oversized unified-memory tensor allocations from triggering macOS
  jetsam SIGKILL.

## [0.12.0] — 2026-06-03

### Changed

- **redb 2.6 → 4.1 upgrade** (#702) — all stores upgraded to redb 4.x API.
  Graceful old-format recovery at every store open: existing `.redb` files
  written by redb 2.x are detected as incompatible, backed up to
  `*.v2-incompatible`, and recreated automatically. No manual intervention
  required.

- **Memory recall ranked by similarity score** (#633) — recall results are
  now sorted by embedding similarity score (descending) rather than insertion
  order, surfacing the most relevant memories first.

> **OPERATOR NOTE:** Existing palace `.redb` files are detected as incompatible
> on first open, backed up to `*.v2-incompatible`, and recreated empty.
> Re-populating palace data requires re-importing or re-creating memories.

## [0.11.1] — 2026-06-02

### Fixed

- **CUDA arena VRAM OOM prevention (issue #600)** — `embedder-cuda` builds now
  configure ORT's BFCArena with `arena_extend_strategy = kSameAsRequested` and an
  explicit `gpu_mem_limit` (default 12 GiB, tunable via `TRUSTY_GPU_MEM_LIMIT_BYTES`
  / `TRUSTY_GPU_MEM_LIMIT_MB`) so the arena no longer grows by `kNextPowerOfTwo`
  and over-reserves device VRAM. Eliminates the OOM failure on 16 GB Tesla T4 GPUs
  without requiring the `TRUSTY_MAX_BATCH_SIZE=32` workaround.

- **Accurate `/health` provider reporting (issue #604)** — the `provider` field in
  `/health` responses now reflects the actual ORT execution provider in use (e.g.
  `CUDA`, `CoreML`, `CPU`) rather than always reporting `CPU`.

## [0.5.0] — 2026-05-26

### Added

- **`UdsEmbedderClient`** in `trusty_common::embedder_client` — a new third impl
  of the `EmbedderClient` trait that communicates with `trusty-embedderd` over a
  Unix Domain Socket using newline-framed JSON-RPC 2.0 (issue #164, Step A).
  Provides sub-millisecond in-host embedding without TCP overhead. Re-exported
  as `pub use uds::UdsEmbedderClient` from the module root.

- **`EmbedderError::Uds(String)`** variant — added to cover UDS transport
  failures (connect refused, broken pipe, decode error) distinctly from the
  existing `Transport(reqwest::Error)` HTTP variant.

### Breaking changes

- **`embed-client` feature removed** — the `embed-client` feature flag (and
  the underlying `trusty_common::embed_client` module) that provided the old
  `EmbedClient` UDS-only struct have been deleted (issue #164, Step C). The
  retired `trusty-embed-daemon` binary (PR #157) is also deleted. **Migration**:
  replace `trusty_common::embed_client::EmbedClient` with
  `trusty_common::embedder_client::UdsEmbedderClient`. The wire protocol is
  identical; the main difference is that `UdsEmbedderClient::embed_batch` now
  implements the `EmbedderClient` trait and returns `EmbedderError` instead of
  `anyhow::Error`.

### Changed

- Updated `embedder_client` module doc-comment to reflect the three-impl unified
  surface (InProcess, HTTP, UDS). Removed the "Issue #164 will reconcile" note.

## [0.4.23] — 2026-05-26

### Added

- **`embedder-client` feature** — moves the former `trusty-embedder-client` crate
  (issue #110 Phase 1) into `trusty-common` as a feature-gated module
  `trusty_common::embedder_client`. Reduces workspace crate count by one and aligns
  the client library under Elastic-2.0 licensing to match the rest of the
  trusty-* ecosystem (the originating PR #163 shipped it as MIT temporarily).

  The new module exposes:
  - `EmbedderClient` trait (async `embed_batch`)
  - `InProcessEmbedderClient` (wraps `FastEmbedder` for zero-config backwards compat)
  - `RemoteEmbedderClient` (HTTP JSON client for a running `trusty-embedderd`)
  - `EmbedRequest` / `EmbedResponse` wire types
  - `EmbedderError` (`thiserror`-derived)

  The module name is `embedder_client` (with `er`) to distinguish from the
  existing `embed_client` (UDS, PR #157). Issue #164 will reconcile the two
  embed-client modules into a unified interface.

  Enable with:
  ```toml
  trusty-common = { version = "0.4.23", features = ["embedder-client"] }
  ```
  Note: `embedder-client` implies `embedder` (and `embedder-bundled-ort` by
  extension of the embedder feature chain) because `InProcessEmbedderClient`
  wraps `FastEmbedder`. Callers that only need the remote HTTP path and wish
  to skip fastembed/ORT compilation are served by `embed-client` (UDS, #157).
  Issue #164 will provide a unified single-feature entry point.

### Changed

- No existing APIs modified. All changes are additive behind the new feature flag.
