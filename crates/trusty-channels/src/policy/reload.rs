//! Reloading the route policy when a file or a branch moves.
//!
//! Why: #8454 S2b §3 and Architect Q3. A route edit takes effect without a
//! restart, but an unreviewed edit must never widen what is in effect, and
//! a narrowing must apply at once.
//! What: [`PolicyLoader`] holds the current [`LoadReport`], the last-good
//! parse of each project file and a content fingerprint. [`PolicyLoader::refresh`]
//! recomputes the fingerprint and reloads only when it changed. A load keeps
//! the fingerprint sampled after its read only when it equals the one
//! sampled before and describes the bytes read; otherwise it keeps none, so
//! the next refresh reloads (#9808). The `RateLimiter` lives with the
//! consumer and is never rebuilt here, so a reload cannot reset a flood's
//! window.
//! Test: `src/policy/tests/reload.rs`.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::policy::fs::{read_host, read_project, ProjectRead};
use crate::policy::gate::{branch_state, BranchState};
use crate::policy::host::{parse_host, HostError};
use crate::policy::load::{
    assemble, check_canonical, listed_dirs, prepare, FileRead, LoadRequest, Prepared,
};
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
/// [`BranchState`], plus the canonical-path verdict over every `projects`
/// entry on every channel, served or not, since a fresh load denies all on
/// any of them. A read or git fault is recorded as its message, so a
/// change of fault also reloads; the host read includes the check that its
/// real path is the one under the canonical home.
/// Test: `same_size_same_mtime_edit_is_seen_on_reload`,
/// `branch_switch_with_identical_bytes_regates`,
/// `host_fault_on_an_unserved_entry_denies_all_on_refresh`,
/// `trusty_tools_becoming_a_symlink_denies_all_on_refresh`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    host: Result<String, String>,
    canonical: Option<Result<(), String>>,
    projects: Vec<ProjectPrint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectPrint {
    dir: PathBuf,
    real: Result<PathBuf, String>,
    file: FileRead,
    branch: Option<Result<BranchState, String>>,
}

impl Fingerprint {
    fn of(req: &LoadRequest) -> Self {
        // #8454 Architect ruling: read_host checks the canonical path, so a
        // ~/.trusty-tools swapped for a symlink changes the fingerprint.
        let host = match req.home.as_deref() {
            Some(home) => read_host(&req.host_path, home).map_err(|e| e.to_string()),
            None => Err(HostError::HomeUnknown.to_string()),
        };
        let ceiling = match (&host, req.home.as_deref()) {
            (Ok(text), Some(home)) => parse_host(text, Some(home)).ok(),
            _ => None,
        };
        // #8454 Q3: an entry this consumer never reads still denies all.
        let canonical = ceiling
            .as_ref()
            .map(|c| check_canonical(c).map_err(|e| e.to_string()));
        let dirs = ceiling
            .map(|c| listed_dirs(&c, &req.channels, req.project.as_deref()))
            .unwrap_or_default();
        let projects = dirs.into_iter().map(|dir| ProjectPrint::of(&dir)).collect();
        Self {
            host,
            canonical,
            projects,
        }
    }

    /// A nonzero digest of every field, for `LoadReport::fingerprint`.
    ///
    /// Why: #8454 VC keys the daemon's egress verdict on it, and 0 is
    /// reserved for "never cache" (a report no loader stamped).
    /// What: hashes each field in order, the project count first; a digest
    /// of 0 is moved to 1.
    /// Test: `loader_report_carries_a_fingerprint_that_follows_its_sources`.
    fn digest(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.host.hash(&mut h);
        self.canonical.hash(&mut h);
        self.projects.len().hash(&mut h);
        for p in &self.projects {
            p.dir.hash(&mut h);
            p.real.hash(&mut h);
            p.file.hash(&mut h);
            // `BranchState` is public and not `Hash`; hash its fields here.
            match &p.branch {
                None => 0u8.hash(&mut h),
                Some(Err(e)) => (1u8, e).hash(&mut h),
                Some(Ok(b)) => (2u8, &b.default, &b.head, &b.commit).hash(&mut h),
            }
        }
        h.finish().max(1)
    }

    /// True when `read` saw exactly the sources this fingerprint records.
    ///
    /// Why: #9808: a load must not keep a fingerprint that describes other
    /// bytes than the ones it parsed.
    /// What: the host text or fault and the canonical verdict must match;
    /// after a host fault no project file enters the report, so none is
    /// compared; otherwise every listed project must have been read, with
    /// these bytes.
    /// Test: `host_file_change_and_revert_during_load_reloads`,
    /// `canonical_fault_cleared_and_restored_during_load_reloads`,
    /// `reload_after_change_and_revert_during_load_reloads`,
    /// `project_not_read_by_the_load_never_settles`,
    /// `stable_load_settles_and_next_refresh_is_false`.
    fn describes(&self, read: &Prepared) -> bool {
        if read.host_read != self.host || read.canonical != self.canonical {
            return false;
        }
        // #9808: a host fault denies all; the project files are not read.
        if read.host.is_err() {
            return true;
        }
        // #9808: a project not read, or read with other bytes, is a mismatch.
        self.projects.iter().all(|p| {
            read.projects
                .iter()
                .any(|r| r.dir == p.dir && r.read.as_ref() == Some(&p.file))
        })
    }
}

