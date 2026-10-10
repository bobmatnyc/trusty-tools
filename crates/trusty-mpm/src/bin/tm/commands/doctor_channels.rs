//! The `channels_*` rows of `tm doctor` (#8454 S2c).
//!
//! Why: every chat channel is deny by default. An operator needs to see what
//! a fresh load of the route policy would put in effect, and why anything is
//! refused, without sending a message or changing a file.
//! What: four READ-ONLY rows, each the verdict of
//! `trusty_channels::policy::load_effective_until`: `channels_host` (the host
//! ceiling, loaded with no channel, so no git runs), `channels_routes` (the
//! daemon view: Slack and Telegram over every listed project),
//! `channels_gchat` (one load per gchat-listed project, as gchat-mcp loads)
//! and `channels_gate` (the default-branch gate's refusals, with the fix).
//! Each load runs on its own thread under [`LOAD_TIMEOUT`], and its git is
//! killed [`KILL_MARGIN`] before that wait ends; one that does not finish
//! reads Unknown and doctor moves on. No enforcement, no `refresh()`
//! and no credential resolution (S2c rulings Q1, Q8, Q9). A row names a
//! credential ref, never a value.
//! Test: `doctor_channels_tests.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use trusty_channels::policy::{
    Channel, FileState, Finding, FindingScope, GateError, HostCeiling, HostError, LoadReport,
    LoadRequest, ProjectFileError, load_effective_until, parse_host,
};
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};

/// The budget for each channel load (S2c ruling Q8).
pub(crate) const LOAD_TIMEOUT: Duration = Duration::from_secs(30);
/// How long before its wait ends a load's git is killed (#8454), so no git
/// process outlives doctor.
pub(crate) const KILL_MARGIN: Duration = Duration::from_secs(2);

const HOST: &str = "channels_host";
const ROUTES: &str = "channels_routes";
const GCHAT: &str = "channels_gchat";
const GATE: &str = "channels_gate";
/// A row names at most this many files or findings, then "+N more".
const MAX_NAMED: usize = 5;

/// A route-policy load whose git ends by the given deadline, injectable so
/// tests can stall or record it.
pub(crate) type Loader = Arc<dyn Fn(&LoadRequest, Instant) -> LoadReport + Send + Sync>;

/// Why a load gave no report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unfinished {
    /// The load ran past its budget; its thread is left behind.
    TimedOut(Duration),
    /// The load stopped without a result (it panicked, or no thread started).
    Stopped,
}

impl fmt::Display for Unfinished {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimedOut(limit) => write!(f, "did not finish within {limit:?}"),
            Self::Stopped => f.write_str("stopped without a result"),
        }
    }
}

/// A load's result, or why there is none.
pub(crate) type Outcome<T> = Result<T, Unfinished>;

/// The host ceiling load.
pub(crate) struct HostProbe {
    /// The host file.
    pub(crate) path: PathBuf,
    /// The load with no channel: home, host file and ceiling checks only.
    pub(crate) report: LoadReport,
    /// The parsed ceiling, for the row's detail; `None` when the load did
    /// not accept it or the re-read failed.
    pub(crate) ceiling: Option<HostCeiling>,
}

/// The four rows for this host.
///
/// Why: `tm doctor` reports what a fresh start would load (S2c plan §0.2:
/// a one-shot process holds no last-good state, so it never reports Stale).
/// What: [`rows_with`] over the production request and loader.
/// Test: `doctor_channels_tests.rs` drives [`rows_with`].
pub(crate) async fn channel_rows() -> Vec<DoctorCheck> {
    rows_with(
        LoadRequest::for_host(None, &[]),
        Arc::new(load_effective_until),
        LOAD_TIMEOUT,
    )
    .await
}

