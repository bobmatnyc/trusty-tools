//! Resume-from-checkpoint for an interrupted reindex (issue #3979).
//!
//! Why: before this module, a SIGKILL / OOM-kill / power loss partway through a
//! reindex discarded every batch the run had already committed. The batches were
//! durable — they live in the staging corpus `index.redb.tmp` (#603) — but the
//! next reindex called `CorpusStore::open_fresh` on that exact path, which
//! *deletes the file first*. On the production fleet's 200k-chunk indexes that
//! threw away hours of embedding on every interruption.
//!
//! What: a small, self-describing record written into the staging corpus's own
//! `_meta` table naming the run that is building it. On the next reindex, the
//! runner reads that record back and either adopts the staging corpus (resume)
//! or discards it and rebuilds from scratch (the pre-#3979 behaviour). Every
//! ambiguous case resolves to "discard", never to "adopt and hope".
//!
//! # Why this is safe — the two load-bearing properties
//!
//! 1. **The resume unit is a file, and the skip is content-verified.**
//!    Adopting the staging corpus also adopts its `file_hashes` table, which is
//!    the done-set. But `batch::prepare_batch_payload` does not skip a file
//!    because it appears in that set — it re-reads the file, re-hashes the
//!    CURRENT bytes, and skips only on an exact SHA-256 match. A file edited
//!    after the crash therefore cannot be skipped, and a file deleted after the
//!    crash is removed by the ordinary prune pass. Corpus drift under a
//!    checkpoint is handled per-file rather than by invalidating everything.
//!
//! 2. **A staged hash implies its staged chunks.** `apply_successful_commit`
//!    persists a batch's hashes only *after* `commit_parsed_batch` returned
//!    `Ok`, in a later redb transaction. So a crash can leave chunks without
//!    their hash (that file is simply re-indexed on resume — wasted work, never
//!    wrong data) but never a hash without its chunks. The done-set can only
//!    under-claim. (The one pre-existing exception is the #1002 path, where a
//!    file whose pre-commit remove failed is filtered out of the batch while
//!    its hash is still persisted; that behaves identically on a clean run, so
//!    it is not new drift introduced by resume.)
//!
//! # Deliberate non-goals
//!
//! - **`force` reindexes never checkpoint and never resume.** A force run stages
//!   an EMPTY corpus and skips the prune pass entirely, so a resumed force run
//!   could keep chunks for files deleted between the crash and the resume —
//!   drift a clean force run would not have. Fixing that means giving `force` a
//!   prune pass, which is a behaviour change to `force` and out of scope here.
//! - **The staged HNSW snapshot (#3970) is not adopted.** It is written by the
//!   periodic persister on its own schedule, so it is not transactionally tied
//!   to the staged corpus. Instead the runner guarantees a vector catch-up pass
//!   after any resumed run, which embeds exactly the chunks the live snapshot is
//!   missing (see `finish::finish_reindex`).
//!
//! Test: `tests` below covers the pure decision function and the fingerprint;
//! `super::resume_tests` covers the end-to-end interrupt/resume equivalence and
//! both fallback paths.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::registry::{IndexHandle, IndexId};

/// On-disk format version for [`ReindexCheckpoint`].
///
/// Why: a future change to what "done" means (a different chunker, a different
/// hash, extra fields whose absence changes the decision) must not be readable
/// as if it were this format. Bumping this constant makes every older record
/// fall back to a full reindex instead of being silently misread.
/// What: compared for exact equality — neither older nor newer records are
/// accepted. `serde` would happily deserialize a v2 record into this struct by
/// ignoring unknown fields, so the version check, not the parse, is the gate.
/// Test: `stale_schema_version_is_discarded`,
/// `checkpoints_written_by_the_shipped_v1_format_are_discarded`.
// #4721: bumped 1 → 2 because [`config_fingerprint`] changed its encoding. A
// record written by trusty-search 0.42.0 carries a fingerprint computed under
// the old, ambiguous framing; the bump makes every such record fail the version
// gate outright rather than relying on the fingerprint comparison to notice.
pub(super) const CHECKPOINT_SCHEMA_VERSION: u32 = 2;

