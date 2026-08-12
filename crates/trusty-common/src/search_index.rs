//! Shared best-effort "ensure this project is indexed by trusty-search" entry
//! point, hoisted out of trusty-mpm so a second crate (trusty-code) can reuse
//! the ONE implementation instead of duplicating it.
//!
//! Why: the register-and-populate logic (derive the canonical index id, then
//! find-or-create the daemon-side index and best-effort trigger a
//! freshness-gated reindex) originally lived only in trusty-mpm's
//! `core::session_launch::search_index::register_project_index` (issues #1373 /
//! #1908). trusty-code now wants the same behaviour at task start so a tcode
//! run's working project is discoverable via trusty-search while the agent
//! loop proceeds. Per the workspace's common-entry-point rule (CLAUDE.md), a
//! capability used by two crates must be one shared function in trusty-common —
//! not copy-pasted — so the two call sites can never silently diverge.
//!
//! What: [`ensure_project_indexed`] resolves the git-root, derives the index id
//! via [`crate::resolve_project_root`] / [`crate::derive_index_id`], and — when
//! the daemon is discoverable — best-effort registers the index (`POST
//! /indexes`, ~1s cap) then best-effort triggers a freshness-gated reindex
//! (`POST /indexes/{id}/reindex`, ~2s cap, skipped when the index already holds
//! chunks indexed within the last hour). Every step is fail-open in the sense
//! that failures are logged at warn/debug and never propagated, so the caller (a
//! session launch or a task run) is never blocked or aborted by an
//! unreachable/slow search daemon. What it is NOT (#5091) is fail-open in its
//! RETURN: the id comes back only when the daemon confirmed the index, so a
//! failed create cannot advance a caller's pin. The blocking HTTP calls run on dedicated OS
//! threads so the function is safe to call from inside a tokio runtime.
//!
//! Mid-task incremental re-indexing: [`ensure_project_indexed`] runs once, at
//! task start — for a greenfield project that starts EMPTY, that means
//! `search_code` finds nothing the engineer writes DURING the task.
//! [`index_files_best_effort`] complements it: called after each successful
//! file write/edit, it POSTs just that file's fresh content to the daemon's
//! cheap per-file `POST /indexes/{id}/index-file` endpoint (never a full
//! reindex walk), so the growing codebase stays searchable within the same
//! task. Same fail-open contract, and non-blocking by construction (hands the
//! work to a background pool rather than relying on the caller to wrap it,
//! since its call sites are tcode's tool executors, not a one-shot task-start
//! hook). That pool is BOUNDED (issue #2798) — see [`crate::index_dispatch`]
//! for the sizes and for what happens to a batch submitted when it is full.
//!
//! `allow_sensitive_path` (issue #2914 — ephemeral index leak): earlier
//! revisions hardcoded `allow_sensitive_path: true` on every `POST /indexes`
//! this module issued, unconditionally bypassing the daemon's
//! `SENSITIVE_PATH_PREFIXES` denylist (`/tmp`, `/private/tmp`, `/var/folders`,
//! `/private/var/folders`) for BOTH callers. That bypass is only meaningful
//! for trusty-code, whose `directory`-bound working project can legitimately
//! live under an OS-temp prefix (issue #2747: a tcode scratch/bake-off
//! project). trusty-mpm's session-launch caller never has a legitimate reason
//! to index an OS-temp path — a real session workspace is always either the
//! user's checked-out repo or a `.worktrees/<uuid>` leaf INSIDE it — so for
//! that caller the bypass was a pure liability: any test exercising the
//! session-launch pipeline with a `tempfile`-backed workspace stand-in (e.g.
//! trusty-mpm's own `*-selfheal-ws`/`*-stale-heal-ws` fixtures) silently
//! registered that throwaway tempdir against whatever REAL trusty-search
//! daemon happened to be discoverable, because the denylist's one guard
//! against exactly that was switched off unconditionally. [`ensure_project_indexed`]
//! now takes `allow_sensitive_path` as an explicit parameter so each caller
//! states its own intent instead of inheriting trusty-code's opt-in for free.
//!
//! Test: `create_rejected_by_the_daemon_withholds_the_pinnable_id`,
//! `ensure_project_indexed_withholds_id_when_nothing_was_registered`,
//! `ensure_project_indexed_none_for_root`, the `index_is_fresh_*` predicate
//! tests, the `index_files_inner_*` / `relative_index_path_*` /
//! `index_file_request_body_*` tests, and the incremental-hardening tests
//! `retry_backoff_is_bounded_and_increasing` /
//! `post_index_file_retries_transient_send_failure` /
//! `post_index_file_exhausts_retries_and_returns_send_failed` in the `tests`
//! module below, plus the #2798 saturation test
//! `index_files_best_effort_drops_the_batch_when_the_shared_pool_is_saturated`.

use std::path::Path;

/// Find-or-create the trusty-search index for `project_root`, best-effort
/// trigger a reindex so it is actually populated, and return its id (issues
/// #1373, #1908).
///
/// Why: pinning a session/task to an index id is only useful if that index
/// actually exists in the daemon — otherwise a query against it returns nothing
/// and the LLM falls back to guessing (the very bug #1373 fixes). Callers
/// therefore derive the project's canonical index id (the same rule
/// trusty-search's `detect_project` uses, via [`crate::derive_index_id`]) and
/// best-effort register it with the running daemon. The daemon's `POST
/// /indexes` is idempotent (returns `created: false` for an existing id), so a
/// re-register is safe and cheap. Issue #1908: `POST /indexes` alone only
/// registers an EMPTY index and starts a future-changes file watcher — it never
/// walks the existing tree — so a reindex is triggered right after, in the same
/// reachable-daemon branch, sharing one "is the daemon up" check.
///
/// `allow_sensitive_path` (issue #2914): forwarded verbatim to `POST
/// /indexes`' `allow_sensitive_path` field (see
/// [`create_index_request_body`]). Pass `true` ONLY when the caller's
/// `project_root` may legitimately be a deliberately-bound OS-temp path (e.g.
/// tcode's `directory` binding — issue #2747); pass `false` for any caller
/// whose root is always a real, persistent project directory (e.g.
/// trusty-mpm's session workspaces), so an accidental OS-temp root — most
/// commonly a `tempfile`-backed fixture standing in for that workspace in a
/// test — is refused by the daemon's `SENSITIVE_PATH_PREFIXES` denylist
/// instead of silently registered against whatever daemon happens to be
/// discoverable.
/// What: resolves the git-root for `project_root`, derives the index id, and —
/// when the id is non-empty AND the trusty-search daemon address is discoverable
/// — POSTs `{id, root_path, allow_sensitive_path}` to `/indexes` then
/// best-effort triggers a reindex (skipping it when the index is already
/// fresh; see [`best_effort_trigger_reindex`]). Returns the id ONLY when that
/// POST came back 2xx — see [`pinnable_index_id`] for why an unconfirmed create
/// must yield `None`. Errors still never propagate: a refusing or absent daemon
/// is logged at warn and the caller makes progress unindexed.
/// Test: `create_rejected_by_the_daemon_withholds_the_pinnable_id`,
/// `ensure_project_indexed_withholds_id_when_nothing_was_registered`,
/// `ensure_project_indexed_none_for_root`,
/// `ensure_project_indexed_sends_allow_sensitive_path_through_to_create_body`.
pub fn ensure_project_indexed(project_root: &Path, allow_sensitive_path: bool) -> Option<String> {
    ensure_project_indexed_with(
        project_root,
        IndexOptions {
            allow_sensitive_path,
            ..IndexOptions::default()
        },
    )
}

