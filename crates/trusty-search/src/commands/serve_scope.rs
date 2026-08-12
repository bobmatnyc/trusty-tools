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
//! The pin is computed once, at startup. `serve` is a long-lived stdio process
//! whose working directory cannot change after exec, so re-resolving per call
//! would re-read an input that never varies.
//!
//! Test: `serve_scope_tests.rs`.

use std::path::{Path, PathBuf};

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
}

impl PinSource {
    /// Human-readable origin, used in the startup report.
    fn label(self) -> &'static str {
        match self {
            PinSource::Flag => "--index / TRUSTY_INDEX",
            PinSource::Project => "--project",
            PinSource::WorkingDir => "working directory",
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

/// The index id a working directory maps to, plus the root it was derived from.
///
/// The root is carried alongside the id because the id alone cannot be
/// verified — see [`confirm_candidate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CwdCandidate {
    pub index_id: String,
    pub project_root: PathBuf,
}

/// Derive the candidate index for a working directory.
///
/// Why (#5264): this is the tier that makes a bare `trusty-search serve` — the
/// registration `setup` writes — usable in a multi-project client, without
/// baking any one project into the config file.
/// What: routes through the trusty-common pair `resolve_project_root` (nearest
/// enclosing `.git`, else the directory itself) and `derive_index_id` (the path
/// basename, verbatim), so the id matches what trusty-mpm would register for
/// the same tree. Returns `None` when the derivation yields an empty id, which
/// `derive_index_id` documents for a filesystem-root path — an empty index id
/// addresses nothing and must never become a pin.
///
/// This deliberately does NOT reuse trusty-search's own `detect_project`, which
/// adds a `.trusty-search` marker-file tier and so can resolve a DIFFERENT root
/// than trusty-mpm would for the same tree. Pin derivation must match the
/// register-and-pin path (#1373), so it uses the trusty-common pair that path
/// uses. Consolidating the two would mean moving the marker tier into
/// trusty-common and changing what every existing caller resolves — a
/// behavioral change to a shared crate, not a refactor, so it is left alone.
/// Test: `cwd_candidate_uses_git_root`, `cwd_candidate_falls_back_to_basename`,
/// `cwd_candidate_none_for_root_path`.
pub(crate) fn derive_cwd_candidate(cwd: &Path) -> Option<CwdCandidate> {
    let project_root = trusty_common::resolve_project_root(cwd);
    let index_id = trusty_common::derive_index_id(&project_root);
    if index_id.is_empty() {
        return None;
    }
    Some(CwdCandidate {
        index_id,
        project_root,
    })
}

/// One index as the daemon reports it on `GET /indexes?details=true`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DaemonIndex {
    pub id: String,
    /// `None` when the daemon could not render the root as UTF-8.
    pub root_path: Option<PathBuf>,
}

/// The verdict on whether a working-directory candidate may be pinned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Confirmation {
    /// The daemon serves this id from this exact root — safe to pin.
    Confirmed,
    /// The daemon serves the id, but from a different project root.
    RootMismatch { serving_root: PathBuf },
    /// The daemon does not serve the id at all.
    NotServed,
    /// The daemon serves the id but reported no usable root to compare.
    RootUnknown,
}

