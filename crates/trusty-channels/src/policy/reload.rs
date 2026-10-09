//! Reloading the route policy when a file or a branch moves.
//!
//! Why: #8454 S2b §3 and Architect Q3. A route edit takes effect without a
//! restart, but an unreviewed edit must never widen what is in effect, and
//! a narrowing must apply at once.
//! What: [`PolicyLoader`] holds the current [`LoadReport`], the last-good
//! parse of each project file and a content fingerprint. [`PolicyLoader::refresh`]
//! recomputes the fingerprint and reloads only when it changed. The
//! `RateLimiter` lives with the consumer and is never rebuilt here, so a
//! reload cannot reset a flood's window.
//! Test: `src/policy/tests/reload.rs`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::policy::fs::{read_host, read_project, ProjectRead};
use crate::policy::gate::{branch_state, BranchState};
use crate::policy::host::parse_host;
use crate::policy::load::{assemble, listed_dirs, prepare, LoadRequest, Prepared};
use crate::policy::project_file::ProjectFile;
use crate::policy::report::{FileState, Finding, LoadReport};
use crate::policy::table::ChannelPolicy;

/// What the reload check compares between batches.
///
/// Why: the Architect's review: a content fingerprint, not mtime/size/inode,
/// so a same-size, same-mtime edit is seen; plus each repo's `HEAD` and
/// default-branch commit, so a branch switch with identical bytes re-gates.
/// What: the exact bytes of the host file and each listed project file
/// (each capped at 256 KiB, so the bytes are the fingerprint and no hash
/// collision can hide an edit), each dir's canonical path, and its
/// [`BranchState`]. A read or git fault is recorded as its message, so a
/// change of fault also reloads.
/// Test: `same_size_same_mtime_edit_is_seen_on_reload`,
/// `branch_switch_with_identical_bytes_regates`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    host: Result<String, String>,
    projects: Vec<ProjectPrint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectPrint {
    dir: PathBuf,
    real: Result<PathBuf, String>,
    file: Result<Option<Vec<u8>>, String>,
    branch: Option<Result<BranchState, String>>,
}

impl Fingerprint {
    fn of(req: &LoadRequest) -> Self {
        let host = read_host(&req.host_path).map_err(|e| e.to_string());
        let ceiling = match (&host, req.home.as_deref()) {
            (Ok(text), Some(home)) => parse_host(text, Some(home)).ok(),
            _ => None,
        };
        let dirs = ceiling
            .map(|c| listed_dirs(&c, &req.channels, req.project.as_deref()))
            .unwrap_or_default();
        let projects = dirs.into_iter().map(|dir| ProjectPrint::of(&dir)).collect();
        Self { host, projects }
    }
}

impl ProjectPrint {
    fn of(dir: &Path) -> Self {
        let file = match read_project(dir) {
            Ok(ProjectRead::Missing) => Ok(None),
            Ok(ProjectRead::Bytes(b)) => Ok(Some(b)),
            Err(e) => Err(e.to_string()),
        };
        // Only a present file is gated, so only then does the branch matter.
        let branch =
            matches!(file, Ok(Some(_))).then(|| branch_state(dir).map_err(|e| e.to_string()));
        Self {
            dir: dir.to_path_buf(),
            real: std::fs::canonicalize(dir).map_err(|e| e.kind().to_string()),
            file,
            branch,
        }
    }
}

/// A route policy that reloads when its sources change.
///
/// Why: gchat's `process_batch` (S3) and the daemon's dispatch (S2c) call
/// [`PolicyLoader::refresh`] before acting, so a reviewed change applies
/// without a restart (S2b plan §3).
/// What: see the module doc and [`PolicyLoader::reload`].
/// Test: `routes_edit_outside_the_reviewed_source_has_no_effect`,
/// `adding_a_project_needs_no_code_change`, `reload_keeps_rate_limit_logs`.
#[derive(Debug, Clone)]
pub struct PolicyLoader {
    req: LoadRequest,
    fingerprint: Fingerprint,
    report: LoadReport,
    last_good: HashMap<PathBuf, ProjectFile>,
}

impl PolicyLoader {
    /// Load once for `req`. A fault at start has no last-good to keep.
    pub fn new(req: LoadRequest) -> Self {
        let fingerprint = Fingerprint::of(&req);
        let mut loader = Self {
            req,
            fingerprint,
            report: LoadReport::empty(),
            last_good: HashMap::new(),
        };
        loader.apply(prepare(&loader.req));
        loader
    }

