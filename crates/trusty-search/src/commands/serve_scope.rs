//! Working-directory index scoping for an MCP `serve` session (#5264).
//!
//! Why: ADR-0042 made the MCP registration argless on the theory that
//! `TRUSTY_INDEX` arrives via the environment — `tm launch` injects it into
//! each `claude` process. Codex is not launched by tm, so nothing injects it,
//! and `trusty-search setup` writes `args = ["serve"]` with no `--project`, so
//! every session it creates was unpinned: each project-scoped tool call had to
//! carry an explicit `index_id` or fail. Writing a static `env` pin into
//! `~/.codex/config.toml` would bind Codex to one project permanently, so the
//! index is instead resolved from the serving process's working directory and
//! the choice is reported.
//!
//! What: derives a candidate index id from the working directory using the
//! same `resolve_project_root` + `derive_index_id` pair in trusty-common that
//! trusty-mpm's register-and-pin uses (#1373 — the two must agree or a session
//! pins one id while querying another), then CONFIRMS that candidate against
//! the daemon's own index list before pinning. A candidate that the daemon
//! does not serve, or serves from a different root, is refused: the session
//! stays unpinned and says why. That refusal is the point — `derive_index_id`
//! returns a bare path basename, so two unrelated projects both named `api`
//! derive the same id, and pinning on the id alone would silently serve one
//! project's results to the other.
//!
//! #6864: that refusal was too broad. Two checkouts of one repository collide on
//! the derived id by construction, and the daemon holds the second under a
//! distinct id — `trusty-tools-checkout` beside `trusty-tools`. So before
//! refusing, the already-fetched entries are scanned for one whose `root_path`
//! IS this working directory, and that index is pinned instead. No extra request
//! is made; only the entries the confirmation already read are re-examined.
//!
//! The pin is computed once, at startup. `serve` is a long-lived stdio process
//! whose working directory cannot change after exec, so re-resolving per call
//! would re-read an input that never varies.
//!
//! Test: `serve_scope_tests.rs`.

use std::path::{Path, PathBuf};

use trusty_search::service::daemon_client::DaemonClient;
use trusty_search::service::rpc::reads::METHOD_INDEXES_LIST;

/// Where a session's index pin came from.
///
/// Why (#5264): the session must report not just WHICH index it pinned but on
/// what basis, so an operator seeing results from an unexpected project can
/// tell an explicit flag from a working-directory guess without re-deriving
/// the precedence by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PinSource {
    /// An explicit `--index`, or the `TRUSTY_INDEX` value clap folds into that
    /// same arg (#4181).
    Flag,
    /// Derived from an explicit `--project <path>`.
    Project,
    /// #5264: derived from the working directory and confirmed against the
    /// daemon's index list.
    WorkingDir,
    /// #6864: the working directory's DERIVED id names another tree (or nothing),
    /// but the daemon serves this exact tree under a different id. Reported
    /// apart from [`PinSource::WorkingDir`] so the startup line says the id was
    /// substituted rather than derived.
    WorkingDirRootMatch,
}

impl PinSource {
    /// Human-readable origin, used in the startup report.
    fn label(self) -> &'static str {
        match self {
            PinSource::Flag => "--index / TRUSTY_INDEX",
            PinSource::Project => "--project",
            PinSource::WorkingDir => "working directory",
            PinSource::WorkingDirRootMatch => "working directory root_path match",
        }
    }
}

/// An index pin together with the source that decided it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinChoice {
    pub index_id: String,
    pub source: PinSource,
}

impl PinChoice {
    /// One-line startup report naming both the index and its origin.
    pub(crate) fn report(&self) -> String {
        format!(
            "MCP session pinned to index {} (from {})",
            self.index_id,
            self.source.label()
        )
    }
}

/// Resolve the index an MCP session pins to from the explicit flags alone.
///
/// Why: precedence here decides which index every tool call in the session
/// defaults to, and getting it wrong is exactly the wrong-index defect #1373
/// fixed. Holding it in one function keeps `main`'s match arm thin and lets the
/// precedence be asserted directly rather than re-implemented in a test.
/// What: an index from any source wins; otherwise the id is derived from
/// `--project` the same way the CLI and trusty-mpm derive it; otherwise `None`
/// and the caller may fall back to the working directory (#5264). `index`
/// carries the `TRUSTY_INDEX` environment value as well as the explicit flag —
/// clap resolves that precedence at parse time, flag over environment.
/// Test: `commands::serve::index_env_tests`.
pub(crate) fn resolve_pinned_index(
    index: Option<String>,
    project: Option<String>,
) -> Option<PinChoice> {
    if let Some(id) = index {
        return Some(PinChoice {
            index_id: id,
            source: PinSource::Flag,
        });
    }
    project.map(|p| {
        let root = trusty_common::resolve_project_root(&PathBuf::from(&p));
        PinChoice {
            index_id: trusty_common::derive_index_id(&root),
            source: PinSource::Project,
        }
    })
}

// #8229: the confirmation half lives in the library so `search_health` can
// share it; re-exported so this module's callers and tests are unchanged.
pub(crate) use trusty_search::mcp::cwd_scope::{
    confirm_candidate, derive_cwd_candidate, parse_index_entries, Confirmation, CwdCandidate,
    DaemonIndex,
};

/// The outcome of the working-directory tier: either a pin, or a refusal that
/// carries the reason to report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutoPin {
    Pinned(PinChoice),
    Unpinned { reason: String },
}

