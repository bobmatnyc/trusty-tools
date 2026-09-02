//! Indexing each audited repository in trusty-search before the report is
//! rendered (#5670, DOC-67 §6 "Still open").
//!
//! Why: `tga audit` always renders with `trusty-review report --analyze`, and
//! that renderer fetches nothing for a repository trusty-search does not serve.
//! Its membership check — `HttpAnalyzeMetricsSource::index_served`, a `GET
//! /indexes` — misses for a repository nobody indexed, and the miss is
//! fail-open: `AnalyzeGap::NotIndexed`, one gap line, exit 0, and the findings
//! table, the complexity distribution and the health factors all render empty.
//! Nothing in the audit path indexed anything, so an unindexed repository
//! produced a hollow report over a clean exit. Starting the analyze daemon
//! ([`super::analyze`]) closed link 3 of the prerequisite chain; this module
//! closes link 2, the per-repository index.
//!
//! What: [`ensure_repositories_indexed`], the `trusty-search` binary resolution
//! rule it applies, and the per-repository [`RepoIndexOutcome`] it returns.
//! Indexing runs as a subprocess — the sanctioned route, since DOC-67 §5
//! forbids a second HTTP-client implementation against the daemon, and the same
//! house pattern [`super::review`] uses to reach `trusty-review`.
//!
//! ## This one is fail-open, and the analyze preflight is not
//!
//! [`super::analyze`] refuses the whole run when the daemon will not start,
//! because that failure takes every repository's analysis with it. A repository
//! that will not index takes only its own, and DOC-67 §9 fixes what happens to a
//! per-repository failure: the repository is excluded, named in Gaps & Caveats,
//! and the run continues. A one-shot sweep over an org cannot spend its one shot
//! on the first repository with a broken checkout. So every failure here becomes
//! a gap line ([`super::index_gap_lines`]); none of them aborts the sweep, and
//! none of them changes the exit status.
//!
//! ## The index id is a cross-process contract
//!
//! trusty-review derives the id it looks up from the checkout path written into
//! `manifest.toml`, which is the only thing the renderer reads. [`index_id_for`]
//! derives it from the same value through the same function
//! ([`trusty_common::derive_checkout_index_id`], #6149) — an id derived any
//! other way indexes every repository under a name nobody ever queries, leaving
//! the reports exactly as hollow as before while looking fixed.
//!
//! ## What the wait does, and does not, guarantee
//!
//! `trusty-search index` is invoked with its default foreground wait, which
//! detaches after the index stops making progress rather than blocking forever.
//! That bounds an unattended run (DOC-67 §2) at the cost of the strongest
//! claim: a detached index is registered and answers the renderer's membership
//! check while still filling. Its sections then render from whatever had been
//! embedded — thinner than a completed pass, and not distinguishable from one
//! here. Waiting forever instead would trade that for a sweep that can hang.
//! Test: `super::tests`; `super::real_binary_tests` for the CLI facts this rests
//! on.
//!
//! # Spec References
//! - [`SPEC-TGAUDIT-06~draft`](docs/specs/DOC-67-tga-audit-mode.md#SPEC-TGAUDIT-06~draft)
//! - [`SPEC-TGAUDIT-09~draft`](docs/specs/DOC-67-tga-audit-mode.md#SPEC-TGAUDIT-09~draft)

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use crate::report::dd_manifest::DdRepositoryEntry;

/// Environment variable that overrides the `trusty-search` binary path.
///
/// Why: the same override-then-PATH resolution [`super::review::ENV_REVIEW_BIN`]
/// and [`super::analyze::ENV_ANALYZE_BIN`] give the other two sibling binaries,
/// so an engagement that pins its tools (`trusty-audit` exports pinned paths
/// onto every `tga audit` child) can pin this one the same way.
pub const ENV_SEARCH_BIN: &str = "TRUSTY_SEARCH_BIN";