/// Default maximum age of an adoptable checkpoint, in seconds (24 h).
///
/// Why: correctness does not depend on this — the per-file hash re-verification
/// above makes an old checkpoint as safe as a fresh one. It is hygiene: the
/// older a staging file is, the more likely it is an orphan from a daemon that
/// died days ago against a corpus that has since moved on, in which case
/// re-walking is cheap relative to the confusion of adopting it.
/// What: overridable via `TRUSTY_REINDEX_CHECKPOINT_MAX_AGE_SECS`; `0` disables
/// the age gate entirely.
/// Test: `stale_by_age_is_discarded`.
const DEFAULT_CHECKPOINT_MAX_AGE_SECS: u64 = 86_400;

/// Durable description of the reindex run that is building a staging corpus.
///
/// Why: resume must be a decision made from evidence, not from the mere
/// existence of an `index.redb.tmp`. Every field here is something that, if it
/// changed, would make the partial corpus non-equivalent to what the current run
/// would produce — so every field is compared and any mismatch discards.
/// What: serialized to JSON and stored under `_meta[reindex_checkpoint]` inside
/// the staging corpus itself.
/// Test: `checkpoint_roundtrips_through_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ReindexCheckpoint {
    /// Format version — see [`CHECKPOINT_SCHEMA_VERSION`].
    pub(super) schema_version: u32,
    /// Index this staging corpus belongs to.
    pub(super) index_id: String,
    /// Canonical walk root the staged chunk paths are relative to. A mismatch
    /// means the staged relative paths would resolve against a different tree —
    /// the #2178 hazard — so it discards.
    pub(super) canonical_root: String,
    /// Crate version of the daemon that wrote the record. A different build may
    /// chunk, hash, or embed differently, so a version change discards rather
    /// than trying to enumerate which changes matter.
    pub(super) crate_version: String,
    /// Fingerprint of the index config that decides which files are walked and
    /// how they are parsed — see [`config_fingerprint`].
    pub(super) config_fingerprint: String,
    /// Unix seconds at which the interrupted run began staging.
    pub(super) created_at_unix: u64,
}

impl ReindexCheckpoint {
    /// Build the checkpoint describing the run that is about to stage `handle`.
    ///
    /// Why: the same constructor produces both the record written at stage time
    /// and the "expected" record compared against a found one, so the two can
    /// never drift apart field-by-field.
    /// What: stamps the current schema version, crate version, wall clock, and
    /// the config fingerprint of `handle`.
    /// Test: `matching_checkpoint_resumes`.
    pub(super) fn for_run(handle: &IndexHandle, index_id: &IndexId, canonical_root: &Path) -> Self {
        Self {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            index_id: index_id.0.clone(),
            canonical_root: canonical_root.to_string_lossy().into_owned(),
            crate_version: env!("CARGO_PKG_VERSION").to_string(),
            config_fingerprint: config_fingerprint(handle),
            created_at_unix: now_unix(),
        }
    }
}

/// Outcome of comparing a found checkpoint against the current run.
///
/// Why: modelling the decision as a two-variant enum with a mandatory reason on
/// the discard arm makes it impossible to discard silently — every fallback to a
/// full reindex is explainable in the log.
/// What: `Resume` adopts the staging corpus; `Discard` rebuilds from scratch.
/// Test: every branch is covered in `tests` below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResumeDecision {
    /// The staging corpus is equivalent to what this run would have built so
    /// far — adopt it.
    Resume,
    /// The staging corpus cannot be proven equivalent — discard and rebuild.
    Discard { reason: String },
}