impl AutoPin {
    /// The pin, when there is one.
    pub(crate) fn choice(&self) -> Option<&PinChoice> {
        match self {
            AutoPin::Pinned(c) => Some(c),
            AutoPin::Unpinned { .. } => None,
        }
    }

    /// The line to print at startup.
    pub(crate) fn report(&self) -> String {
        match self {
            AutoPin::Pinned(c) => c.report(),
            AutoPin::Unpinned { reason } => reason.clone(),
        }
    }
}

/// Turn a confirmation verdict into a pin decision and its startup report.
///
/// Why (#5264): an unindexed or ambiguous working directory must neither pin
/// silently to something wrong nor hard-fail a session that can still serve
/// explicit `index_id` calls. Staying unpinned is exactly the behavior a bare
/// `serve` had before this tier existed, so a refusal costs nothing that
/// previously worked — it only declines to guess.
/// What: `Confirmed` pins with [`PinSource::WorkingDir`] and — since #6864 —
/// `ServedByAnotherId` pins the substituted id with
/// [`PinSource::WorkingDirRootMatch`], so the startup line names the index that
/// was actually pinned and says it came from a root match rather than the
/// basename. Every other verdict leaves the session unpinned and names the
/// reason, the derived id, and the remedy.
/// Test: `decide_pins_on_confirmation`, `decide_refuses_on_root_mismatch`,
/// `decide_refuses_when_not_served`, `decide_pins_the_substituted_index`.
pub(crate) fn decide_auto_pin(candidate: &CwdCandidate, verdict: Confirmation) -> AutoPin {
    let root = candidate.project_root.display();
    let id = &candidate.index_id;
    match verdict {
        Confirmation::Confirmed => AutoPin::Pinned(PinChoice {
            index_id: candidate.index_id.clone(),
            source: PinSource::WorkingDir,
        }),
        // #6864: pin the index registered at this tree, not the derived name.
        Confirmation::ServedByAnotherId { index_id } => {
            tracing::info!(
                "working directory {root} derives index {id}, which does not name this \
                 tree; the daemon serves it as {index_id} and that is what this session \
                 pins (#6864)"
            );
            AutoPin::Pinned(PinChoice {
                index_id,
                source: PinSource::WorkingDirRootMatch,
            })
        }
        Confirmation::RootMismatch { serving_root } => AutoPin::Unpinned {
            reason: format!(
                "MCP session UNPINNED — working directory {root} derives index {id}, \
                 but the daemon serves that id from {}. Refusing to pin a different \
                 project; pass index_id explicitly.",
                serving_root.display()
            ),
        },
        Confirmation::NotServed => AutoPin::Unpinned {
            reason: format!(
                "MCP session UNPINNED — working directory {root} derives index {id}, \
                 which this daemon does not serve. Pass index_id explicitly, or index \
                 this project first."
            ),
        },
        Confirmation::RootUnknown => AutoPin::Unpinned {
            reason: format!(
                "MCP session UNPINNED — the daemon serves index {id} but reported no \
                 root path to confirm it against {root}. Pass index_id explicitly."
            ),
        },
    }
}

/// Budget for the startup pin confirmation — the 5 s request timeout the
/// HTTP probe had (#9168).
pub(crate) const PIN_CONFIRM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Fetch the daemon's index list over its socket (#9168).
///
/// Why: the startup pin must be confirmed against the SAME daemon the session
/// will query, and the session reaches it only through `daemon`.
/// [`PIN_CONFIRM_TIMEOUT`] bounds the wait, so a wedged daemon cannot hang
/// `serve` startup.
/// What: `search.indexes.list` with `details: true`, parsed by
/// [`parse_index_entries`]. Errors propagate so the caller can report the
/// session as unpinned rather than guessing.
/// Test: `fetch_reads_entries_from_a_live_server` binds a scratch socket.
pub(crate) async fn fetch_index_entries(daemon: &DaemonClient) -> anyhow::Result<Vec<DaemonIndex>> {
    let body = daemon
        .call_with_timeout(
            METHOD_INDEXES_LIST,
            serde_json::json!({ "details": true }),
            PIN_CONFIRM_TIMEOUT,
        )
        .await?;
    Ok(parse_index_entries(&body))
}

/// Resolve and confirm a working-directory pin for a session with no explicit
/// flag (#5264).
///
/// Why: this is the whole working-directory tier, in the one place `serve`
/// calls it — derivation, daemon confirmation, and the refusal-with-reason are
/// a single decision and are kept together so no caller can perform half of it.
/// What: returns `None` when the directory yields no candidate at all (an empty
/// derived id), an [`AutoPin`] otherwise. A daemon that cannot be listed is a
/// refusal, not a pin: an unconfirmable guess is exactly what must not be
/// presented as a confirmed session.
/// Test: covered through its parts — `derive_cwd_candidate`,
/// `confirm_candidate`, and `decide_auto_pin` each have direct tests, and
/// `fetch_reads_entries_from_a_live_server` covers the transport.
pub(crate) async fn auto_pin_from_cwd(daemon: &DaemonClient, cwd: &Path) -> Option<AutoPin> {
    let candidate = derive_cwd_candidate(cwd)?;
    match fetch_index_entries(daemon).await {
        Ok(entries) => Some(decide_auto_pin(
            &candidate,
            confirm_candidate(&candidate, &entries),
        )),
        Err(e) => Some(AutoPin::Unpinned {
            reason: format!(
                "MCP session UNPINNED — could not list indexes to confirm the working \
                 directory's index ({e:#}). Pass index_id explicitly."
            ),
        }),
    }
}