/// Per-call knobs for [`ensure_project_indexed_with`] (#5060).
///
/// Why: [`ensure_project_indexed`] grew a second orthogonal dimension when
/// trusty-mpm began registering an index for a git WORKTREE at the moment the
/// worktree is created. A worktree differs from its base checkout by a small
/// diff: its exact text (BM25) and its symbol graph (KG) are branch-specific
/// and must be worktree-accurate, but conceptual similarity is not — it does
/// not change because a branch moved a few functions. So the expensive lane
/// (embedding) is built once on the base checkout and the cheap lanes are
/// built per worktree. A struct rather than a third positional `bool` keeps
/// the two flags from being silently transposed at a call site.
/// What: a plain options bag whose `Default` reproduces the pre-#5060
/// behaviour exactly (`allow_sensitive_path: false`, `skip_vector: false`), so
/// [`ensure_project_indexed`] stays a one-line wrapper and no existing caller
/// changes behaviour.
///
/// `#[non_exhaustive]`: this crate is published, and the daemon already carries
/// a third orthogonal flag this bag does not yet expose (`skip_kg`), so a third
/// field is expected. Without the attribute, adding it would break every
/// external struct-literal construction — a SemVer break this crate has taken
/// before. The attribute bars struct expressions from other crates outright
/// (functional-update syntax does not exempt them), so external callers build
/// with [`IndexOptions::default`] plus the `with_*` setters:
/// `IndexOptions::default().with_skip_vector(true)`.
/// Test: `index_options_default_matches_legacy_ensure_call`,
/// `create_index_request_body_sets_skip_vector`,
/// `index_options_builders_match_field_construction`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct IndexOptions {
    /// Forwarded to `POST /indexes`' `allow_sensitive_path` field — see
    /// [`ensure_project_indexed`]'s doc comment for when `true` is correct.
    pub allow_sensitive_path: bool,

    /// When `true`, ask the daemon to register this index with its vector
    /// lane permanently suppressed (`skip_vector`, trusty-search issue #2984
    /// Phase 1): the embedder is never invoked, the semantic stage is marked
    /// `Skipped`, and the index's reported `search_capabilities` omit
    /// `vector`. BM25 and KG are built as normal.
    pub skip_vector: bool,
}

impl IndexOptions {
    /// Set `allow_sensitive_path`, consuming and returning `self`.
    ///
    /// Why: `#[non_exhaustive]` bars other crates from constructing this bag
    /// with a struct expression at all, so a setter is the only way an external
    /// caller can express a non-default value. See the type's doc for why the
    /// attribute is there.
    /// Test: `index_options_builders_match_field_construction`.
    #[must_use]
    pub fn with_allow_sensitive_path(mut self, allow: bool) -> Self {
        self.allow_sensitive_path = allow;
        self
    }

    /// Set `skip_vector`, consuming and returning `self`.
    ///
    /// Why: see [`IndexOptions::with_allow_sensitive_path`].
    /// Test: `index_options_builders_match_field_construction`.
    #[must_use]
    pub fn with_skip_vector(mut self, skip: bool) -> Self {
        self.skip_vector = skip;
        self
    }
}

/// What the daemon-side half of an `ensure_project_indexed*` call achieved
/// (#5065 review).
///
/// Why: [`ensure_project_indexed_with`] returns the derived id unconditionally,
/// so its caller cannot tell "the daemon confirmed this index exists" from "the
/// daemon was down and nothing was sent". trusty-mpm's worktree hook was
/// logging `worktree index registered` for the second case — announcing a
/// success it never observed, in the one code path whose stated purpose is to
/// make outcomes distinguishable. Reporting the registration outcome beside the
/// id fixes that without making the call fallible: nothing here ever propagates
/// an error. #5091 then wired this enum into the return rather than leaving it
/// advisory: the id-only entry points hand back an id only when it says
/// `Confirmed` (see [`pinnable_index_id`]), while this report still carries the
/// derived id in every case.
/// What: the four terminal states of the `POST /indexes` attempt. Only
/// `Confirmed` means the index is known to exist daemon-side.
/// Test: `reporting_says_skipped_under_test_harness`,
/// `reporting_says_daemon_unreachable_when_no_daemon_is_discoverable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IndexRegistration {
    /// `POST /indexes` returned 2xx — the index exists in the daemon.
    Confirmed,
    /// The daemon address resolved but the create call did not confirm: a
    /// non-2xx response, a transport error, or a panicked worker thread. All
    /// three are logged at warn by [`best_effort_create_index`].
    NotConfirmed,
    /// No trusty-search daemon address could be resolved, so nothing was sent.
    DaemonUnreachable,
    /// This is a test process and the write was deliberately suppressed
    /// (#4255). Never a real registration.
    SkippedUnderTest,
}

/// The derived index id plus what actually happened daemon-side (#5065 review).
///
/// Why: see [`IndexRegistration`]. A struct rather than a tuple so a third
/// reported quantity can be added without breaking callers.
/// What: `index_id` is `None` only when derivation yielded an empty string, in
/// which case nothing was attempted and `registration` is `NotConfirmed`.
/// Test: `reporting_says_skipped_under_test_harness`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct EnsureIndexReport {
    /// The canonical index id, or `None` when derivation yielded empty.
    pub index_id: Option<String>,
    /// What the `POST /indexes` attempt achieved.
    pub registration: IndexRegistration,
}

/// [`ensure_project_indexed`] with explicit per-call [`IndexOptions`] (#5060).
///
/// Why: see [`IndexOptions`]. Kept as a thin wrapper over
/// [`ensure_project_indexed_reporting`] so the register-then-populate sequence
/// can never drift between the session-launch, task-start, and
/// worktree-creation callers.
/// What: identical to [`ensure_project_indexed`] in every respect except that
/// `opts.skip_vector` is threaded into the `POST /indexes` body. Failures are
/// still logged and swallowed rather than propagated, and the returned id is
/// still gated on a confirmed registration ([`pinnable_index_id`]). A caller
/// that needs the DERIVED id whether or not the daemon confirmed it — to name
/// the index in a log line, or to GC it — must use
/// [`ensure_project_indexed_reporting`], where the adjacent `registration`
/// field makes ignoring the failure a visible choice.
/// Test: `create_rejected_by_the_daemon_withholds_the_pinnable_id`,
/// `ensure_project_indexed_none_for_root`,
/// `create_index_request_body_sets_skip_vector`.
pub fn ensure_project_indexed_with(project_root: &Path, opts: IndexOptions) -> Option<String> {
    pinnable_index_id(ensure_project_indexed_reporting(project_root, opts))
}