/// The four rows for `base`'s host file and home, loading through `loader`.
///
/// Why: one combined load would false-Fail: two projects routing gchat to one
/// person deny the combined set, while gchat-mcp loads one project at a time
/// (S2c plan §3). So each consumer's view is loaded as that consumer loads it.
/// What: the host load first (its ceiling lists the gchat projects), then the
/// daemon view and the gchat view concurrently, each under `limit`, with
/// its git ending by [`timed`]'s deadline.
/// Test: `daemon_view_serves_slack_and_telegram_and_gchat_loads_each_project`,
/// `a_load_that_hangs_is_unknown_and_doctor_finishes`, `a_load_that_panics_is_unknown`,
/// `doctor_leaves_no_git_running_at_its_budget`.
pub(crate) async fn rows_with(
    base: LoadRequest,
    loader: Loader,
    limit: Duration,
) -> Vec<DoctorCheck> {
    let host = {
        let (loader, base) = (Arc::clone(&loader), base.clone());
        timed(limit, move |deadline| host_probe(&*loader, &base, deadline)).await
    };
    let gchat_dirs = host.as_ref().map(|p| gchat_dirs(p.ceiling.as_ref()));
    let daemon = {
        let req = view(&base, None, &[Channel::Slack, Channel::Telegram]);
        let loader = Arc::clone(&loader);
        timed(limit, move |deadline| vec![loader(&req, deadline)])
    };
    let gchat = async {
        // #8454 S2c: no host result means no project list; never "no projects".
        let dirs = gchat_dirs.map_err(|u| *u)?;
        let reqs: Vec<LoadRequest> = if dirs.is_empty() {
            vec![view(&base, None, &[Channel::Gchat])]
        } else {
            dirs.into_iter()
                .map(|d| view(&base, Some(d), &[Channel::Gchat]))
                .collect()
        };
        let loader = Arc::clone(&loader);
        // #8454: one deadline for every project; past it, the rest are refused.
        timed(limit, move |deadline| {
            reqs.iter().map(|r| loader(r, deadline)).collect()
        })
        .await
    };
    let (daemon, gchat) = tokio::join!(daemon, gchat);
    vec![
        host_row(&host),
        view_row(ROUTES, "daemon view", &daemon),
        view_row(GCHAT, "gchat view", &gchat),
        gate_row(&daemon, &gchat),
    ]
}

/// Run `f` on a detached thread; `Err` when it does not return within `limit`.
///
/// Why (Q8): a git step can block up to 10 s and a project list is unbounded,
/// so a load has no overall budget of its own. A detached std thread, not
/// `spawn_blocking`: a runtime waits for its blocking tasks on shutdown, so a
/// stuck load would hold doctor's exit. #8454: a thread left behind must not
/// leave git behind, so `f` gets a deadline to pass to the load.
/// What: hands `f` the deadline [`load_deadline`] sets, [`KILL_MARGIN`]
/// before the wait ends, and waits up to `limit` for its value; a panic in
/// `f`, or a thread that cannot start, is [`Unfinished::Stopped`].
/// Test: `a_load_that_hangs_is_unknown_and_doctor_finishes`,
/// `a_load_that_panics_is_unknown`, `doctor_leaves_no_git_running_at_its_budget`.
async fn timed<T: Send + 'static>(
    limit: Duration,
    f: impl FnOnce(Instant) -> T + Send + 'static,
) -> Outcome<T> {
    let deadline = load_deadline(Instant::now(), limit);
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("tm-doctor-channels".into())
        .spawn(move || {
            let _ = tx.send(f(deadline));
        })
        .map_err(|_| Unfinished::Stopped)?;
    match tokio::time::timeout(limit, rx).await {
        Ok(Ok(value)) => Ok(value),
        // The sender dropped without a value: `f` panicked.
        Ok(Err(_)) => Err(Unfinished::Stopped),
        Err(_) => Err(Unfinished::TimedOut(limit)),
    }
}

/// When a load waited on from `start` for `limit` must have killed its git:
/// [`KILL_MARGIN`] before the wait ends, or halfway through a shorter wait.
/// Test: `a_load_deadline_falls_before_its_wait_ends`.
pub(crate) fn load_deadline(start: Instant, limit: Duration) -> Instant {
    start + limit - KILL_MARGIN.min(limit / 2)
}

/// `base`'s host file and home, for `project` and `channels`.
fn view(base: &LoadRequest, project: Option<PathBuf>, channels: &[Channel]) -> LoadRequest {
    LoadRequest {
        host_path: base.host_path.clone(),
        home: base.home.clone(),
        project,
        channels: channels.to_vec(),
    }
}

/// Load with no channel, then re-read the accepted ceiling for its detail.
fn host_probe(
    loader: &(dyn Fn(&LoadRequest, Instant) -> LoadReport + Send + Sync),
    base: &LoadRequest,
    deadline: Instant,
) -> HostProbe {
    let report = loader(&view(base, None, &[]), deadline);
    let ceiling = if report.denied {
        None
    } else {
        std::fs::read_to_string(&base.host_path)
            .ok()
            .and_then(|text| parse_host(&text, base.home.as_deref()).ok())
    };
    HostProbe {
        path: base.host_path.clone(),
        report,
        ceiling,
    }
}