/// Decide whether `found` may be adopted by the run described by `current`.
///
/// Why: this is the whole safety argument in one pure function, so it can be
/// exhaustively unit-tested without a daemon, a filesystem, or a redb file.
/// Anything it cannot positively prove equivalent must discard — the cost of a
/// wrong `Discard` is a slow rebuild, the cost of a wrong `Resume` is a silently
/// incorrect index.
/// What: compares schema version, index id, canonical root, crate version, and
/// config fingerprint for exact equality, then applies the age gate. Returns
/// `Resume` only when every check passes.
/// Test: `matching_checkpoint_resumes`, `stale_schema_version_is_discarded`,
/// `foreign_index_id_is_discarded`, `moved_root_is_discarded`,
/// `changed_crate_version_is_discarded`, `changed_config_is_discarded`,
/// `stale_by_age_is_discarded`, `zero_max_age_disables_age_gate`.
pub(super) fn evaluate(
    found: &ReindexCheckpoint,
    current: &ReindexCheckpoint,
    max_age_secs: u64,
) -> ResumeDecision {
    let discard = |reason: String| ResumeDecision::Discard { reason };

    if found.schema_version != CHECKPOINT_SCHEMA_VERSION {
        return discard(format!(
            "checkpoint schema version {} != supported {CHECKPOINT_SCHEMA_VERSION}",
            found.schema_version,
        ));
    }
    if found.index_id != current.index_id {
        return discard(format!(
            "checkpoint belongs to index '{}', not '{}'",
            found.index_id, current.index_id,
        ));
    }
    if found.canonical_root != current.canonical_root {
        return discard(format!(
            "checkpoint root '{}' != current walk root '{}' — staged chunk paths \
             are relative to a different tree",
            found.canonical_root, current.canonical_root,
        ));
    }
    if found.crate_version != current.crate_version {
        return discard(format!(
            "checkpoint written by trusty-search {} but this daemon is {}",
            found.crate_version, current.crate_version,
        ));
    }
    if found.config_fingerprint != current.config_fingerprint {
        return discard(
            "index config changed since the checkpoint was written (walk filters, \
             extensions, or pipeline flags)"
                .to_string(),
        );
    }
    if max_age_secs > 0 {
        let age = now_unix().saturating_sub(found.created_at_unix);
        if age > max_age_secs {
            return discard(format!(
                "checkpoint is {age}s old, past the {max_age_secs}s adoption window"
            ));
        }
    }
    ResumeDecision::Resume
}

/// Stable fingerprint of every `IndexHandle` field that changes what a reindex
/// produces.
///
/// Why: adopting a partial corpus is only equivalent if the current run would
/// have walked the same files and parsed them the same way. Rather than trying
/// to reason about which config changes are benign, hash all of them and discard
/// on any difference.
/// What: SHA-256 over a LENGTH-PREFIXED rendering of the walk filters
/// (`include_paths`, `exclude_globs`, `extensions`, `path_filter`,
/// `extra_skip_dirs`, `include_docs`, `respect_gitignore`, `follow_links`,
/// `data_file_max_bytes`) and the pipeline flags that decide which stages run
/// (`lexical_only`, `skip_kg`, `skip_vector`, `defer_embed`). Collection fields
/// are sorted so a pure reordering in `indexes.toml` does not force a rebuild.
///
/// # Why length-prefixed and not a separator (#4721)
///
/// The original encoding was `name=value;` with list elements joined by `,`,
/// which is not injective: `exclude_globs = ["a", "b"]` and
/// `exclude_globs = ["a,b"]` both rendered `exclude_globs=a,b;` and therefore
/// hashed identically, and a value containing `;name=` could forge a field
/// boundary outright. Two distinct configurations sharing a fingerprint means a
/// checkpoint is accepted as valid for a configuration it was not built under —
/// precisely the outcome the record exists to prevent.
///
/// No separator byte is safe here. Globs, extensions, and skip-dir names arrive
/// from `indexes.toml`, so they are arbitrary Rust `String`s — every candidate
/// delimiter (`,`, `;`, `=`, `\n`, even `\0`) is a legal character inside one.
/// Paths are worse still: on POSIX only `/` and NUL are reserved, and `/` is
/// already meaningful inside the value. Length prefixing sidesteps the question
/// entirely — `len ‖ bytes` is injective for arbitrary byte strings, so no value
/// can impersonate a boundary regardless of its content. Paths are absorbed as
/// their raw `OsStr` bytes rather than `to_string_lossy`, which maps every
/// invalid sequence to the same `U+FFFD` and is therefore lossy in exactly the
/// direction that creates collisions.
///
/// Test: `config_fingerprint_is_order_insensitive`,
/// `config_fingerprint_changes_with_extensions`,
/// `config_fingerprint_distinguishes_list_element_boundaries`,
/// `config_fingerprint_cannot_be_forged_across_field_boundaries`.
pub(super) fn config_fingerprint(handle: &IndexHandle) -> String {
    let mut hasher = Sha256::new();
    // Domain-separation tag: a fingerprint is only ever compared against another
    // fingerprint produced by this same encoding version.
    absorb(&mut hasher, b"trusty-search/reindex-checkpoint/config/v2");

    absorb_paths(&mut hasher, "include_paths", &handle.include_paths);
    absorb_list(&mut hasher, "exclude_globs", &handle.exclude_globs);
    absorb_list(&mut hasher, "extensions", &handle.extensions);
    absorb_list(&mut hasher, "path_filter", &handle.path_filter);
    absorb_list(&mut hasher, "extra_skip_dirs", &handle.extra_skip_dirs);
    absorb_scalar(&mut hasher, "include_docs", &handle.include_docs);
    absorb_scalar(&mut hasher, "respect_gitignore", &handle.respect_gitignore);
    absorb_scalar(&mut hasher, "follow_links", &handle.follow_links);
    absorb_scalar(
        &mut hasher,
        "data_file_max_bytes",
        &handle.data_file_max_bytes,
    );
    absorb_scalar(&mut hasher, "lexical_only", &handle.lexical_only);
    absorb_scalar(&mut hasher, "skip_kg", &handle.skip_kg);
    absorb_scalar(&mut hasher, "skip_vector", &handle.skip_vector);
    absorb_scalar(&mut hasher, "defer_embed", &handle.defer_embed);

    format!("{:x}", hasher.finalize())
}