/// Default binary name searched on PATH.
pub const DEFAULT_SEARCH_BIN: &str = "trusty-search";

/// What became of one repository's index.
///
/// Why: the caller needs the distinction between "this repository is now
/// analyzable" and "this repository's report sections are not assessed" — the
/// second is a Gaps & Caveats line, the first is silence.
/// What: served before the audit started, indexed by this run, or failed with
/// the reason.
/// Test: `super::tests::an_unindexed_repository_is_indexed_before_the_render`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RepoIndexStatus {
    /// trusty-search already served this index; nothing was spawned.
    AlreadyServed,
    /// This run indexed the repository.
    Indexed,
    /// The repository could not be indexed, for the reason carried here.
    ///
    /// The text is the child's, so it is redacted and excerpted on its way into
    /// the report — see [`super::index_gap_lines`].
    Failed(String),
}

/// One repository's indexing result.
///
/// Why: the gap lines name repositories the way the report does, so the outcome
/// carries the manifest's display name rather than a path the reader has never
/// seen.
/// What: the display name, the index id the renderer will look up (`None` when
/// none could be derived), and the status.
/// Test: `super::tests::one_repository_that_fails_to_index_does_not_stop_the_others`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RepoIndexOutcome {
    /// Display name, as `manifest.toml` and the report use it.
    pub repo: String,
    /// The trusty-search index id, or `None` when the path yielded no basename.
    pub index_id: Option<String>,
    /// What happened.
    pub status: RepoIndexStatus,
}

impl RepoIndexOutcome {
    /// Whether this repository is unassessed and therefore owes a gap line.
    pub fn failed(&self) -> bool {
        matches!(self.status, RepoIndexStatus::Failed(_))
    }
}

/// The `trusty-search` binary this process will invoke.
///
/// Why/What: [`ENV_SEARCH_BIN`] when set to a non-empty value, else
/// [`DEFAULT_SEARCH_BIN`] resolved on PATH by the OS. Reading the variable is all
/// this does; the rule lives in [`binary_from_override`] so tests never call
/// `std::env::set_var`, which is `unsafe` in edition 2024 and unsound under the
/// parallel harness (#5308 review).
/// Test: `super::tests::search_binary_resolution_prefers_the_env_override`.
pub fn resolve_search_binary() -> String {
    binary_from_override(std::env::var(ENV_SEARCH_BIN).ok().as_deref())
}

/// The resolution rule itself: an override wins unless it is empty.
///
/// Why/What/Test: see [`resolve_search_binary`].
pub(super) fn binary_from_override(override_value: Option<&str>) -> String {
    override_value
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_SEARCH_BIN)
        .to_string()
}

/// The trusty-search index id for a checkout path.
///
/// Why: this must agree with `trusty_review::report::index_registry::
/// derive_index_id`, which is what the renderer looks the index up by. Until
/// #6149 both were the checkout BASENAME, copied rather than called — and two
/// checkouts of one repository therefore collided on one id, so the sweep
/// indexed one tree and the renderer read another. Both now call
/// [`trusty_common::derive_checkout_index_id`], so the agreement is a call.
/// What: `"<slugified basename>-<8 hex over the canonical path>"` — `None` for a
/// path with no final component, e.g. `/`.
/// Test: `super::tests::{the_index_id_distinguishes_two_checkouts_of_one_repo,
/// index_ids_match_the_manifest_paths_the_renderer_reads}`.
pub fn index_id_for(path: &Path) -> Option<String> {
    trusty_common::derive_checkout_index_id(path)
}

/// The membership probe's argument vector.
///
/// Why: `index-status <id>` asks the daemon for `/indexes/<id>/status` and exits
/// non-zero on its 404, which is the same question the renderer's `index_served`
/// asks — so a repository that would satisfy the renderer is not re-indexed.
/// Building the vector in a pure function is what lets a test assert it without
/// spawning anything, as [`super::review::report_args`] does.
/// Test: `super::tests::the_index_invocation_names_the_path_and_the_id` and
/// `super::tests::an_already_indexed_repository_is_not_reindexed` — the first
/// asserts the vector without spawning, the second that a served repository is
/// probed with `index-status <id>` and nothing else is spawned.
pub(super) fn probe_args(index_id: &str) -> Vec<OsString> {
    vec!["index-status".into(), index_id.into()]
}