/// The enabled gchat channel's listed projects.
fn gchat_dirs(ceiling: Option<&HostCeiling>) -> Vec<PathBuf> {
    ceiling
        .and_then(|c| c.channel(Channel::Gchat))
        .filter(|ch| ch.enabled())
        .map(|ch| ch.projects().to_vec())
        .unwrap_or_default()
}

/// What the host ceiling decided for one load.
enum HostVerdict<'a> {
    /// The ceiling loaded.
    Loaded,
    /// No file or no `channels:` section: the deny state, by design.
    NotConfigured(&'a HostError),
    /// No home directory is known.
    HomeUnknown,
    /// Any other host fault; every route is denied.
    Refused(&'a HostError),
}

fn host_verdict(report: &LoadReport) -> HostVerdict<'_> {
    let found = report.findings.iter().find_map(|f| match f {
        Finding::HostRefused { error } => Some(error),
        _ => None,
    });
    match found {
        None => HostVerdict::Loaded,
        Some(e @ (HostError::Missing | HostError::NoChannelsSection)) => {
            HostVerdict::NotConfigured(e)
        }
        Some(HostError::HomeUnknown) => HostVerdict::HomeUnknown,
        // `HostError` is non_exhaustive: an unknown fault is a refusal.
        Some(e) => HostVerdict::Refused(e),
    }
}

/// The row a host verdict decides on its own, or `None` once it loaded.
fn host_decides(name: &str, report: &LoadReport) -> Option<DoctorCheck> {
    match host_verdict(report) {
        HostVerdict::Loaded => None,
        HostVerdict::NotConfigured(e) => {
            Some(row(name, CheckStatus::Ok, format!("not configured: {e}")))
        }
        HostVerdict::HomeUnknown => Some(row(
            name,
            CheckStatus::Unknown,
            "no home directory is known, so the host ceiling cannot be read; every channel is denied",
        )),
        HostVerdict::Refused(e) => Some(row(
            name,
            CheckStatus::Fail,
            format!("host ceiling refused: {e}; every channel is denied"),
        )),
    }
}

fn row(name: &str, status: CheckStatus, message: impl Into<String>) -> DoctorCheck {
    DoctorCheck::new(name, status, message)
}

/// The `channels_host` row.
///
/// Why: the host file is the root of trust; a fault there denies every route.
/// What: Ok "not configured" for a missing file or section (that IS the
/// deny state); Fail with the `HostError` text, which never holds a value;
/// Unknown for no home, an unfinished load or an unreadable detail; else Ok
/// with each channel's state, project count and ref names.
/// Test: `not_configured_reads_as_the_deny_state`,
/// `host_faults_fail_and_never_echo_the_value`, `home_unknown_is_unknown`,
/// `valid_host_names_refs_only`, `host_loaded_but_unreadable_details_is_unknown`,
/// `a_denied_load_without_a_host_finding_is_fail`.
pub(crate) fn host_row(probe: &Outcome<HostProbe>) -> DoctorCheck {
    let probe = match probe {
        Ok(p) => p,
        Err(u) => {
            return row(
                HOST,
                CheckStatus::Unknown,
                format!("the host ceiling load {u}"),
            );
        }
    };
    if let Some(decided) = host_decides(HOST, &probe.report) {
        return decided;
    }
    let path = probe.path.display();
    if probe.report.denied {
        return row(
            HOST,
            CheckStatus::Fail,
            format!(
                "{path}: every channel is denied: {}",
                deny_text(&probe.report)
            ),
        );
    }
    let Some(ceiling) = &probe.ceiling else {
        return row(
            HOST,
            CheckStatus::Unknown,
            format!("{path} loaded, but its details could not be re-read"),
        );
    };
    let limit = ceiling.rate_limit();
    row(
        HOST,
        CheckStatus::Ok,
        format!(
            "{path}: {}; default limit {} per {} s",
            channels_text(ceiling),
            limit.limit(),
            limit.window_secs()
        ),
    )
}

/// Each named channel: on or off, project count, and its ref names.
fn channels_text(ceiling: &HostCeiling) -> String {
    let parts: Vec<String> = Channel::ALL
        .into_iter()
        .filter_map(|c| {
            let ch = ceiling.channel(c)?;
            let state = if ch.enabled() { "on" } else { "off" };
            let mut text = format!("{c} {state}, {} project(s)", ch.projects().len());
            if c != Channel::Gchat {
                // #8454 S2c: ref names only; nothing here resolves a secret.
                match ch.bot_ref() {
                    Some(bot) => text.push_str(&format!(", bot_ref {bot}")),
                    None => text.push_str(", no connection"),
                }
                if let Some(app) = ch.app_ref() {
                    text.push_str(&format!(", app_ref {app}"));
                }
            }
            Some(text)
        })
        .collect();
    if parts.is_empty() {
        "no channel is named".into()
    } else {
        parts.join("; ")
    }
}