/// The id a caller may PIN, or `None` when nothing observed the index (#5091).
///
/// Why: `POST /indexes` can fail — a non-2xx, a transport error, a daemon that
/// is not running — and the id-only entry points used to hand the derived id
/// back anyway. Session launch writes that id into `.mcp.json` as
/// `trusty-search serve --index <id>`, so a create that silently failed left the
/// session pinned to an index the daemon has never heard of: every `search`
/// answers `404 unknown index` for the life of the session, while
/// `search_health` and the `search` doctor probe both stay green because they
/// ask about the daemon, not the pin (#5045 measured 4 of 75 live worktrees
/// actually indexed). Withholding the id leaves the pin unadvanced, which is the
/// one outcome that cannot lie: an unpinned stub is visibly unpinned, and
/// `tm doctor`'s `search_index_pin` check says so.
/// What: returns `report.index_id` for [`IndexRegistration::Confirmed`] — the
/// only variant that means the daemon acknowledged the index, and since `POST
/// /indexes` is find-or-create it covers "already existed" too. Every other
/// variant, including the #4255 test-harness suppression (which sends nothing,
/// so it registers nothing), logs at warn and returns `None`.
/// Test: `create_rejected_by_the_daemon_withholds_the_pinnable_id`,
/// `ensure_project_indexed_withholds_id_when_nothing_was_registered`.
fn pinnable_index_id(report: EnsureIndexReport) -> Option<String> {
    if report.registration == IndexRegistration::Confirmed {
        return report.index_id;
    }
    if let Some(id) = &report.index_id {
        // #5091: an unconfirmed create must not advance the caller's pin.
        tracing::warn!(
            "trusty-search index '{id}' was NOT confirmed ({:?}); withholding it so the \
             caller cannot pin an index that may not exist",
            report.registration
        );
    }
    None
}

/// [`ensure_project_indexed_with`], but reporting what the daemon actually did
/// (#5065 review).
///
/// Why: the id-only return cannot distinguish a confirmed registration from a
/// silent no-op against a down daemon — see [`IndexRegistration`]. This is the
/// ONE implementation; the two id-only entry points delegate here, so no third
/// copy of the register-then-populate sequence exists.
/// What: resolves the git-root, derives the id, and — when the id is non-empty,
/// this is not a test process, and a daemon address resolves — issues the
/// find-or-create `POST /indexes` followed by the freshness-gated reindex
/// trigger. Returns the id in every case except empty derivation, alongside the
/// registration outcome. Still fail-open: no step propagates an error.
/// Test: `reporting_says_skipped_under_test_harness`,
/// `reporting_says_daemon_unreachable_when_no_daemon_is_discoverable`,
/// `ensure_project_indexed_none_for_root`.
pub fn ensure_project_indexed_reporting(
    project_root: &Path,
    opts: IndexOptions,
) -> EnsureIndexReport {
    let root = crate::resolve_project_root(project_root);
    let index_id = crate::derive_index_id(&root);
    if index_id.trim().is_empty() {
        tracing::warn!(
            "skipping trusty-search index registration: empty index id for {}",
            root.display()
        );
        return EnsureIndexReport {
            index_id: None,
            registration: IndexRegistration::NotConfirmed,
        };
    }

    // #4255: never register a fixture root against the operator's real daemon.
    if refuse_daemon_write_under_test("registration", &index_id) {
        return EnsureIndexReport {
            index_id: Some(index_id),
            registration: IndexRegistration::SkippedUnderTest,
        };
    }

    // Discover the running daemon's address (issue #2033: via the shared
    // `resolve_daemon_base_url` helper — never a hardcoded port). Absent /
    // unreadable file ⇒ daemon not started, so nothing is sent and nothing is
    // registered. #5091: the earlier claim here — that the daemon would create
    // the index on first reindex — was false; no later step retries, which is
    // why `DaemonUnreachable` is not a pinnable outcome.
    let registration = match crate::resolve_daemon_base_url("trusty-search") {
        Some(base) => {
            let outcome = best_effort_create_index(&base, &index_id, &root, opts);
            best_effort_trigger_reindex(&base, &index_id);
            outcome
        }
        None => {
            tracing::warn!(
                "trusty-search daemon address not found; index '{index_id}' was NOT \
                 registered and nothing will retry it"
            );
            IndexRegistration::DaemonUnreachable
        }
    };

    EnsureIndexReport {
        index_id: Some(index_id),
        registration,
    }
}

/// Should this process refuse to mutate a real trusty-search daemon?
///
/// Why (issue #4255): both mutating entry points in this module talk to
/// whatever daemon is discoverable on the machine. Under `cargo test` that is
/// the OPERATOR's daemon, so a test exercising a session launch or a task run
/// against a `tempfile` fixture registered that throwaway directory in the
/// live `indexes.toml` — the dead roots then stall warm boot for the timeout,
/// once per entry. Issue #2914 narrowed this by making the temp-dir denylist
/// bypass opt-in, and trusty-code's tests added a per-test
/// `isolate_ambient_daemons()` call, but both leave the safety to whoever
/// writes the next test. The live registry carried five `.tmpXXXXXX` roots
/// proving that was forgotten. Deciding it here, in the shared helper, is the
/// version nobody can forget.
/// What: returns `true` — and logs why — when
/// [`crate::running_under_test_harness`] says this is a test process. A test
/// that genuinely wants the real daemon sets `TRUSTY_ALLOW_PRODUCTION_STATE=1`
/// (see [`crate::test_harness::ALLOW_PRODUCTION_ENV`]). Reads are untouched:
/// this gates only the writes.
/// Test: `ensure_project_indexed_never_writes_to_a_daemon_under_test`,
/// `index_files_inner_never_writes_to_a_daemon_under_test`.
fn refuse_daemon_write_under_test(operation: &str, index_id: &str) -> bool {
    if !crate::running_under_test_harness() {
        return false;
    }
    tracing::debug!(
        "test harness detected (issue #4255): skipping trusty-search {operation} for \
         '{index_id}' so a fixture root can never reach the operator's live registry. \
         Set {}=1 to opt in to real daemon writes.",
        crate::test_harness::ALLOW_PRODUCTION_ENV
    );
    true
}