/// The indexing invocation's argument vector.
///
/// Why: `--name` is what binds the index to the id the renderer will look up;
/// without it trusty-search would name the index after whatever directory the
/// audit happens to be running from.
/// What: `index <path> --name <index_id>`.
/// Test: `super::tests::the_index_invocation_names_the_path_and_the_id`.
pub(super) fn index_args(path: &Path, index_id: &str) -> Vec<OsString> {
    vec![
        "index".into(),
        path.as_os_str().to_owned(),
        "--name".into(),
        index_id.into(),
    ]
}

/// Index every audited repository that trusty-search does not already serve.
///
/// Why/What: see the module docs. Resolves the binary from the environment once,
/// at this entry point, and delegates to [`ensure_repositories_indexed_with`].
/// Takes the manifest's own repository entries rather than the config so the
/// index ids are derived from the exact paths the renderer will read.
///
/// Never returns an error: a failure is a per-repository gap, not a run
/// failure (DOC-67 §9).
///
/// Test: `super::tests::an_unindexed_repository_is_indexed_before_the_render`.
pub async fn ensure_repositories_indexed(repos: &[DdRepositoryEntry]) -> Vec<RepoIndexOutcome> {
    ensure_repositories_indexed_with(resolve_search_binary(), repos).await
}

/// [`ensure_repositories_indexed`] with the binary already resolved.
///
/// Why: taking it as a parameter is what lets a test drive the whole
/// probe-and-index path against a stub executable without touching the process
/// environment (#5308 review).
/// What: runs the repositories one at a time on the blocking pool — an indexing
/// pass is CPU- and I/O-heavy at the daemon, and running an org's worth of them
/// at once would make each slower rather than the set faster. Order is the
/// manifest's, so two runs over the same state produce the same gap lines.
/// Test: `super::tests::{an_already_indexed_repository_is_not_reindexed,
/// one_repository_that_fails_to_index_does_not_stop_the_others}`.
pub(super) async fn ensure_repositories_indexed_with(
    binary: String,
    repos: &[DdRepositoryEntry],
) -> Vec<RepoIndexOutcome> {
    let owned: Vec<DdRepositoryEntry> = repos.to_vec();
    let task = tokio::task::spawn_blocking(move || {
        owned
            .iter()
            .map(|entry| ensure_one(&binary, entry))
            .collect::<Vec<_>>()
    });

    match task.await {
        Ok(outcomes) => outcomes,
        // A cancelled blocking task leaves every repository unindexed and
        // unexamined. Reporting that as a gap per repository keeps the one
        // invariant this module has: nothing goes unassessed without being named.
        Err(e) => repos
            .iter()
            .map(|entry| RepoIndexOutcome {
                repo: entry.name.clone(),
                index_id: index_id_for(&entry.path),
                status: RepoIndexStatus::Failed(format!("the indexing task did not complete: {e}")),
            })
            .collect(),
    }
}