/// The findings that deny a whole load.
fn deny_text(report: &LoadReport) -> String {
    let items: Vec<String> = report
        .findings
        .iter()
        .filter(|f| f.scope() == FindingScope::DenyAll)
        .map(ToString::to_string)
        .collect();
    if items.is_empty() {
        "no finding names the cause".into()
    } else {
        capped(items)
    }
}

/// At most [`MAX_NAMED`] items, then "+N more".
fn capped(items: Vec<String>) -> String {
    let extra = items.len().saturating_sub(MAX_NAMED);
    let mut text = items
        .into_iter()
        .take(MAX_NAMED)
        .collect::<Vec<_>>()
        .join("; ");
    if extra > 0 {
        text.push_str(&format!(" (+{extra} more)"));
    }
    text
}

/// A `channels_routes` or `channels_gchat` row over one view's reports.
///
/// Why (Fail-Open Check): only a clean load reads Ok.
/// What: Unknown for an unfinished load; the host verdict first; Fail when
/// any load is denied; Warn when a file is refused or in a state this build
/// does not treat as in effect (`Stale`, or a state added later); else Ok
/// with the routes in effect per channel and the files by state.
/// Test: `cross_file_overlap_is_fail`, `file_faults_and_odd_states_are_not_ok`,
/// `refused_route_file_warns_and_gate_names_the_fix`, `unfinished_outcomes_are_unknown`,
/// `committed_route_is_ok_and_leaves_repo_untouched`.
pub(crate) fn view_row(
    name: &'static str,
    view: &str,
    reports: &Outcome<Vec<LoadReport>>,
) -> DoctorCheck {
    let reports = match reports {
        Ok(r) => r,
        Err(u) => return row(name, CheckStatus::Unknown, format!("the {view} load {u}")),
    };
    if reports.is_empty() {
        return row(
            name,
            CheckStatus::Unknown,
            format!("the {view} ran no load"),
        );
    }
    if let Some(decided) = reports.iter().find_map(|r| host_decides(name, r)) {
        return decided;
    }
    let denied: Vec<String> = reports.iter().filter(|r| r.denied).map(deny_text).collect();
    if !denied.is_empty() {
        return row(
            name,
            CheckStatus::Fail,
            format!("{view}: every route is denied: {}", denied.join("; ")),
        );
    }
    let tally = Tally::of(reports);
    let summary = tally.summary();
    if !tally.odd.is_empty() {
        let odd = capped(tally.odd);
        return row(
            name,
            CheckStatus::Warn,
            format!("{view}: {summary}; not in effect: {odd}"),
        );
    }
    if tally.refused > 0 || !tally.file_findings.is_empty() {
        let refused = capped(tally.file_findings);
        return row(
            name,
            CheckStatus::Warn,
            format!("{view}: {summary}; refused: {refused}"),
        );
    }
    row(name, CheckStatus::Ok, format!("{view}: {summary}"))
}

/// One view's routes and files, counted.
#[derive(Default)]
struct Tally {
    routes: BTreeMap<Channel, usize>,
    effective: usize,
    missing: usize,
    refused: usize,
    dropped: usize,
    file_findings: Vec<String>,
    odd: Vec<String>,
}

impl Tally {
    fn of(reports: &[LoadReport]) -> Self {
        let mut t = Self::default();
        for report in reports {
            for route in report.policy.routes() {
                *t.routes.entry(route.channel()).or_default() += 1;
            }
            for status in &report.per_file {
                let file = status.file.display();
                match status.state {
                    FileState::Effective { .. } => t.effective += 1,
                    FileState::Missing => t.missing += 1,
                    FileState::Refused => t.refused += 1,
                    FileState::Stale => t.odd.push(format!("{file}: last-good routes in effect")),
                    FileState::Withheld => t.odd.push(format!("{file}: withheld")),
                    // `FileState` is non_exhaustive: a new state is never Ok.
                    _ => t.odd.push(format!("{file}: unrecognised state")),
                }
            }
            for finding in &report.findings {
                match finding.scope() {
                    FindingScope::Route => t.dropped += 1,
                    _ => t.file_findings.push(finding.to_string()),
                }
            }
        }
        t
    }