/// Absorb one length-prefixed byte string into the fingerprint (#4721).
///
/// Why: the single primitive the injectivity argument in [`config_fingerprint`]
/// rests on — every value is preceded by its own length, so no value's content
/// can be mistaken for framing.
/// What: an 8-byte little-endian length followed by the bytes themselves.
/// Test: `config_fingerprint_distinguishes_list_element_boundaries`.
fn absorb(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// Absorb a `name` → scalar pair, rendering the scalar via `Display` (#4721).
fn absorb_scalar(hasher: &mut Sha256, name: &str, value: &impl std::fmt::Display) {
    absorb(hasher, name.as_bytes());
    absorb(hasher, value.to_string().as_bytes());
}

/// Absorb a `name` → sorted string-list pair (#4721).
///
/// Why: the element COUNT is absorbed alongside the elements, so a list cannot
/// be confused with a shorter list whose trailing elements were absorbed as part
/// of the following field.
/// What: sorts a copy (order in `indexes.toml` is not meaningful), then absorbs
/// the count followed by each element length-prefixed.
/// Test: `config_fingerprint_distinguishes_list_element_boundaries`.
fn absorb_list(hasher: &mut Sha256, name: &str, items: &[String]) {
    absorb(hasher, name.as_bytes());
    let mut sorted: Vec<&str> = items.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    hasher.update((sorted.len() as u64).to_le_bytes());
    for item in sorted {
        absorb(hasher, item.as_bytes());
    }
}

/// Absorb a `name` → sorted path-list pair, byte-exactly (#4721).
///
/// Why: `to_string_lossy` collapses every invalid UTF-8 sequence to `U+FFFD`, so
/// two genuinely different roots could fingerprint identically.
/// `OsStr::as_encoded_bytes` is lossless and portable.
/// Test: `config_fingerprint_is_order_insensitive`.
fn absorb_paths(hasher: &mut Sha256, name: &str, items: &[PathBuf]) {
    absorb(hasher, name.as_bytes());
    let mut sorted: Vec<&[u8]> = items
        .iter()
        .map(|p| p.as_os_str().as_encoded_bytes())
        .collect();
    sorted.sort_unstable();
    hasher.update((sorted.len() as u64).to_le_bytes());
    for item in sorted {
        absorb(hasher, item);
    }
}

/// Whether resume-from-checkpoint is enabled for this daemon.
///
/// Why: this path decides whether partial work is adopted, so an operator who
/// suspects it needs a one-flag kill switch that forces the pre-#3979
/// always-rebuild behaviour without a downgrade.
/// What: reads `TRUSTY_REINDEX_RESUME` and defers the decision to
/// [`resume_enabled_from`]. Defaults to enabled — the whole point of the feature
/// is that the fleet gets it without configuration.
/// Test: `resume_kill_switch_disables_adoption` (end to end, in an isolated
/// child process); the parsing itself is covered by `resume_enabled_from_parses`.
pub(super) fn resume_enabled() -> bool {
    resume_enabled_from(std::env::var("TRUSTY_REINDEX_RESUME").ok().as_deref())
}

/// Pure decision for [`resume_enabled`] — no environment access (#4721).
///
/// Why: the env read and the parse used to be one function, so the only way to
/// test the parse was to mutate a process-global variable from a test. Splitting
/// them means the interesting half is exhaustively testable with no globals, no
/// `unsafe`, and no cross-test interference at all.
/// What: `false` only for `0`, `false`, `no`, or `off` (trimmed,
/// case-insensitive); `true` for anything else, including `None`.
/// Test: `resume_enabled_from_parses`.
pub(super) fn resume_enabled_from(value: Option<&str>) -> bool {
    match value {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        None => true,
    }
}

/// Adoption window for a found checkpoint, in seconds.
///
/// Why: see [`DEFAULT_CHECKPOINT_MAX_AGE_SECS`].
/// What: `TRUSTY_REINDEX_CHECKPOINT_MAX_AGE_SECS` when parseable, else the
/// default. `0` disables the age gate.
/// Test: `stale_by_age_is_discarded`, `zero_max_age_disables_age_gate`,
/// `checkpoint_max_age_from_parses`.
pub(super) fn checkpoint_max_age_secs() -> u64 {
    checkpoint_max_age_from(
        std::env::var("TRUSTY_REINDEX_CHECKPOINT_MAX_AGE_SECS")
            .ok()
            .as_deref(),
    )
}

/// Pure parse for [`checkpoint_max_age_secs`] — no environment access (#4721).
///
/// Why: same reason as [`resume_enabled_from`] — testing the parse must not
/// require mutating a process-global variable shared with every other test in
/// the binary.
/// What: the parsed seconds when the value is a valid `u64`, else the default. A
/// garbage value therefore keeps the gate rather than silently disabling it.
/// Test: `checkpoint_max_age_from_parses`.
pub(super) fn checkpoint_max_age_from(value: Option<&str>) -> u64 {
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_CHECKPOINT_MAX_AGE_SECS)
}