/// Best-effort, non-blocking incremental re-index of specific files into an
/// ALREADY-REGISTERED trusty-search index (mid-task incremental re-indexing).
///
/// Why: [`ensure_project_indexed`] runs once at task start, when a greenfield
/// project is often EMPTY — so `search_code` finds nothing the engineer goes
/// on to write during the task. Re-registering (or fully reindexing) the
/// whole project after every write would mean a full-tree walk per file
/// (expensive); the daemon's per-file `POST /indexes/{id}/index-file`
/// endpoint lets a caller add or update ONE file's chunks cheaply, so the
/// growing codebase stays searchable within the same task.
/// What: submits ONE job to the shared bounded pool ([`crate::index_dispatch`])
/// and returns immediately — the caller (a tool executor mid-turn) must never
/// block or fail because trusty-search is unreachable or slow. On a worker,
/// [`index_files_inner`] derives the same `(root, index_id)`
/// [`ensure_project_indexed`] would (so this always targets the same index a
/// task-start call already created) and POSTs each of `paths` to the daemon. A
/// no-op with zero work submitted when `paths` is empty.
///
/// Saturation (issue #2798): the pool runs at most
/// [`crate::index_dispatch::MAX_INDEX_WORKERS`] batches at once with at most
/// [`crate::index_dispatch::INDEX_QUEUE_CAPACITY`] more queued. A batch
/// submitted when both are full is **DROPPED, not blocked and not queued** —
/// the alternative, blocking the caller, would turn a slow daemon into a
/// stalled agent task. The drop is not silent: it is logged at `warn` naming
/// the file count, the project root, the first path, and the running
/// process-wide drop total — and it is readable as state via
/// [`index_drop_stats`], which trusty-code's `GET /health` publishes so a
/// saturation episode changes the health answer rather than only a log line.
/// Losing an incremental update degrades mid-task search freshness until the
/// next write or reindex covers the file; it does not lose the file, and it
/// does not fail the tool call.
///
/// Sensitive-path note (issue #2747): unlike `POST /indexes`, the per-file
/// `index-file` endpoint does NOT re-run the sensitive-path denylist — it
/// looks the index up by id in the daemon's in-memory registry
/// (`crates/trusty-search/src/service/server/files.rs`'s `index_file_handler`
/// calls `state.registry.get(&index_id)`, never `allowlist::is_denied`), so
/// an index created under the #2747 `allow_sensitive_path` bypass (a tempdir
/// root) accepts incremental updates unconditionally. No bypass flag is
/// threaded through here because none is needed.
/// Test: `index_files_best_effort_drops_the_batch_when_the_shared_pool_is_saturated`
/// covers the submit/reject half; the work itself is [`index_files_inner`],
/// which the `index_files_inner_*` tests below exercise directly
/// (synchronously, off any worker) for determinism.
pub fn index_files_best_effort(project_root: &Path, paths: &[std::path::PathBuf]) {
    if paths.is_empty() {
        return;
    }
    let count = paths.len();
    let root_display = project_root.display().to_string();
    let first = paths.first().map(|p| p.display().to_string());
    let project_root = project_root.to_path_buf();
    let paths = paths.to_vec();

    // #2798: bound the in-flight indexing threads — a degraded daemon must not
    // let a burst of writes spawn OS threads without limit.
    let accepted = crate::index_dispatch::global().try_submit(Box::new(move || {
        index_files_inner(&project_root, &paths);
    }));
    if !accepted {
        tracing::warn!(
            "DROPPED incremental trusty-search index update for {count} file(s) under \
             {root_display} (first: {}): all {} indexing workers are busy and the \
             {}-slot queue is full; {} batch(es) dropped in this process so far (#2798)",
            first.as_deref().unwrap_or("<none>"),
            crate::index_dispatch::MAX_INDEX_WORKERS,
            crate::index_dispatch::INDEX_QUEUE_CAPACITY,
            crate::index_dispatch::global().rejected(),
        );
    }
}

/// How many incremental index batches this process has dropped, and when the
/// last one happened (#2798 review).
///
/// Why: the bound is only acceptable because the loss it creates is visible.
/// A `warn!` line nobody greps is not visibility, so this is the read surface a
/// health check consumes — trusty-code's `GET /health` publishes it as
/// `incremental_index`.
///
/// The four fields are two pairs, and both pairs are needed. Within a pair, the
/// count says whether the loss has EVER happened and the age says whether it is
/// happening NOW — a monotonic total alone cannot distinguish a wedged daemon
/// right now from one episode an hour ago. Between the pairs, a DROP means the
/// pool refused the batch outright and none of it ran, while a TRUNCATION means
/// the pool accepted and started the batch and then
/// [`BATCH_INDEX_BUDGET`] cut it short partway. Different causes, different
/// fixes, so they are never summed: an episode where every batch is accepted
/// and then truncated leaves files unindexed while `dropped_batches` reads `0`
/// forever.
/// What: both counts are monotonic for the life of the process; each age is
/// `None` until that loss first happens, then the age of the most recent one
/// (saturating at 0 if the wall clock moved backwards). All four read the
/// shared pool, so they cover every caller in the process.
/// Test: `index_files_best_effort_drops_the_batch_when_the_shared_pool_is_saturated`
/// (asserts the drop pair right after a real drop),
/// `a_truncated_batch_is_counted_separately_from_a_dropped_one`,
/// `a_fresh_pool_reports_no_drop_ever`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct IndexDropStats {
    /// Batches refused at submission because the pool was saturated, since
    /// process start. None of a dropped batch's files were indexed.
    pub dropped_batches: u64,
    /// Age in seconds of the most recent drop; `None` if there has been none.
    pub seconds_since_last_drop: Option<u64>,
    /// Batches the pool accepted and started, then cut short at
    /// [`BATCH_INDEX_BUDGET`], since process start.
    ///
    /// The files a truncated batch had not reached yet are ABANDONED, not
    /// retried: nothing records which paths were skipped, so this crate never
    /// attempts them again. They become searchable once something unrelated
    /// covers them — the next write to the same file, a full reindex, or
    /// trusty-search's own file watcher where one is running for that index —
    /// and nothing here triggers or confirms any of those.
    pub truncated_batches: u64,
    /// Age in seconds of the most recent truncation; `None` if there has been
    /// none.
    pub seconds_since_last_truncation: Option<u64>,
}

/// Snapshot the shared pool's loss counters — see [`IndexDropStats`].
///
/// Test: `index_files_best_effort_drops_the_batch_when_the_shared_pool_is_saturated`,
/// `a_truncated_batch_is_counted_separately_from_a_dropped_one`.
#[must_use]
pub fn index_drop_stats() -> IndexDropStats {
    let pool = crate::index_dispatch::global();
    IndexDropStats {
        dropped_batches: pool.rejected(),
        seconds_since_last_drop: seconds_since(pool.last_drop_unix_secs()),
        truncated_batches: pool.truncated(),
        seconds_since_last_truncation: seconds_since(pool.last_truncation_unix_secs()),
    }
}