/// Probe one repository, index it if the daemon does not already serve it.
///
/// Why: the probe is what keeps an audit over an already-indexed org cheap — a
/// re-index of an unchanged repository is wasted embedding time the one-shot run
/// does not have to spend.
/// What: derives the id, probes, and on a miss spawns the indexing pass. Every
/// outcome is a value; nothing here panics or propagates.
/// Test: `super::tests::an_already_indexed_repository_is_not_reindexed`.
fn ensure_one(binary: &str, entry: &DdRepositoryEntry) -> RepoIndexOutcome {
    let outcome = |index_id: Option<String>, status: RepoIndexStatus| RepoIndexOutcome {
        repo: entry.name.clone(),
        index_id,
        status,
    };

    let index_id = match index_id_for(&entry.path) {
        Some(id) => id,
        None => {
            return report(outcome(
                None,
                RepoIndexStatus::Failed(format!(
                    "the checkout path `{}` has no directory name, so trusty-search and \
                     trusty-review cannot agree on an index id for it",
                    entry.path.display()
                )),
            ))
        }
    };

    if served(binary, &index_id) {
        return outcome(Some(index_id), RepoIndexStatus::AlreadyServed);
    }

    eprintln!(
        "[tga audit] indexing `{}` into trusty-search index `{index_id}`…",
        entry.path.display()
    );
    let status = match run(binary, index_args(&entry.path, &index_id)) {
        Ok(output) if output.status.success() => RepoIndexStatus::Indexed,
        Ok(output) => RepoIndexStatus::Failed(format!(
            "`{binary} index` exited with {}: {}",
            output
                .status
                .code()
                .map_or_else(|| "a signal".to_string(), |c| format!("code {c}")),
            last_line(&String::from_utf8_lossy(&output.stderr)).unwrap_or("no output on stderr"),
        )),
        Err(e) => RepoIndexStatus::Failed(spawn_failure(binary, &e)),
    };
    report(outcome(Some(index_id), status))
}

/// Echo a failed outcome to stderr, then hand it back unchanged.
///
/// Why: the gap line reaches the operator only once the report exists, and the
/// audit can run for many minutes after this point. The run log is where someone
/// watching finds out — the same reason [`super::analyze`] narrates its spawn.
fn report(outcome: RepoIndexOutcome) -> RepoIndexOutcome {
    if let RepoIndexStatus::Failed(reason) = &outcome.status {
        eprintln!(
            "[tga audit] `{}` was not indexed ({reason}); its findings, complexity and health \
             factors will be reported as not assessed",
            outcome.repo
        );
    }
    outcome
}

/// Whether trusty-search already serves `index_id`.
///
/// Why: exit status is the whole answer, so nothing here parses another crate's
/// JSON. A probe that cannot run at all (no binary, no daemon) answers "no",
/// which sends the repository down the indexing path — where the same fault
/// produces a named gap instead of a silent skip.
/// Test: `super::tests::an_already_indexed_repository_is_not_reindexed`.
fn served(binary: &str, index_id: &str) -> bool {
    run(binary, probe_args(index_id)).is_ok_and(|output| output.status.success())
}

/// Run `binary` with `args`, capturing both streams.
///
/// `Command::output` gives the child a null stdin, which is what keeps DOC-67
/// §2's no-prompt rule true of the children as well as of the sweep.
fn run(binary: &str, args: Vec<OsString>) -> std::io::Result<std::process::Output> {
    Command::new(binary).args(args).output()
}

/// The operator-facing text for a child that would not start.
///
/// Why: "not installed" is a one-line fix and every other spawn failure is not,
/// so the two do not share a message. The remedy names both ways to supply the
/// binary, since an engagement that pins its tools uses the override rather than
/// PATH.
/// Test: `super::tests::a_missing_search_binary_is_named_and_the_run_continues`.
fn spawn_failure(binary: &str, error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound {
        // The remedy leads and the path trails, because this text is excerpted
        // into the report at a fixed character budget (`super::gaps`) and a long
        // pinned-tool path would otherwise push the fix past the cut.
        return format!(
            "the trusty-search binary was not found — `cargo install trusty-search`, or set \
             {ENV_SEARCH_BIN} to its full path (tried `{binary}`)"
        );
    }
    format!("failed to run `{binary}`: {error}")
}

/// The last non-blank line of a child's output.
///
/// Why: a failing `trusty-search index` prints its cause last, and the lines
/// above it are progress the report's reader has no use for.
fn last_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).rfind(|l| !l.is_empty())
}