/// A staging corpus that passed every adoption check, together with the open
/// handle the probe used to check it.
///
/// Why: carries the three things the runner needs from the probe — which file to
/// adopt, the done-set to seed the in-process hash cache with, and (#4721) the
/// already-open `CorpusStore` itself. The store used to be dropped at the end of
/// the probe and re-opened moments later by `adopt_staged_corpus`, which was
/// both wasted work on a multi-GB redb file and an unguarded window: redb takes
/// an exclusive lock per file, so between the two opens the staging corpus was
/// owned by nobody and the second open could fail (`DatabaseAlreadyOpen`, a
/// removed file, a wedged open gate) — and that failure path silently gives up
/// the resume and writes to the live corpus instead. Handing the handle over
/// makes the file open exactly once, from validation through adoption.
///
/// What: the staging path, the open store, its persisted `(relative_path,
/// sha256)` pairs, and its chunk count. The store is OWNED (not `Arc`-shared) so
/// that adopting it moves it onto the indexer — nothing can retain a second
/// reference that would still hold the file open at promote time, when the
/// rename requires every handle released.
/// Test: `super::resume_tests::interrupted_reindex_resumes_to_identical_index`,
/// `super::resume_tests::probe_hands_the_open_staging_corpus_to_the_adoption`.
pub(super) struct ResumeState {
    /// Path of the staging corpus to adopt (`index.redb.tmp`).
    pub(super) tmp_path: PathBuf,
    /// The open staging corpus, moved onto the indexer by the adoption (#4721).
    pub(super) staged: crate::core::corpus::CorpusStore,
    /// The staged file-hash table — the done-set for this resume.
    pub(super) staged_hashes: Vec<(String, String)>,
    /// Chunks already present in the staging corpus (logging / SSE only).
    pub(super) staged_chunks: usize,
}