/// Age in seconds of a unix-second stamp, saturating at 0 if the clock moved
/// backwards.
fn seconds_since(at: Option<u64>) -> Option<u64> {
    at.map(|at| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        now.saturating_sub(at)
    })
}

/// Wall-clock budget one batch may spend indexing before it stops early.
///
/// Why (#2798 review): a job is a whole `write_files` BATCH, and that tool caps
/// nothing — a scaffold write is one job. At [`MAX_INDEX_ATTEMPTS`]'s ~6.2s
/// worst case per file against a degraded daemon, a 30-file batch would hold
/// one of the four workers for over three minutes, and the queue-depth
/// reasoning behind [`crate::index_dispatch::INDEX_QUEUE_CAPACITY`] collapses.
/// Capping the batch in TIME is what makes worker turnover derivable: no job
/// occupies a worker for more than this budget plus the one file already in
/// flight (~36s), so a full 64-slot queue drains in ~10 minutes worst case
/// rather than an unbounded time.
/// What: 30s, checked before each file — never mid-request, so an in-flight
/// POST always finishes. Files the batch had not reached when the budget ran
/// out are abandoned; the loss is counted as
/// [`IndexDropStats::truncated_batches`], separately from a pool rejection.
/// Test: `batch_budget_is_exhausted_at_and_past_the_cap`.
pub(crate) const BATCH_INDEX_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// Has this batch spent its [`BATCH_INDEX_BUDGET`]?
///
/// Test: `batch_budget_is_exhausted_at_and_past_the_cap`.
fn batch_budget_exhausted(elapsed: std::time::Duration) -> bool {
    elapsed >= BATCH_INDEX_BUDGET
}

/// Should this batch stop here? Counts and logs the truncation when it should.
///
/// Why: the decision and the accounting are one function so a batch cannot stop
/// without being counted. When the `break` only logged, a sustained episode in
/// which every batch was accepted and then truncated reported
/// `dropped_batches: 0` in `GET /health` for as long as it lasted, while files
/// went unindexed batch after batch — the same single-reader blind spot the
/// rejection counter was added to close, reintroduced on the other loss path.
/// Splitting "decide" from "record" is what would let it come back.
/// What: returns `true` once [`batch_budget_exhausted`], and on that edge
/// records the truncation on the shared pool (readable as
/// [`IndexDropStats::truncated_batches`], distinct from a drop) and warns with
/// how many of the batch's files were reached and how many are abandoned.
/// Returns `false` and records nothing while the budget holds.
/// Test: `a_truncated_batch_is_counted_separately_from_a_dropped_one`,
/// `an_unexhausted_budget_records_no_truncation`.
fn stop_batch_for_budget(
    elapsed: std::time::Duration,
    index_id: &str,
    done: usize,
    total: usize,
) -> bool {
    if !batch_budget_exhausted(elapsed) {
        return false;
    }
    crate::index_dispatch::global().record_truncation();
    tracing::warn!(
        "incremental index update for '{index_id}' stopped after {done} of {total} \
         file(s): the {}s per-batch budget was exhausted; the remaining {} file(s) \
         were skipped and are NOT retried — they stay searchable only from the next \
         write, a reindex, or the daemon's own file watcher (#2798)",
        BATCH_INDEX_BUDGET.as_secs(),
        total.saturating_sub(done),
    );
    true
}

/// Synchronous body of [`index_files_best_effort`], run on a pool worker (or
/// called directly by tests for determinism).
///
/// Why: split out so tests can exercise the fail-open branches (empty index
/// id, undiscoverable daemon) synchronously, without waiting on — or racing
/// — a spawned thread.
/// What: derives `(root, index_id)` via [`crate::resolve_project_root`] /
/// [`crate::derive_index_id`]; returns early (logged at debug) when the id is
/// empty or [`crate::resolve_daemon_base_url`] finds no running daemon;
/// otherwise builds ONE pooled HTTP client for the whole batch (issue #2785:
/// so multiple files in a `write_files` batch reuse keep-alive connections
/// instead of a fresh TCP connect per file) and, for each path, resolves it
/// against `root`, reads its current content from disk (an unreadable file —
/// e.g. deleted since the write — is logged at debug and skipped, not fatal to
/// the batch), and POSTs it via [`best_effort_index_one_file`] (which itself
/// retries transient send failures with backoff). Every step fails open. The
/// loop also stops early once [`BATCH_INDEX_BUDGET`] is spent (#2798) — a batch
/// has no size limit, so without that a single large write pins a pool worker
/// for minutes. Stopping goes through [`stop_batch_for_budget`], which counts
/// the truncation into [`index_drop_stats`] as well as logging it; the files it
/// had not reached are abandoned, never retried from here.
/// Test: `index_files_inner_is_noop_for_empty_paths`,
/// `index_files_inner_skips_when_index_id_empty`,
/// `index_files_inner_skips_gracefully_when_daemon_down`.
fn index_files_inner(project_root: &Path, paths: &[std::path::PathBuf]) {
    if paths.is_empty() {
        return;
    }
    let root = crate::resolve_project_root(project_root);
    let index_id = crate::derive_index_id(&root);
    if index_id.trim().is_empty() {
        tracing::debug!(
            "skipping incremental trusty-search index update: empty index id for {}",
            root.display()
        );
        return;
    }
    // #4255: never push fixture file content into the operator's real indexes.
    if refuse_daemon_write_under_test("incremental index update", &index_id) {
        return;
    }
    let Some(base) = crate::resolve_daemon_base_url("trusty-search") else {
        tracing::debug!(
            "trusty-search daemon address not found; skipping incremental index \
             update for '{index_id}' ({} file(s))",
            paths.len()
        );
        return;
    };

    // One client per batch (#2785): reqwest keeps a connection pool per client,
    // so reusing it across the batch's files lets rapid successive writes ride
    // existing keep-alive connections instead of paying a fresh TCP connect
    // (and its transient-failure risk) per file. Fail open if it cannot build.
    let client = match build_index_client() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("skipping incremental index update: could not build HTTP client: {e}");
            return;
        }
    };

    // #2798: a batch is unbounded in size, so cap it in time — otherwise one
    // large write holds a worker for minutes and the queue never turns over.
    let started = std::time::Instant::now();
    for (done, path) in paths.iter().enumerate() {
        if stop_batch_for_budget(started.elapsed(), &index_id, done, paths.len()) {
            break;
        }
        let abs = if path.is_absolute() {
            path.clone()
        } else {
            root.join(path)
        };
        let rel = relative_index_path(&root, &abs);
        let content = match std::fs::read_to_string(&abs) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(
                    "skipping incremental index update for {}: {e}",
                    abs.display()
                );
                continue;
            }
        };
        best_effort_index_one_file(&client, &base, &index_id, &rel, &content);
    }
}