    fn summary(&self) -> String {
        let total: usize = self.routes.values().sum();
        let per: Vec<String> = self
            .routes
            .iter()
            .map(|(c, n)| format!("{c} {n}"))
            .collect();
        let per = if per.is_empty() {
            String::new()
        } else {
            format!(" ({})", per.join(", "))
        };
        format!(
            "{total} route(s) in effect{per}; route files: {} effective, {} missing, {} refused; {} route(s) dropped by the host ceiling",
            self.effective, self.missing, self.refused, self.dropped
        )
    }
}

/// The `channels_gate` row over both views.
///
/// Why: a refused route file is fixed in git, so the row names the fix.
/// What: Unknown when either view is unfinished or the host was refused (the
/// gate never ran, so its state is unread); Ok "not configured" for the deny
/// state; Warn naming each refused file, its reason and fix; else Ok with
/// the count of files that matched the default branch.
/// Test: `each_gate_refusal_is_a_warn_with_its_fix`,
/// `refused_route_file_warns_and_gate_names_the_fix`, `unfinished_outcomes_are_unknown`.
pub(crate) fn gate_row(
    daemon: &Outcome<Vec<LoadReport>>,
    gchat: &Outcome<Vec<LoadReport>>,
) -> DoctorCheck {
    let (daemon, gchat) = match (daemon, gchat) {
        (Ok(d), Ok(g)) => (d, g),
        (Err(u), _) | (_, Err(u)) => {
            return row(
                GATE,
                CheckStatus::Unknown,
                format!("the gate state is unread: a load {u}"),
            );
        }
    };
    let all: Vec<&LoadReport> = daemon.iter().chain(gchat).collect();
    for report in &all {
        match host_verdict(report) {
            HostVerdict::Loaded => {}
            HostVerdict::NotConfigured(e) => {
                return row(GATE, CheckStatus::Ok, format!("not configured: {e}"));
            }
            HostVerdict::HomeUnknown => {
                return row(
                    GATE,
                    CheckStatus::Unknown,
                    "the gate did not run: no home directory is known",
                );
            }
            HostVerdict::Refused(e) => {
                return row(
                    GATE,
                    CheckStatus::Unknown,
                    format!("the gate did not run: host ceiling refused: {e}"),
                );
            }
        }
    }
    let mut refusals: BTreeMap<&Path, &GateError> = BTreeMap::new();
    let mut passed: BTreeSet<&Path> = BTreeSet::new();
    for report in &all {
        for finding in &report.findings {
            if let Finding::FileRefused {
                file,
                error: ProjectFileError::Gate { error },
            } = finding
            {
                refusals.entry(file.as_path()).or_insert(error);
            }
        }
        for status in &report.per_file {
            if matches!(
                status.state,
                FileState::Effective { .. } | FileState::Withheld
            ) {
                passed.insert(status.file.as_path());
            }
        }
    }
    if refusals.is_empty() {
        return row(
            GATE,
            CheckStatus::Ok,
            format!("{} route file(s) match the default branch", passed.len()),
        );
    }
    let items = refusals
        .iter()
        .map(|(file, e)| format!("{}: {e} (fix: {})", file.display(), gate_fix(e)))
        .collect();
    row(
        GATE,
        CheckStatus::Warn,
        format!("{} gate refusal(s): {}", refusals.len(), capped(items)),
    )
}

/// The operator's fix for a gate refusal.
fn gate_fix(e: &GateError) -> String {
    match e {
        GateError::NotOnDefaultBranch { default, .. } => format!("git switch {default}"),
        GateError::DetachedHead => "check out the default branch".into(),
        GateError::NotCommitted { .. } | GateError::ContentDiffers { .. } => {
            "commit the file through a reviewed PR to the default branch".into()
        }
        GateError::NotTopLevel => "list the repository's top level, not a subdirectory".into(),
        GateError::NotARepository => "list the top level of a git repository".into(),
        GateError::DefaultBranchUnknown => {
            "set origin/HEAD (git remote set-head origin -a), or keep exactly one of main and master".into()
        }
        GateError::NoDefaultCommit { branch } => format!("commit to {branch}"),
        GateError::GitUnavailable { .. } | GateError::GitFailed { .. } | GateError::GitTimedOut { .. } => {
            "check git in that repository; the gate could not verify the file".into()
        }
        // `GateError` is non_exhaustive.
        _ => "an unrecognised gate refusal; the file is not in effect".into(),
    }
}

#[cfg(test)]
#[path = "doctor_channels_tests.rs"]
mod tests;
