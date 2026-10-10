//! The pure merge of the host ceiling and the project route files.
//!
//! Why: a route takes effect only when its file is sound, its channel is
//! enabled, its project is listed for that channel, and it is no looser
//! than the ceiling (#8454 plan §3.4). Every other route is dropped with a
//! named finding. Keeping this pure (no I/O, no clock, no env) lets every
//! rule be tested from strings.
//! What: [`merge`] builds a [`LoadReport`]. Fault isolation follows the Q1
//! ruling: a host fault or a cross-file overlap denies all; a broken file
//! refuses only its own routes.
//! Test: `src/policy/tests/merge.rs`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::gchat::routes::Connection;
use crate::policy::error::PolicyError;
use crate::policy::host::{HostCeiling, HostChannel, HostError};
use crate::policy::project_file::{ProjectFile, ProjectFileError, ProjectRoute};
use crate::policy::report::{FileState, FileStatus, Finding, FindingScope, LoadReport, Origin};
use crate::policy::table::{ChannelPolicy, PolicySpec};
use crate::policy::types::{Channel, RateLimitSpec, RouteSpec};

/// One project's route file, already read and parsed (S2b does the I/O).
///
/// Why: the merge needs to know which project a file speaks for, to check
/// the per-channel `projects` list (Architect Q7), and where it lives, to
/// name it in findings.
/// What: the project directory as listed in the host ceiling, the file
/// path, and the parse result; a parse error refuses only this file.
/// Test: `broken_file_in_one_project_leaves_other_project_effective`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectInput {
    /// The project directory, compared by path components with the
    /// ceiling's `projects` entries.
    pub project_dir: PathBuf,
    /// The route file path, for findings.
    pub file: PathBuf,
    /// The parse result.
    pub parsed: Result<ProjectFile, ProjectFileError>,
}

/// Merge the host ceiling with every project's routes.
///
/// Why: the single place the two-file model becomes one enforceable
/// policy, so no consumer re-implements a rule (#8454 plan §3.4, Q1, Q2).
/// What: a host error denies all. For each file, in order: a parse error
/// refuses it; S1's [`ChannelPolicy::build`] over all its routes against
/// the host default refuses it on any rule (a looser rate limit is reported
/// as a widening); a `[gchat.connection]` that differs from the host's is a
/// widening. Then each route is dropped when its channel is absent or
/// disabled (`ChannelDisabled`) or its project is not listed for that
/// channel (`ProjectNotListed`); a kept route with a kind outside the
/// channel's ceiling is a widening; kept gchat routes with no connection in
/// the host or the file refuse the file. Any widening refuses the whole
/// file. Last, one build over every kept route: an overlap there is across
/// files and denies the whole load, naming both files.
/// Test: `host_unknown_key_denies_all`, `no_channels_section_denies_all`,
/// `disabled_channel_drops_routes_with_finding`,
/// `unlisted_project_has_no_routes`, `widening_rate_limit_fails_that_file_only`,
/// `kind_outside_ceiling_fails_file`, `connection_mismatch_fails_file`,
/// `overlap_across_files_names_both_files`, `review_notice_on_slack_fails_file`,
/// `broken_file_in_one_project_leaves_other_project_effective`,
/// `host_faults_deny_all`, `gchat_routes_without_any_connection_fail_file`.
pub fn merge(host: Result<HostCeiling, HostError>, projects: Vec<ProjectInput>) -> LoadReport {
    merge_for(host, projects, &Channel::ALL)
}