/// Resolve `abs` to the path string the corpus stores for a file under `root`.
///
/// Why: the reindex walker stores every chunk's `file` field relative to the
/// index root (`crates/trusty-search/src/service/walker.rs` strips the
/// canonical root prefix); posting an absolute path here would create a
/// duplicate, differently-keyed corpus entry for the same file instead of
/// updating the one the walker already produced.
/// What: strips `root` as a prefix and forward-slash-normalises the
/// remainder; falls back to `abs` itself (lossy) when it does not live under
/// `root` — should not happen for a working-directory-scoped tool write, but
/// fails safe rather than panicking or silently dropping the update.
/// Test: `relative_index_path_strips_root_prefix`,
/// `relative_index_path_falls_back_for_paths_outside_root`.
fn relative_index_path(root: &Path, abs: &Path) -> String {
    abs.strip_prefix(root)
        .unwrap_or(abs)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Build the pooled blocking HTTP client used for incremental index updates.
///
/// Why: extracted so [`index_files_inner`] builds exactly ONE client per batch
/// (issue #2785 connection reuse) and so the retry test can construct an
/// identically-configured client.
/// What: a `reqwest::blocking::Client` with a 2s overall / 750ms connect
/// timeout — tight caps because this runs on a mid-task detached thread and
/// must never stall a long task when the daemon is slow. reqwest maintains an
/// idle-connection pool per client, so reusing the returned client across a
/// batch's files amortises TCP/handshake setup.
/// Test: covered indirectly by `post_index_file_retries_transient_send_failure`
/// (which builds and drives one), and by the daemon-down fail-open path in
/// `index_files_inner_skips_gracefully_when_daemon_down`.
fn build_index_client() -> reqwest::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .connect_timeout(std::time::Duration::from_millis(750))
        .build()
}

/// Max attempts (initial try + retries) for a single per-file index POST.
///
/// Why: issue #2785 — under sustained mid-task load the per-file HTTP call sees
/// transient send failures (connection resets / connect races under rapid
/// repeated writes). A tiny bounded retry recovers the vast majority of them.
/// What: 3 total attempts.
/// Latency note: the SLEEP this adds beyond a single attempt is only
/// [`retry_backoff`]'s sum (~200ms across 3 attempts) — cheap when failures
/// are the fast connect-refused/reset kind this fix targets. But that is NOT
/// the worst-case TOTAL latency: each attempt still carries
/// [`build_index_client`]'s own per-call timeout (2s overall / 750ms connect),
/// and a *slow-but-reachable* daemon can consume the full 2s on every attempt
/// before erroring or hanging up. Worst case against such a daemon is
/// therefore ~3 × 2s + ~200ms backoff ≈ **6.2s for a single file**, on the
/// batch's detached thread — never on the tool-executor's return path, but
/// worth knowing before shrinking timeouts or raising `MAX_INDEX_ATTEMPTS`.
/// Test: `retry_backoff_is_bounded_and_increasing`.
const MAX_INDEX_ATTEMPTS: u32 = 3;

/// Backoff to sleep BEFORE retry `attempt` (1-based) of a per-file index POST.
///
/// Why: a transient send failure under load often clears within tens of
/// milliseconds once the daemon drains the burst; a short exponential backoff
/// spaces retries without materially slowing the task. Kept as a pure function
/// so the schedule is unit-testable without any I/O.
/// What: `50ms * 3^(attempt-1)`, capped at 1s — i.e. 50ms before the 2nd try,
/// 150ms before the 3rd. Saturating arithmetic keeps it panic-free for any
/// `attempt`.
/// Test: `retry_backoff_is_bounded_and_increasing`.
fn retry_backoff(attempt: u32) -> std::time::Duration {
    let factor = 3u64.saturating_pow(attempt.saturating_sub(1));
    let millis = 50u64.saturating_mul(factor).min(1000);
    std::time::Duration::from_millis(millis)
}

/// Outcome of a per-file index POST, surfaced so tests can assert the
/// retry-then-succeed AND retry-exhaustion paths without scraping logs.
///
/// Why: [`post_index_file_with_retries`] is otherwise pure I/O; returning a
/// small enum lets tests prove both that a transient send failure is retried
/// and ultimately succeeds, and that persistent failure is reported (not
/// silently hung or panicked) once attempts are exhausted.
/// What: `Indexed` (2xx), `HttpStatus` (non-2xx — not retried; a 4xx/404 for an
/// unknown index won't fix itself), or `SendFailed` (transport error on every
/// attempt).
/// Test: `post_index_file_retries_transient_send_failure`,
/// `post_index_file_exhausts_retries_and_returns_send_failed`.
#[derive(Debug, PartialEq, Eq)]
enum IndexOutcome {
    Indexed,
    HttpStatus(u16),
    SendFailed,
}

/// POST a single file's `{path, content}` to `url`, retrying transient send
/// failures with [`retry_backoff`] up to [`MAX_INDEX_ATTEMPTS`] times.
///
/// Why: issue #2785 — a single transport-level `send()` failure (connection
/// reset/connect race under rapid concurrent writes) previously dropped the
/// update entirely. Retrying transport errors (but NOT HTTP non-2xx, which
/// will not self-heal) recovers those transient failures.
/// What: reuses the caller-supplied pooled `client`; on a transport `Err` it
/// sleeps [`retry_backoff`] and retries (until attempts are exhausted → returns
/// `SendFailed`); a 2xx returns `Indexed` immediately; any other status returns
/// `HttpStatus` immediately (no retry). Never panics, never propagates. See
/// [`MAX_INDEX_ATTEMPTS`]'s doc comment for the latency distinction between
/// the ~200ms of added backoff SLEEP and the much larger (~6.2s) worst-case
/// TOTAL wall time this function can spend against a slow-but-up daemon,
/// since each of the 3 attempts carries its own 2s/750ms client timeout.
/// Test: `post_index_file_retries_transient_send_failure`,
/// `post_index_file_exhausts_retries_and_returns_send_failed`.
fn post_index_file_with_retries(
    client: &reqwest::blocking::Client,
    url: &str,
    body: &serde_json::Value,
) -> IndexOutcome {
    let mut last_err: Option<reqwest::Error> = None;
    for attempt in 0..MAX_INDEX_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(retry_backoff(attempt));
        }
        match client.post(url).json(body).send() {
            Ok(resp) if resp.status().is_success() => return IndexOutcome::Indexed,
            Ok(resp) => return IndexOutcome::HttpStatus(resp.status().as_u16()),
            Err(e) => last_err = Some(e),
        }
    }
    if let Some(e) = &last_err {
        tracing::debug!(
            "per-file index POST to {url} failed after {MAX_INDEX_ATTEMPTS} attempts: {e}"
        );
    }
    IndexOutcome::SendFailed
}