/// Look for an adoptable staging corpus left by an interrupted run (#3979).
///
/// Why: this is the single entry point that turns "there is a file at
/// `index.redb.tmp`" into "that file is safe to continue". It is called before
/// the hash-cache is loaded, because whether the cache comes from the live
/// corpus or from the staged one is exactly what resuming changes.
/// What: resolves the staging path, and — when it exists — opens it, reads and
/// [`evaluate`]s its checkpoint, and on `Resume` returns the staged file-hash
/// table together with the still-open handle (#4721: the adoption reuses it
/// rather than opening the same file a second time). Every discard path drops
/// the handle before unlinking the file it refers to. Every failure mode
/// (missing file, unopenable file, absent record,
/// unparseable record, any mismatch) logs a reason and returns `None`, which
/// leaves the caller on the untouched pre-#3979 path. On a `Discard` the stale
/// staging file is removed so it cannot be re-evaluated on every subsequent run;
/// the LIVE corpus is never touched by any branch of this function.
/// Test: `super::resume_tests::interrupted_reindex_resumes_to_identical_index`,
/// `corrupt_checkpoint_falls_back_to_full_reindex`,
/// `stale_root_checkpoint_falls_back_to_full_reindex`.
pub(super) async fn probe_resume(
    handle: &IndexHandle,
    index_id: &IndexId,
    canonical_root: &Path,
    current: &ReindexCheckpoint,
) -> Option<ResumeState> {
    if !resume_enabled() {
        tracing::debug!(
            "reindex[{}]: resume-from-checkpoint disabled via TRUSTY_REINDEX_RESUME",
            index_id.0
        );
        return None;
    }
    let tmp_path = super::corpus_swap::staging_corpus_path(handle, index_id)?;
    if !tmp_path.exists() {
        return None;
    }

    // Open the STAGING file only. `open_serialized` is required here for the
    // same reason the swap paths use it (#3659): another task may be opening
    // this exact path concurrently.
    let open_path = tmp_path.clone();
    let opened = crate::core::corpus::open_serialized(&tmp_path, move || {
        crate::core::corpus::CorpusStore::open(&open_path)
    })
    .await;
    let store = match opened {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "reindex[{}]: found a staging corpus at {} but could not open it ({e}) — \
                 falling back to a full reindex (issue #3979)",
                index_id.0,
                tmp_path.display(),
            );
            discard_staging(&tmp_path, index_id, "staging corpus could not be opened").await;
            return None;
        }
    };

    // #4721: the store is handed BACK out of the blocking task rather than
    // dropped here, so the adoption below inherits this one open handle instead
    // of re-opening the same redb file (see `ResumeState`).
    let read = tokio::task::spawn_blocking(move || {
        let raw = store.read_reindex_checkpoint_sync()?;
        let hashes = store.load_file_hashes()?;
        let chunks = store.chunk_count().unwrap_or(0);
        Ok::<_, anyhow::Error>((store, raw, hashes, chunks))
    })
    .await;
    let (store, raw, staged_hashes, staged_chunks) = match read {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            tracing::warn!(
                "reindex[{}]: staging corpus at {} is unreadable ({e}) — falling back \
                 to a full reindex (issue #3979)",
                index_id.0,
                tmp_path.display(),
            );
            discard_staging(&tmp_path, index_id, "staging corpus is unreadable").await;
            return None;
        }
        Err(e) => {
            tracing::warn!(
                "reindex[{}]: staging-corpus probe task panicked ({e}) — falling back \
                 to a full reindex (issue #3979)",
                index_id.0,
            );
            return None;
        }
    };

    let Some(raw) = raw else {
        // A staging file with no checkpoint record: either pre-#3979 leftovers
        // or a `force` run's staging (force never checkpoints). Not adoptable.
        // #4721: release the handle before unlinking the file it holds.
        drop(store);
        discard_staging(
            &tmp_path,
            index_id,
            "staging corpus carries no checkpoint record",
        )
        .await;
        return None;
    };
    let found: ReindexCheckpoint = match serde_json::from_slice(&raw) {
        Ok(c) => c,
        Err(e) => {
            drop(store); // #4721: release before discarding the file.
            discard_staging(
                &tmp_path,
                index_id,
                &format!("checkpoint record is corrupt ({e})"),
            )
            .await;
            return None;
        }
    };

    match evaluate(&found, current, checkpoint_max_age_secs()) {
        ResumeDecision::Discard { reason } => {
            drop(store); // #4721: release before discarding the file.
            discard_staging(&tmp_path, index_id, &reason).await;
            None
        }
        ResumeDecision::Resume => {
            tracing::info!(
                "reindex[{}]: RESUMING from checkpoint at {} — {} chunk(s) and {} file \
                 hash(es) already staged under root {}; every staged file is still \
                 re-hashed against its current on-disk bytes before it can be skipped \
                 (issue #3979)",
                index_id.0,
                tmp_path.display(),
                staged_chunks,
                staged_hashes.len(),
                canonical_root.display(),
            );
            Some(ResumeState {
                tmp_path,
                // #4721: hand the validated, still-open handle to the adoption.
                staged: store,
                staged_hashes,
                staged_chunks,
            })
        }
    }
}