/// The fingerprint a load keeps: `Some` only when it describes the load.
///
/// Why: #9808: a fingerprint taken before the read misses a change that is
/// reverted before the next refresh (S1 to S2 and back to S1), so the S2
/// policy would stay published.
/// What: samples the sources after the read; keeps that sample only when it
/// equals `pre` and [`Fingerprint::describes`] the read, else `None`. A git
/// state that flips inside the read and back before this sample is not seen
/// (the gate runs its own git).
/// Test: `persistent_change_during_load_reloads_once_then_settles`,
/// `refresh_after_change_and_revert_during_load_reloads`,
/// `stable_load_settles_and_next_refresh_is_false`.
fn settle(req: &LoadRequest, pre: Fingerprint, read: &Prepared) -> Option<Fingerprint> {
    let post = Fingerprint::of(req);
    // #9808: a change during the load, kept or not, matches nothing.
    if post != pre {
        return None;
    }
    post.describes(read).then_some(post)
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
    /// #9808: `None` when the last load did not settle; matches nothing.
    fingerprint: Option<Fingerprint>,
    report: LoadReport,
    last_good: HashMap<PathBuf, ProjectFile>,
}

impl PolicyLoader {
    /// Load once for `req`. A fault at start has no last-good to keep.
    pub fn new(req: LoadRequest) -> Self {
        Self::new_with(req, prepare)
    }

    /// [`PolicyLoader::new`], reading through `prepare_fn`.
    ///
    /// Why: #9808: a test must change the sources inside the load's read.
    /// What: production passes [`prepare`]; a test wraps it.
    /// Test: `new_after_change_and_revert_during_load_reloads`.
    pub(crate) fn new_with(
        req: LoadRequest,
        prepare_fn: impl FnOnce(&LoadRequest) -> Prepared,
    ) -> Self {
        let pre = Fingerprint::of(&req);
        let mut loader = Self {
            req,
            fingerprint: None,
            report: LoadReport::empty(),
            last_good: HashMap::new(),
        };
        loader.load_with(pre, prepare_fn);
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
    /// What: compares [`Fingerprint`]s; on a difference, or when the last
    /// load kept none, reloads.
    /// Test: `same_size_same_mtime_edit_is_seen_on_reload`,
    /// `branch_switch_with_identical_bytes_regates`,
    /// `refresh_after_change_and_revert_during_load_reloads`.
    pub fn refresh(&mut self) -> bool {
        self.refresh_with(prepare)
    }

    /// [`PolicyLoader::refresh`], reading through `prepare_fn` (#9808).
    pub(crate) fn refresh_with(
        &mut self,
        prepare_fn: impl FnOnce(&LoadRequest) -> Prepared,
    ) -> bool {
        let now = Fingerprint::of(&self.req);
        if self.fingerprint.as_ref() == Some(&now) {
            return false;
        }
        self.load_with(now, prepare_fn);
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
        self.reload_with(prepare);
    }

    /// [`PolicyLoader::reload`], reading through `prepare_fn` (#9808).
    pub(crate) fn reload_with(&mut self, prepare_fn: impl FnOnce(&LoadRequest) -> Prepared) {
        let pre = Fingerprint::of(&self.req);
        self.load_with(pre, prepare_fn);
    }

    /// Read through `prepare_fn`, keep the settled fingerprint, then apply.
    ///
    /// Why: #9808: one sequence for `new`, `refresh` and `reload`, so none
    /// keeps a fingerprint taken before its read.
    /// What: the fingerprint is written only after `prepare_fn` returns, so
    /// a panic there leaves the previous fingerprint and report, which
    /// agree. It is written before `apply`, so a stamp inside `apply` sees
    /// the settled value (#9807).
    /// Test: `panic_during_load_leaves_a_loader_that_reloads`,
    /// `new_after_change_and_revert_during_load_reloads`.
    fn load_with(&mut self, pre: Fingerprint, prepare_fn: impl FnOnce(&LoadRequest) -> Prepared) {
        let prepared = prepare_fn(&self.req);
        self.fingerprint = settle(&self.req, pre, &prepared);
        self.apply(prepared);
    }

    fn apply(&mut self, prepared: Prepared) {
        let channels = self.req.channels.clone();
        if prepared.host.is_err() {
            // #8454 Q3: the host is the master switch.
            self.last_good.clear();
            self.report = assemble(prepared, &channels);
            // #8454 VC: a denied reload is stamped too; the route answers it as not ready.
            self.report.fingerprint = self.fingerprint.digest();
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
        // #8454 VC: only a loader's report carries a fingerprint.
        self.report.fingerprint = self.fingerprint.digest();
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