/// [`merge`] for a consumer that serves only `channels`.
///
/// Why: S2a critic (#8454 S2b plan §8): the combined overlap build must span
/// only the consumer's channels, so two projects routing gchat to one person
/// do not deny the daemon's Slack/Telegram load. A dir listed for two
/// channels must not overlap with itself.
/// What: inputs naming a file already seen are dropped (first kept). Each
/// file is checked over all its routes exactly as in [`merge`]; only routes
/// on `channels` enter the policy and the combined build.
/// Test: `combined_overlap_spans_only_the_consumer_channels`,
/// `duplicate_project_input_does_not_overlap_itself`.
pub fn merge_for(
    host: Result<HostCeiling, HostError>,
    mut projects: Vec<ProjectInput>,
    channels: &[Channel],
) -> LoadReport {
    // #8454 S2b §8: one file read twice must not overlap with itself.
    let mut seen = HashSet::new();
    projects.retain(|p| seen.insert(p.file.clone()));
    let host = match host {
        Ok(h) => h,
        Err(error) => {
            let per_file = projects
                .iter()
                .map(|p| FileStatus::new(&p.project_dir, &p.file, FileState::Withheld))
                .collect();
            return LoadReport::deny_all(Finding::HostRefused { error }, Vec::new(), per_file);
        }
    };
    let default = host.rate_limit();
    let default = RateLimitSpec {
        limit: i64::from(default.limit()),
        window_secs: i64::from(default.window_secs()),
    };
    let mut findings = Vec::new();
    let mut per_file = Vec::with_capacity(projects.len());
    let mut kept: Vec<(RouteSpec, Origin)> = Vec::new();
    for input in projects {
        let outcome = merge_file(&host, default, input);
        findings.extend(outcome.findings);
        per_file.push(outcome.status);
        // #8454 S2b §8: the per-file build above spans every route; the
        // combined build spans the consumer's channels only.
        kept.extend(
            outcome
                .routes
                .into_iter()
                .filter(|(s, _)| channels.contains(&s.channel)),
        );
    }
    let origins = origin_map(kept.iter().map(|(s, o)| (s, o.clone())));
    let spec = PolicySpec {
        rate_limit: Some(default),
        routes: kept.into_iter().map(|(s, _)| s).collect(),
    };
    match ChannelPolicy::build(spec) {
        Ok(policy) => LoadReport {
            policy,
            denied: false,
            findings,
            per_file,
        },
        // #8454 Q1: every file passed alone, so this is a cross-file overlap.
        // It denies the whole load, naming both files.
        Err(e) => {
            let finding = attribute(e, &origins, Path::new(""));
            LoadReport::deny_all(finding, findings, per_file)
        }
    }
}

struct FileOutcome {
    status: FileStatus,
    findings: Vec<Finding>,
    routes: Vec<(RouteSpec, Origin)>,
}

fn merge_file(host: &HostCeiling, default: RateLimitSpec, input: ProjectInput) -> FileOutcome {
    let ProjectInput {
        project_dir,
        file,
        parsed,
    } = input;
    let refused = |findings: Vec<Finding>| FileOutcome {
        status: FileStatus::new(&project_dir, &file, FileState::Refused),
        findings,
        routes: Vec::new(),
    };
    let parsed = match parsed {
        Ok(p) => p,
        Err(error) => {
            let file = file.clone();
            return refused(vec![Finding::FileRefused { file, error }]);
        }
    };
    // S1 rules over the whole file, so a bad route fails its file even when
    // its channel is off.
    let origins = origin_map(
        parsed
            .routes
            .iter()
            .map(|r| (&r.spec, Origin::new(&file, &r.entry))),
    );
    let spec = PolicySpec {
        rate_limit: Some(default),
        routes: parsed.routes.iter().map(|r| r.spec.clone()).collect(),
    };
    if let Err(e) = ChannelPolicy::build(spec) {
        return refused(vec![attribute(e, &origins, &file)]);
    }

    let mut findings = Vec::new();
    // #8454 Q2: the host connection, when present, is authoritative.
    let host_conn = host
        .channel(Channel::Gchat)
        .and_then(HostChannel::gchat_connection);
    if let (Some(h), Some(p)) = (host_conn, parsed.gchat_connection.as_ref()) {
        if let Some(field) = connection_diff(h, p) {
            findings.push(Finding::Widening {
                origin: Origin::new(&file, "gchat.connection"),
                field: "gchat.connection",
                detail: format!("{field} differs from the host connection"),
            });
        }
    }
    let connection = host_conn.or(parsed.gchat_connection.as_ref()).cloned();

    let mut keep: Vec<ProjectRoute> = Vec::new();
    for route in parsed.routes {
        let origin = Origin::new(&file, &route.entry);
        let channel = route.spec.channel;
        match host.channel(channel).filter(|c| c.enabled()) {
            None => findings.push(Finding::ChannelDisabled { origin, channel }),
            Some(c) if !c.lists(&project_dir) => findings.push(Finding::ProjectNotListed {
                origin,
                channel,
                project: project_dir.clone(),
            }),
            Some(c) => match route.spec.kinds.iter().find(|k| !c.kinds().contains(*k)) {
                Some(kind) => findings.push(Finding::Widening {
                    origin,
                    field: "kinds",
                    detail: format!("kind {kind} is outside the {channel} ceiling"),
                }),
                None => keep.push(route),
            },
        }
    }
    let gchat_kept = keep.iter().any(|r| r.spec.channel == Channel::Gchat);
    if gchat_kept && connection.is_none() {
        findings.push(Finding::NoGchatConnection { file: file.clone() });
    }
    // #8454 Q1: any file-scoped finding refuses this file only.
    if findings.iter().any(|f| f.scope() != FindingScope::Route) {
        return refused(findings);
    }

    let mut status = FileStatus::new(
        &project_dir,
        &file,
        FileState::Effective { routes: keep.len() },
    );
    if gchat_kept {
        status.gchat_connection = connection;
    }
    let mut routes = Vec::with_capacity(keep.len());
    for r in keep {
        if let Some(space) = r.space {
            status.gchat_spaces.insert(r.spec.name.clone(), space);
        }
        let origin = Origin::new(&file, r.entry);
        routes.push((r.spec, origin));
    }
    FileOutcome {
        status,
        findings,
        routes,
    }
}