/// POST `/indexes/{id}/index-file` for a single file; failures are logged,
/// never propagated.
///
/// Why: mirrors [`best_effort_create_index`]'s fail-open contract for the
/// per-file endpoint, hardened for issue #2785 (retry + connection reuse).
/// What: delegates to [`post_index_file_with_retries`] using the pooled
/// `client` [`index_files_inner`] built once for the batch (so rapid writes
/// reuse keep-alive connections). Unlike [`best_effort_create_index`], this
/// does NOT spawn-and-join its own nested OS thread: it is only ever reached
/// from inside [`index_files_inner`] running on a [`crate::index_dispatch`]
/// pool worker (submitted by [`index_files_best_effort`]), a plain
/// `std::thread` that is already off any tokio runtime, so a
/// direct blocking call here cannot trigger the "cannot drop a runtime in a
/// context where blocking is not allowed" panic. A non-2xx response (including
/// 404 for an unregistered/unknown index — e.g. the daemon restarted since task
/// start) is logged at warn; a transport error surviving all retries is logged
/// at warn. Both are swallowed.
/// Test: exercised via `index_files_inner_skips_gracefully_when_daemon_down`
/// (daemon-down path, never reaches this function) and
/// `post_index_file_retries_transient_send_failure` (retry path); the live HTTP
/// success path is covered by integration use.
fn best_effort_index_one_file(
    client: &reqwest::blocking::Client,
    base: &str,
    index_id: &str,
    rel_path: &str,
    content: &str,
) {
    let url = format!("{base}/indexes/{index_id}/index-file");
    let body = index_file_request_body(rel_path, content);

    match post_index_file_with_retries(client, &url, &body) {
        IndexOutcome::Indexed => {
            tracing::debug!("incrementally indexed '{rel_path}' into '{index_id}'");
        }
        IndexOutcome::HttpStatus(status) => {
            tracing::warn!(
                "incremental index update for '{rel_path}' in '{index_id}' returned HTTP {status}"
            );
        }
        IndexOutcome::SendFailed => {
            tracing::warn!(
                "incremental index update for '{rel_path}' in '{index_id}' failed after \
                 {MAX_INDEX_ATTEMPTS} attempts"
            );
        }
    }
}

/// Build the JSON body for the `POST /indexes/{id}/index-file` call.
///
/// Why: extracted so the request shape is unit-testable without a live
/// daemon or a spawned thread — mirrors [`create_index_request_body`].
/// What: `{path, content}` — the exact shape the per-file endpoint's
/// `IndexFileRequest` expects (`crates/trusty-search/src/service/server/router.rs`).
/// No `allow_sensitive_path` field: see [`index_files_best_effort`]'s doc
/// comment for why the per-file endpoint needs no such opt-in.
/// Test: `index_file_request_body_targets_relative_path_and_content`.
fn index_file_request_body(rel_path: &str, content: &str) -> serde_json::Value {
    serde_json::json!({
        "path": rel_path,
        "content": content,
    })
}

/// Build the JSON body for the `POST /indexes` find-or-create call.
///
/// Why: extracted from `best_effort_create_index` so the request shape —
/// specifically, whether `allow_sensitive_path` is set — is unit-testable
/// without a live daemon or a spawned thread.
/// What: `allow_sensitive_path` (explicit-index-sensitive-path-bypass) is
/// forwarded verbatim from the caller (issue #2914 — it is NOT unconditionally
/// `true` any more). When `true`, this is the "explicit request" case the
/// daemon-side flag exists for: it lets trusty-search index a bake-off scratch
/// project living under an OS-temp prefix (e.g. `/var/folders/…`) instead of
/// hard-rejecting it with 400 (issue #2747 — tcode's `directory` binding).
/// When `false`, an OS-temp root (most commonly an accidental `tempfile`
/// fixture standing in for a real project in a test) is refused by the
/// daemon's `SENSITIVE_PATH_PREFIXES` denylist instead of silently registered.
/// Harmless either way for ordinary project roots (trusty-mpm worktrees,
/// checked-out repos): none of those live under `SENSITIVE_PATH_PREFIXES`, so
/// the flag is a no-op for them. It never bypasses the OTHER denylist checks
/// (credential dirs, sensitive file names, top-level home dirs) — see
/// `trusty-search::allowlist::is_denied_allowing_sensitive_path`'s doc comment
/// for exactly what stays enforced.
///
/// `skip_vector` (#5060) asks the daemon to register the index with its vector
/// lane permanently suppressed — see [`IndexOptions::skip_vector`]. It is sent
/// unconditionally (as `false` for every pre-#5060 caller) because the
/// daemon's `CreateIndexRequest` field is `Option<bool>` with `None` and
/// `Some(false)` both meaning "build the vector lane": an explicit `false` is
/// byte-for-byte equivalent to omitting it, and keeping the field present
/// makes the request shape uniform across callers.
/// Test: `create_index_request_body_respects_allow_sensitive_path_param`,
/// `create_index_request_body_sets_skip_vector`.
fn create_index_request_body(index_id: &str, root: &Path, opts: IndexOptions) -> serde_json::Value {
    serde_json::json!({
        "id": index_id,
        "root_path": root.to_string_lossy(),
        "allow_sensitive_path": opts.allow_sensitive_path,
        "skip_vector": opts.skip_vector,
    })
}

