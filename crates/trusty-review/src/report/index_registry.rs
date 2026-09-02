//! The one place the report pipeline decides WHICH trusty-search index serves a
//! checkout (#6677).
//!
//! Why: the report pass addressed an index by derived id alone —
//! `trusty_common::derive_checkout_index_id(repo_path)` — and accepted only an
//! exact id match. trusty-search holds one index per root path (409 on a second
//! registration, #2305/#2336), so a checkout already registered under any other
//! id could never be found: on 2026-09-02 the derived id was
//! `trusty-tools-4e2cf878` while the daemon served the same tree, ready and
//! 103,082 chunks, as `trusty-tools-checkout`. `--analyze` degraded to scan,
//! all ten trace lookups returned `IndexAbsent`, and the run exited 0.
//!
//! What: [`resolve_report_index`] derives the id first — a registered derived id
//! resolves exactly as it did — and only when the daemon does not hold it falls
//! back to the index whose registered `root_path` IS this checkout. The
//! root-path match is `config::index_resolver`'s, the matcher #661 gave the
//! CLI/MCP entry points, narrowed to an exact root: a parent-rooted index covers
//! the path but describes a wider tree than the one under audit (#6137).
//! [`fetch_registered_indexes`] is the read that feeds it, fail-open to an empty
//! list so an unreachable daemon lands back on the derived id.
//!
//! Both report call sites resolve through this module —
//! `analyze_adapter::enrich_with_analyze_gaps` and
//! `investigate::trace::assemble_traces` — and a second matcher in either fails
//! `neither_report_call_site_derives_its_own_index_id`.
//!
//! Test: `index_registry_tests.rs`.

use std::path::Path;

use tracing::warn;
use trusty_common::daemon_guard::DaemonAddrLayout;

use crate::config::index_resolver::{best_matching_index, canonical_source_root};
use crate::integrations::search_client::{HttpSearchClient, IndexInfo, SearchClient};

/// Derive the trusty-search/analyze index id for a local checkout path.
///
/// Why: the renderer must address the SAME index the audit indexed, and the two
/// run as separate processes with no shared state but the manifest's checkout
/// path. Until #6149 both derived the repo directory's BASENAME, so a machine
/// holding two checkouts of one repository served whichever registered first and
/// the report measured a tree it never audited. Both sides now call
/// [`trusty_common::derive_checkout_index_id`], which hashes the canonical path
/// into the id.
/// What: forwards to that function — `"<slugified basename>-<8 hex>"`, or `None`
/// for a path with no final component (e.g. `/`).
/// Test: `derive_index_id_distinguishes_same_named_checkouts`,
/// `derive_index_id_is_the_shared_derivation`.
pub fn derive_index_id(path: &Path) -> Option<String> {
    trusty_common::derive_checkout_index_id(path)
}

/// Which index the report pass addresses for one checkout, and how it got there.
///
/// Why: the three outcomes carry different operator remedies, and collapsing
/// them to `Option<String>` is what hid #6677 — a substitution and a derived hit
/// read identically at the call site, and an unresolved id read as a derived
/// one. The variant is what lets the pass log the substitution once and name the
/// derived id when nothing covers the path.
/// What: `Derived` is the daemon holding the derived id; `Substituted` is an
/// index registered at this checkout's `root_path` under another id;
/// `Unresolved` is neither, and still carries the derived id so the existing
/// not-indexed fallback runs unchanged.
/// Test: `index_registry_tests.rs::{a_registered_derived_id_is_used_as_is,
/// a_root_path_match_substitutes_for_an_unregistered_derived_id,
/// no_match_keeps_the_derived_id}`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReportIndex {
    /// The daemon holds the derived id; nothing was substituted.
    Derived(String),
    /// The derived id is absent; this index is registered at the checkout root.
    Substituted {
        /// The registered index id the pass will address.
        id: String,
        /// What the checkout path derives to, when it derives to anything.
        derived: Option<String>,
    },
    /// Neither the derived id nor any registered `root_path` matched.
    Unresolved {
        /// What the checkout path derives to, when it derives to anything.
        derived: Option<String>,
    },
}

impl ReportIndex {
    /// The index id to address, or `None` when the path derives to nothing.
    ///
    /// `Unresolved` still yields the derived id: the daemon gets asked, answers
    /// that it does not hold it, and the caller's existing fallback runs. That
    /// branch is unchanged by #6677.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Derived(id) | Self::Substituted { id, .. } => Some(id.as_str()),
            Self::Unresolved { derived } => derived.as_deref(),
        }
    }

    /// [`Self::id`], owned.
    #[must_use]
    pub fn into_id(self) -> Option<String> {
        match self {
            Self::Derived(id) | Self::Substituted { id, .. } => Some(id),
            Self::Unresolved { derived } => derived,
        }
    }
}