/// Delete a staging corpus that cannot be adopted, logging why (#3979).
///
/// Why: leaving an unadoptable `index.redb.tmp` on disk would make every
/// subsequent reindex pay the probe cost and reach the same conclusion, and on a
/// large index it is a multi-GB orphan. `CorpusStore::open_fresh` would delete
/// it moments later anyway — doing it here just makes the reason visible.
/// What: best-effort `remove_file`; a failure is logged at `warn` and ignored
/// because the fresh-open path removes it as well. Only ever called with a
/// `*.tmp` staging path — never with a live corpus path.
/// Test: `super::resume_tests::stale_root_checkpoint_falls_back_to_full_reindex`
/// asserts the fallback produces a full, correct index.
async fn discard_staging(tmp_path: &Path, index_id: &IndexId, reason: &str) {
    tracing::info!(
        "reindex[{}]: discarding the staging corpus at {} — {reason}; this reindex \
         rebuilds from scratch (issue #3979). The live corpus is untouched.",
        index_id.0,
        tmp_path.display(),
    );
    let path = tmp_path.to_path_buf();
    let removed = tokio::task::spawn_blocking(move || match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    })
    .await;
    if let Ok(Err(e)) = removed {
        tracing::warn!(
            "reindex[{}]: could not remove the unadoptable staging corpus at {} ({e}) — \
             the fresh-open path will clear it",
            index_id.0,
            tmp_path.display(),
        );
    }
}

/// Seed `map` with the staged done-set, replacing whatever was in it (#3979).
///
/// Why: the in-process hash cache normally mirrors the LIVE corpus. On a resume
/// the authority is the STAGED corpus instead, and the two are not
/// interchangeable: an entry that survives from the live cache but has no
/// corresponding rows in the staging corpus would let the batch loop skip a file
/// whose chunks are not in the corpus being promoted — a silently incomplete
/// index. Clearing first makes the cache exactly the staged done-set.
/// What: clears `map`, then inserts every staged `(relative_path, hash)` pair.
/// Returns the number of entries loaded.
/// Test: `super::resume_tests::interrupted_reindex_resumes_to_identical_index`.
pub(super) fn seed_hash_cache_from_staging(
    map: &Arc<DashMap<PathBuf, String>>,
    staged: &[(String, String)],
) -> usize {
    map.clear();
    for (path, hash) in staged {
        map.insert(PathBuf::from(path), hash.clone());
    }
    map.len()
}

/// Wall-clock seconds since the Unix epoch, saturating at 0 before it.
///
/// Why: the age gate needs a coarse timestamp, not a monotonic clock — it must
/// survive process restarts, which `Instant` cannot.
/// What: `SystemTime::now()` minus `UNIX_EPOCH`, `0` if the clock is before it.
/// Test: exercised by `stale_by_age_is_discarded`.
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod tests;