/// POST `/indexes` to find-or-create `index_id`; failures are logged, never
/// propagated (issue #1373).
///
/// Why: registration is best-effort — a daemon that is briefly unreachable, or
/// an HTTP hiccup, must NOT abort the caller. Isolating the blocking HTTP call
/// here keeps [`ensure_project_indexed`] readable and the error handling in one
/// place.
/// What: issues a short-timeout blocking `POST {base}/indexes` with body
/// `{id, root_path, allow_sensitive_path}` (built by
/// [`create_index_request_body`]) ON A DEDICATED OS THREAD. Callers are
/// frequently inside a tokio runtime; creating `reqwest::blocking`'s internal
/// runtime directly there panics with "Cannot drop a runtime in a context
/// where blocking is not allowed". Running the blocking client on a
/// freshly-spawned `std::thread` (joined here) keeps that nested runtime
/// entirely off the async worker, so the call is safe from both sync and
/// async callers. A non-2xx response or transport error is logged at
/// warn/debug and swallowed; the daemon endpoint is idempotent so re-creates
/// are harmless. The client uses a tight ~1s overall timeout (750 ms connect)
/// so the joined thread returns quickly: this call sits on a hot path and
/// must NOT stall when the daemon is slow or unreachable.
///
/// Returns [`IndexRegistration::Confirmed`] ONLY for a 2xx response (#5065
/// review): a non-2xx, a transport error, and a panicked worker thread are all
/// `NotConfirmed`. They are still logged and swallowed — the return value gives
/// the caller something honest to report, it does not make the call fallible.
/// Test: exercised via `ensure_project_indexed_returns_derived_id_when_daemon_down`
/// (daemon-down path); the live HTTP path is covered by integration use.
fn best_effort_create_index(
    base: &str,
    index_id: &str,
    root: &Path,
    opts: IndexOptions,
) -> IndexRegistration {
    let url = format!("{base}/indexes");
    let body = create_index_request_body(index_id, root, opts);
    let index_id = index_id.to_string();
    let root_display = root.display().to_string();

    let result = std::thread::spawn(move || {
        // 1s overall / 750ms connect cap: this runs synchronously on a hot
        // path, so the worst-case stall must stay small.
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(1))
            .connect_timeout(std::time::Duration::from_millis(750))
            .build()?;
        let resp = client.post(&url).json(&body).send()?;
        Ok::<reqwest::StatusCode, reqwest::Error>(resp.status())
    })
    .join();

    match result {
        Ok(Ok(status)) if status.is_success() => {
            tracing::debug!("registered trusty-search index '{index_id}' (root={root_display})");
            IndexRegistration::Confirmed
        }
        Ok(Ok(status)) => {
            tracing::warn!(
                "trusty-search index registration for '{index_id}' returned HTTP {status}"
            );
            IndexRegistration::NotConfirmed
        }
        Ok(Err(e)) => {
            tracing::warn!("trusty-search index registration for '{index_id}' failed: {e}");
            IndexRegistration::NotConfirmed
        }
        Err(_) => {
            tracing::warn!("trusty-search index registration thread for '{index_id}' panicked");
            IndexRegistration::NotConfirmed
        }
    }
}

/// Best-effort, non-blocking trigger of a trusty-search reindex for `index_id`
/// (issue #1908).
///
/// Why: [`best_effort_create_index`] only find-or-creates an EMPTY index — the
/// daemon's `POST /indexes` handler registers the id and starts a
/// future-changes file watcher but never walks the existing tree. Without an
/// explicit reindex trigger, a freshly registered index stays empty until
/// *something* changes on disk, so the very first `search`/`grep` query silently
/// returns nothing. `POST /indexes/{id}/reindex` is fire-and-forget server-side
/// — it `tokio::spawn`s the walk and returns almost instantly — so triggering it
/// here does not risk a long stall; the short dedicated-thread timeout guards
/// the (much rarer) case where even the initial HTTP round trip is slow.
/// What: on a dedicated OS thread (mirroring [`best_effort_create_index`]) with
/// a ~2s overall / 750ms connect timeout: first does a cheap `GET
/// {base}/indexes/{id}/status` freshness probe (see [`index_is_fresh`]) and
/// skips the reindex entirely when the index already has chunks and was indexed
/// within the last hour; otherwise POSTs `{base}/indexes/{id}/reindex`. A failed
/// status probe is treated as "not fresh" (fail-open toward reindexing). Every
/// outcome — skipped, triggered, non-2xx, transport error, panicked thread — is
/// logged at warn/debug and swallowed; the daemon-side reindex is itself
/// idempotent, so calling it redundantly is harmless, and the caller must never
/// block or fail because trusty-search is unreachable or slow.
/// Test: `index_is_fresh_true_when_recently_indexed_with_chunks`,
/// `index_is_fresh_false_when_no_chunks`, `index_is_fresh_false_when_stale`,
/// `index_is_fresh_false_when_last_indexed_missing_or_malformed`; the live-HTTP
/// trigger path is exercised the same way `best_effort_create_index` is
/// (daemon-down graceful path via
/// `ensure_project_indexed_returns_derived_id_when_daemon_down`).
fn best_effort_trigger_reindex(base: &str, index_id: &str) {
    let status_url = format!("{base}/indexes/{index_id}/status");
    let reindex_url = format!("{base}/indexes/{index_id}/reindex");
    let index_id = index_id.to_string();

    let result = std::thread::spawn(move || -> Result<&'static str, reqwest::Error> {
        // 2s overall / 750ms connect cap: this runs synchronously on a hot path
        // (after best_effort_create_index's own 1s budget), so the worst-case
        // added stall must stay small.
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .connect_timeout(std::time::Duration::from_millis(750))
            .build()?;

        let already_fresh = client
            .get(&status_url)
            .send()
            .ok()
            .filter(|resp| resp.status().is_success())
            .and_then(|resp| resp.json::<serde_json::Value>().ok())
            .is_some_and(|body| index_is_fresh(&body));
        if already_fresh {
            return Ok("skipped: index already fresh");
        }

        let resp = client.post(&reindex_url).send()?;
        Ok(if resp.status().is_success() {
            "triggered"
        } else {
            "reindex request returned non-2xx"
        })
    })
    .join();

    match result {
        Ok(Ok(outcome)) => {
            tracing::debug!("trusty-search reindex for '{index_id}': {outcome}");
        }
        Ok(Err(e)) => {
            tracing::warn!("trusty-search reindex trigger for '{index_id}' failed: {e}");
        }
        Err(_) => {
            tracing::warn!("trusty-search reindex trigger thread for '{index_id}' panicked");
        }
    }
}

/// Whether a `GET /indexes/{id}/status` response body represents an index
/// fresh enough that [`best_effort_trigger_reindex`] should skip reindexing
/// (issue #1908).
///
/// Why: pure predicate over the JSON body so the freshness rule is unit
/// testable without a live daemon — [`best_effort_trigger_reindex`] is
/// otherwise pure I/O. Skipping redundant reindexes avoids reindex spam on
/// every launch/run of an already-fresh workspace.
/// What: returns `true` when `chunk_count` is a positive integer AND
/// `last_indexed` parses as an RFC3339 timestamp no more than one hour in the
/// past (clock skew that makes it appear in the future is also treated as not
/// fresh, out of caution). Any missing/malformed/zero field returns `false`
/// (fail-open toward reindexing, never toward skipping).
/// Test: `index_is_fresh_true_when_recently_indexed_with_chunks`,
/// `index_is_fresh_false_when_no_chunks`, `index_is_fresh_false_when_stale`,
/// `index_is_fresh_false_when_last_indexed_missing_or_malformed`.
pub fn index_is_fresh(status: &serde_json::Value) -> bool {
    let chunk_count = status
        .get("chunk_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    if chunk_count == 0 {
        return false;
    }
    let Some(last_indexed) = status
        .get("last_indexed")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let Ok(indexed_at) = chrono::DateTime::parse_from_rfc3339(last_indexed) else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(indexed_at.with_timezone(&chrono::Utc));
    age >= chrono::Duration::zero() && age <= chrono::Duration::hours(1)
}

// Tests are in a sibling file to keep this file under the 500-SLOC production
// cap (issue #2914 split). The submodule can access private items via
// `super::` (Rust child-module rule).
#[cfg(test)]
#[path = "search_index_tests.rs"]
mod tests;