    /// The current report.
    pub fn report(&self) -> &LoadReport {
        &self.report
    }

    /// The routes in effect now.
    pub fn policy(&self) -> &ChannelPolicy {
        &self.report.policy
    }

    /// Reload when the fingerprint changed; true when it reloaded.
    ///
    /// Why: one call per batch or dispatch; git runs only on a change.
    /// What: compares [`Fingerprint`]s; on a difference, reloads.
    /// Test: `same_size_same_mtime_edit_is_seen_on_reload`,
    /// `branch_switch_with_identical_bytes_regates`.
    pub fn refresh(&mut self) -> bool {
        let now = Fingerprint::of(&self.req);
        if now == self.fingerprint {
            return false;
        }
        // Recorded before the load reads: a change in between only causes
        // one more reload, never a missed one.
        self.fingerprint = now;
        self.apply(prepare(&self.req));
        true
    }

    /// Reload unconditionally (doctor, tests).
    ///
    /// Why: Architect Q3: a host fault denies all at once; a project-file
    /// fault keeps that project's last-good routes, marked `Stale`; a valid
    /// narrowing applies at once.
    /// What: loads; each file refused in that load and holding a last-good
    /// parse is merged again from its last-good parse, under the current
    /// host ceiling, and marked `Stale` with its refusal kept as findings.
    /// Last-good is then the parse of every file `Effective` in this load;
    /// a host fault clears it.
    /// Test: `routes_edit_outside_the_reviewed_source_has_no_effect`,
    /// `host_fault_on_reload_denies_all_at_once`,
    /// `valid_narrowing_reload_applies_at_once`.
    pub fn reload(&mut self) {
        self.fingerprint = Fingerprint::of(&self.req);
        self.apply(prepare(&self.req));
    }

    fn apply(&mut self, prepared: Prepared) {
        let channels = self.req.channels.clone();
        if prepared.host.is_err() {
            // #8454 Q3: the host is the master switch.
            self.last_good.clear();
            self.report = assemble(prepared, &channels);
            return;
        }
        let first = assemble(prepared.clone(), &channels);
        let stale: HashSet<PathBuf> = first
            .per_file
            .iter()
            .filter(|s| s.state == FileState::Refused && self.last_good.contains_key(&s.file))
            .map(|s| s.file.clone())
            .collect();
        let report = if stale.is_empty() {
            first
        } else {
            let mut second = prepared.clone();
            for p in &mut second.projects {
                if let Some(good) = self
                    .last_good
                    .get(&p.file)
                    .filter(|_| stale.contains(&p.file))
                {
                    p.parsed = Some(Ok(good.clone()));
                }
            }
            let mut report = assemble(second, &channels);
            for s in &mut report.per_file {
                if stale.contains(&s.file) && matches!(s.state, FileState::Effective { .. }) {
                    s.state = FileState::Stale;
                }
            }
            // #8454 Q3: the refusal stays visible beside the Stale finding.
            for f in first.findings.iter().filter(|f| in_files(f, &stale)) {
                report.findings.push(f.clone());
            }
            for file in &stale {
                report.findings.push(Finding::Stale { file: file.clone() });
            }
            report
        };
        // #8454 Q3: last-good comes only from a file that passed the gate
        // and the merge in this load; a Stale file keeps its old one.
        let fresh: HashMap<&Path, &ProjectFile> = prepared
            .projects
            .iter()
            .filter_map(|p| match &p.parsed {
                Some(Ok(f)) => Some((p.file.as_path(), f)),
                _ => None,
            })
            .collect();
        let mut last_good = HashMap::new();
        for s in &report.per_file {
            let kept = match s.state {
                FileState::Stale => self.last_good.get(&s.file).cloned(),
                FileState::Effective { .. } => fresh.get(s.file.as_path()).map(|f| (*f).clone()),
                _ => None,
            };
            if let Some(f) = kept {
                last_good.insert(s.file.clone(), f);
            }
        }
        self.last_good = last_good;
        self.report = report;
    }
}

/// True when `f` concerns one of `files`.
fn in_files(f: &Finding, files: &HashSet<PathBuf>) -> bool {
    let file = match f {
        Finding::FileRefused { file, .. } | Finding::NoGchatConnection { file } => file,
        Finding::RouteRejected { origin, .. } | Finding::Widening { origin, .. } => &origin.file,
        Finding::Overlap { first, .. } => &first.file,
        _ => return false,
    };
    files.contains(file)
}