/// Compare two roots, tolerating symlinks and platform path aliasing.
///
/// Why: on macOS the same directory is reachable as both `/tmp/x` and
/// `/private/tmp/x`, and a git worktree is routinely reached through a
/// symlink. A byte comparison would call those a mismatch and refuse a pin
/// that is in fact correct.
/// What: exact comparison first (cheap, and the only thing that works when a
/// root no longer exists on disk), then a `canonicalize` comparison. A
/// canonicalize failure on either side falls back to the already-failed exact
/// comparison, so a since-deleted root reads as a mismatch rather than a match.
fn same_root(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Decide whether the daemon's index list confirms a working-directory
/// candidate.
///
/// Why (#5264): the fail-open shape this guards against is a session that looks
/// healthy while serving another project's code. Matching on the derived id
/// alone would do exactly that, because `derive_index_id` is a bare path
/// basename: two checkouts named `api` collide. Comparing the daemon's
/// `root_path` against the root the id was derived from turns that collision
/// into a refusal.
/// What: finds the entry with a matching id, then requires its root to be the
/// same directory. Every other outcome is a distinct refusal reason so the
/// report can say which one happened.
/// Test: `confirm_accepts_matching_root`, `confirm_rejects_colliding_basename`,
/// `confirm_rejects_unknown_index`, `confirm_rejects_null_root`.
pub(crate) fn confirm_candidate(candidate: &CwdCandidate, entries: &[DaemonIndex]) -> Confirmation {
    let Some(entry) = entries.iter().find(|e| e.id == candidate.index_id) else {
        return Confirmation::NotServed;
    };
    let Some(root) = entry.root_path.as_ref() else {
        return Confirmation::RootUnknown;
    };
    if same_root(root, &candidate.project_root) {
        Confirmation::Confirmed
    } else {
        Confirmation::RootMismatch {
            serving_root: root.clone(),
        }
    }
}

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
/// What: `Confirmed` pins with [`PinSource::WorkingDir`]; every other verdict
/// leaves the session unpinned and names the reason, the derived id, and the
/// remedy.
/// Test: `decide_pins_on_confirmation`, `decide_refuses_on_root_mismatch`,
/// `decide_refuses_when_not_served`.
pub(crate) fn decide_auto_pin(candidate: &CwdCandidate, verdict: Confirmation) -> AutoPin {
    let root = candidate.project_root.display();
    let id = &candidate.index_id;
    match verdict {
        Confirmation::Confirmed => AutoPin::Pinned(PinChoice {
            index_id: candidate.index_id.clone(),
            source: PinSource::WorkingDir,
        }),
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

/// Read the daemon's index list into id/root pairs.
///
/// Why: `?details=true` carries `root_path` alongside each id (#661, added so
/// callers could derive the index from the current project directory), which is
/// the field [`confirm_candidate`] needs. The flat `GET /indexes` returns bare
/// ids and cannot confirm identity.
/// What: tolerates entries missing or misshaping `root_path` by carrying `None`
/// rather than dropping the entry, so a null root reports as unconfirmable
/// instead of as an unknown index. Entries with no string `id` are skipped.
/// Test: `parse_entries_reads_id_and_root`, `parse_entries_tolerates_null_root`.
pub(crate) fn parse_index_entries(body: &serde_json::Value) -> Vec<DaemonIndex> {
    body.get("indexes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let id = e.get("id")?.as_str()?.to_string();
                    let root_path = e
                        .get("root_path")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from);
                    Some(DaemonIndex { id, root_path })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Fetch the daemon's index list.
///
/// Why: routes through `trusty_common::server::daemon_http_client`, the shared
/// entry point every other trusty-search daemon call already uses, so the
/// 2 s connect / 5 s request timeouts apply here too — a startup probe must
/// never hang an MCP session waiting on a wedged daemon.
/// What: `GET {base_url}/indexes?details=true`, parsed by
/// [`parse_index_entries`]. Errors propagate so the caller can report the
/// session as unpinned rather than guessing.
/// Test: `fetch_reads_entries_from_a_live_server` binds an ephemeral port.
pub(crate) async fn fetch_index_entries(base_url: &str) -> anyhow::Result<Vec<DaemonIndex>> {
    let client = trusty_common::server::daemon_http_client()?;
    let url = format!("{}/indexes?details=true", base_url.trim_end_matches('/'));
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("daemon returned {} for {url}", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
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
pub(crate) async fn auto_pin_from_cwd(base_url: &str, cwd: &Path) -> Option<AutoPin> {
    let candidate = derive_cwd_candidate(cwd)?;
    match fetch_index_entries(base_url).await {
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