/// Resolve the index the report pass should address for `repo_path`.
///
/// Why/What: see the module doc. `indexes` is the daemon's registry as
/// [`fetch_registered_indexes`] returns it; an empty list resolves to the
/// derived id, which is the behaviour that predates #6677.
/// Test: `index_registry_tests.rs` — one test per variant, plus
/// `a_parent_rooted_index_is_not_substituted`.
pub fn resolve_report_index(repo_path: &Path, indexes: &[IndexInfo]) -> ReportIndex {
    let derived = derive_index_id(repo_path);
    if let Some(id) = derived.as_ref()
        && indexes.iter().any(|i| &i.id == id)
    {
        return ReportIndex::Derived(id.clone());
    }
    match index_at_root(indexes, repo_path) {
        Some(id) => {
            warn!(
                index_id = %id,
                derived = derived.as_deref().unwrap_or("<none>"),
                repo_path = %repo_path.display(),
                "report index resolution: the derived index id is not registered; \
                 addressing the index registered at this checkout's root_path instead"
            );
            ReportIndex::Substituted { id, derived }
        }
        None => {
            // #6677 review: the cause rides on this line, so an operator reading
            // one warning knows which remedy applies.
            warn!(
                derived = derived.as_deref().unwrap_or("<none>"),
                repo_path = %repo_path.display(),
                registry = registry_state(indexes),
                registered = indexes.len(),
                "report index resolution: no registered index matches this checkout; \
                 registry=empty means the daemon answered nothing — it is down, or it \
                 holds no index — and registry=populated means it holds indexes but \
                 none rooted here; the analyze fetch falls back to scan and traces \
                 record no anchor"
            );
            ReportIndex::Unresolved { derived }
        }
    }
}

/// Which of the two `Unresolved` causes produced an empty match.
///
/// Why: the warning had a count and no cause, so a reader had to correlate it
/// with `fetch_registered_indexes`'s own line to tell "the daemon told us
/// nothing" from "the daemon holds indexes, none of them this tree". The two
/// have different remedies — start or reach the daemon, versus index this
/// checkout — and they now ride on the one line.
/// What: `"empty"` for a registry with no entries (an unreachable daemon reads
/// as one, fail-open), `"populated"` otherwise.
/// Test: `index_registry_tests::the_unresolved_warning_names_which_cause`.
fn registry_state(indexes: &[IndexInfo]) -> &'static str {
    if indexes.is_empty() {
        "empty"
    } else {
        "populated"
    }
}

/// The registered index whose root IS this checkout.
///
/// Why: #661's `best_matching_index` picks the longest registered `root_path`
/// COVERING a directory, which is right for `--source-root` and too loose here —
/// a parent-rooted index answers about files outside the audited tree, the
/// stale-scope failure #6137 exists to catch. Reusing that matcher and then
/// demanding its winner BE the checkout keeps one root-path matcher in the crate
/// and one report-side rule about which index describes this tree.
/// What: `Some(id)` when the winning index's canonicalised `root_path` equals
/// the canonicalised `repo_path`; `None` otherwise. `canonical_source_root`
/// falls back to the raw path for a directory that no longer exists, so a
/// registry entry naming a deleted tree matches only a `repo_path` whose raw
/// string is the same — a real checkout never collides with one.
/// Test: `index_registry_tests.rs::{a_root_path_match_substitutes_for_an_
/// unregistered_derived_id, a_parent_rooted_index_is_not_substituted,
/// a_registry_root_that_no_longer_exists_matches_only_its_own_raw_path}`.
fn index_at_root(indexes: &[IndexInfo], repo_path: &Path) -> Option<String> {
    let root = canonical_source_root(repo_path);
    let id = best_matching_index(indexes, &root)?;
    let info = indexes.iter().find(|i| i.id == id)?;
    let registered = canonical_source_root(Path::new(info.root_path.as_ref()?));
    (registered == root).then_some(id)
}

/// Read one trusty-search daemon's registered indexes, fail-open.
///
/// Why: resolution needs `root_path`, which only `GET /indexes?details=true`
/// carries. The request is [`HttpSearchClient::list_indexes`] — the crate's one
/// implementation of it — pointed at an explicit base URL rather than
/// `ReviewConfig::search_url`, because the report pass must address the daemon
/// the audit indexed.
/// What: the registry, or an empty list when the client will not build or the
/// daemon does not answer. Empty resolves to the derived id, so a down daemon
/// degrades exactly as it did before #6677.
/// Test: `index_registry_tests.rs::an_unreachable_daemon_reads_an_empty_registry`.
pub async fn fetch_registered_indexes(base_url: &str) -> Vec<IndexInfo> {
    let client = match HttpSearchClient::new(base_url) {
        Ok(c) => c,
        Err(e) => {
            warn!(base_url, error = %e, "report index resolution: no trusty-search client");
            return Vec::new();
        }
    };
    match client.list_indexes().await {
        Ok(indexes) => indexes,
        Err(e) => {
            warn!(base_url, error = %e, "report index resolution: index list unavailable");
            Vec::new()
        }
    }
}

/// [`fetch_registered_indexes`] against the daemon `trusty-search` advertises.
///
/// Why: a hard-coded `127.0.0.1:7878` misses an auto-ported daemon and every
/// `TRUSTY_DATA_DIR`-isolated one — the same resolution `HttpTraceSource` makes.
/// Test: the read is `an_unreachable_daemon_reads_an_empty_registry`; the
/// address resolution is `DaemonAddrLayout`'s, covered by
/// `the_shared_search_layout_is_the_one_being_resolved`.
pub async fn registered_indexes() -> Vec<IndexInfo> {
    fetch_registered_indexes(&DaemonAddrLayout::TRUSTY_SEARCH.resolve_base_url()).await
}

#[cfg(test)]
#[path = "index_registry_tests.rs"]
mod tests;