/// The first connection field that differs, by name; values are not shown.
fn connection_diff(host: &Connection, project: &Connection) -> Option<&'static str> {
    if host.project_id != project.project_id {
        Some("project_id")
    } else if host.subscription != project.subscription {
        Some("subscription")
    } else if host.key_file != project.key_file {
        Some("key_file")
    } else {
        None
    }
}

/// S1's entry text for input route `i`, as `ChannelPolicy::build` writes it.
///
/// Why: a [`PolicyError`] indexes the merged spec list; this maps the index
/// back to the file and entry the operator edits.
/// What: `routes[<i>] <channel> "<name>"`, matching `table.rs`.
/// Test: `policy_errors_name_the_file_entry`.
fn s1_entry(i: usize, spec: &RouteSpec) -> String {
    format!("routes[{i}] {} {:?}", spec.channel, spec.name)
}

fn origin_map<'a>(
    routes: impl Iterator<Item = (&'a RouteSpec, Origin)>,
) -> HashMap<String, Origin> {
    routes
        .enumerate()
        .map(|(i, (spec, origin))| (s1_entry(i, spec), origin))
        .collect()
}

/// Turn an S1 build error into a finding that names the file entry.
///
/// Why: S1 errors name `routes[i]`; an operator needs `(file, entry)`.
/// What: a duplicate becomes an `Overlap` naming both origins; a looser
/// rate limit a `Widening`; any other rule a `RouteRejected`. An entry not
/// in `origins` keeps S1's text, attributed to `fallback`.
/// Test: `policy_errors_name_the_file_entry`, `unmapped_policy_error_keeps_raw_entry`.
pub(crate) fn attribute(
    err: PolicyError,
    origins: &HashMap<String, Origin>,
    fallback: &Path,
) -> Finding {
    let origin_of = |entry: &str| {
        origins
            .get(entry)
            .cloned()
            .unwrap_or_else(|| Origin::new(fallback, entry))
    };
    match err {
        PolicyError::Duplicate {
            first,
            second,
            field,
            value,
        } => Finding::Overlap {
            first: origin_of(&first),
            second: origin_of(&second),
            field,
            value,
        },
        PolicyError::RateLimitRaised {
            entry,
            field,
            route,
            default,
        } => Finding::Widening {
            origin: origin_of(&entry),
            field: "rate_limit",
            detail: format!("{field} {route} is looser than the host default {default}"),
        },
        PolicyError::EmptyKinds { ref entry }
        | PolicyError::KindNotOnChannel { ref entry, .. }
        | PolicyError::InvalidRoute { ref entry, .. }
        | PolicyError::InvalidRateLimit { ref entry, .. } => Finding::RouteRejected {
            origin: origin_of(entry),
            error: err.clone(),
        },
    }
}
