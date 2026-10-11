//! `tm doctor` rows for the trusty-search project map (#9169 B2).
//!
//! Why: trusty-search owns `project-map.toml` (ruling f6), which names one
//! live index per project and parks duplicates (f7). Nothing told an operator
//! when their project had no index, when a group still held a live duplicate,
//! when two main checkouts competed for one repo (#9230), or when the daemon
//! could not read its map and every resolve by project failed closed.
//! What: [`check_search_project`] reads the map once with `search.project.map`,
//! resolves the doctor's own project with `search.project.resolve`, and
//! resolves each pinned index (bounded by [`FanoutLimits`]). Four rows come
//! back: [`NO_INDEX`], [`DUPLICATE`], [`TWO_CHECKOUTS`], [`MAP_UNREADABLE`].
//! Read-only: tm calls no write method and touches no file. An input it could
//! not read is reported as "not checked", never as `Ok`.
//! Test: `doctor_search_project_tests.rs` and `doctor_search_project_e2e_tests.rs`.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;
use trusty_common::uds::server::{CODE_INTERNAL_ERROR, CODE_METHOD_NOT_FOUND};

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::daemon::search_rpc::{self, CODE_CONFLICT, CODE_NOT_FOUND, SearchRpcError};

use super::PROBE_TIMEOUT;

/// The doctor's own project has no live index.
pub(super) const NO_INDEX: &str = "search_project_no_index";
/// A group holds a live worktree, orphaned or same-root duplicate.
pub(super) const DUPLICATE: &str = "search_project_duplicate_index";
/// A group holds a second live main checkout (#9230).
pub(super) const TWO_CHECKOUTS: &str = "search_project_two_checkouts";
/// The daemon cannot read `project-map.toml`.
pub(super) const MAP_UNREADABLE: &str = "search_project_map_unreadable";

/// The map, as stored (B1).
const METHOD_MAP: &str = "search.project.map";
/// A project to its one live index (B1).
const METHOD_RESOLVE: &str = "search.project.resolve";

/// Bounds on the per-group resolve fan-out.
///
/// Why: a daemon with many groups, or one that accepts and never answers,
/// must not hang `tm doctor`.
/// What: at most `max_groups` resolves, each bounded by `per_call`, all
/// inside `budget`. Groups left over are reported as not checked.
/// Test: `a_resolve_that_times_out_stops_the_fanout_and_is_not_ok`,
/// `groups_past_the_cap_are_not_checked_not_ok`.
#[derive(Debug, Clone, Copy)]
pub(super) struct FanoutLimits {
    /// Most pinned groups one run resolves.
    pub max_groups: usize,
    /// Bound on one resolve.
    pub per_call: Duration,
    /// Bound on the whole fan-out.
    pub budget: Duration,
}

/// The limits `tm doctor` runs with.
pub(super) const DEFAULT_LIMITS: FanoutLimits = FanoutLimits {
    max_groups: 64,
    per_call: Duration::from_secs(3),
    budget: Duration::from_secs(15),
};

/// One socket call, classified.
#[derive(Debug, Clone)]
pub(super) enum Rpc {
    /// The daemon's `result`.
    Answer(Value),
    /// The daemon answered with an error frame.
    Refused {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    /// No answer: dial failure, timeout, or a broken frame.
    Unreachable(String),
}

/// Make one bounded call and classify it.
async fn call(socket: &Path, method: &str, params: Value, timeout: Duration) -> Rpc {
    match search_rpc::call_at(socket, method, params, timeout).await {
        Ok(body) => Rpc::Answer(body),
        Err(e) => match e.downcast_ref::<SearchRpcError>() {
            Some(rpc) => Rpc::Refused {
                code: rpc.code,
                message: rpc.message.clone(),
                data: rpc.data.clone(),
            },
            None => Rpc::Unreachable(format!("{e:#}")),
        },
    }
}

/// One `[[projects]]` row of the map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Pin {
    /// The group key (`owner/repo` or `root:<path>`).
    pub key: String,
    /// The pinned index.
    pub index_id: String,
    /// `migration`, `create` or `operator`.
    pub pinned_by: String,
}

/// What `search.project.map` told us.
#[derive(Debug, Clone)]
pub(super) enum MapRead {
    /// The map's path and its pins.
    Readable { path: String, pins: Vec<Pin> },
    /// The daemon answered that it cannot read the map; its message names the
    /// file and the remedy.
    Unreadable(String),
    /// The daemon did not answer at all.
    Down(String),
    /// The daemon answered, but not with a map we could use.
    NotChecked(String),
}

/// Classify a `search.project.map` call.
///
/// What: an internal error is the fail-closed map; a missing method is a
/// daemon older than trusty-search 0.60.0; a body without a `projects` array
/// is malformed. None of them is `Readable`.
/// Test: `unreadable_project_map_is_fail_not_ok`,
/// `a_daemon_without_the_map_method_is_not_checked`.
pub(super) fn read_map(rpc: Rpc) -> MapRead {
    let body = match rpc {
        Rpc::Answer(body) => body,
        Rpc::Unreachable(e) => return MapRead::Down(e),
        Rpc::Refused { code, message, .. } if code == CODE_INTERNAL_ERROR => {
            return MapRead::Unreadable(message);
        }
        Rpc::Refused { code, .. } if code == CODE_METHOD_NOT_FOUND => {
            return MapRead::NotChecked(format!(
                "the daemon has no `{METHOD_MAP}` method; the project map needs \
                 trusty-search 0.60.0 or newer"
            ));
        }
        Rpc::Refused { code, message, .. } => {
            return MapRead::NotChecked(format!("`{METHOD_MAP}` refused ({code}): {message}"));
        }
    };
    let Some(rows) = body.get("projects").and_then(Value::as_array) else {
        return MapRead::NotChecked(format!("`{METHOD_MAP}` answered without a `projects` list"));
    };
    let field = |row: &Value, name: &str| row.get(name).and_then(Value::as_str).map(str::to_string);
    let mut pins = Vec::with_capacity(rows.len());
    for row in rows {
        let (Some(key), Some(index_id)) = (field(row, "key"), field(row, "index_id")) else {
            return MapRead::NotChecked(format!(
                "`{METHOD_MAP}` answered a pin without a key or id"
            ));
        };
        let pinned_by = field(row, "pinned_by").unwrap_or_default();
        pins.push(Pin {
            key,
            index_id,
            pinned_by,
        });
    }
    let path = field(&body, "path").unwrap_or_else(|| "project-map.toml".to_string());
    MapRead::Readable { path, pins }
}

/// One group's live duplicates, sorted by what fixes them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct GroupFinding {
    /// The group key.
    pub key: String,
    /// The group's live answer.
    pub answer: String,
    /// Parkable: worktree, orphaned, or sharing a root with another member.
    pub parkable: Vec<String>,
    /// Not parkable: another main checkout (or an unprobed root).
    pub checkouts: Vec<String>,
    /// Who pinned the answer.
    pub pinned_by: String,
}

/// The fan-out's result.
#[derive(Debug, Clone, Default)]
pub(super) struct Fanout {
    /// One per group that resolved.
    pub groups: Vec<GroupFinding>,
    /// `(key, reason)` per group that did not resolve or was not reached.
    pub unchecked: Vec<(String, String)>,
}

/// Classify one resolve answer for `pin`'s group.
///
/// What: drops parked duplicates; a duplicate that is a worktree, orphaned,
/// or shares its root with another member is parkable; any other live
/// duplicate is a competing checkout.
/// Test: `duplicate_index_group_is_warn`, `a_same_root_sibling_is_a_duplicate_not_a_checkout`,
/// `a_parked_duplicate_is_ok`.
pub(super) fn classify(pin: &Pin, body: &Value) -> GroupFinding {
    let text = |v: &Value, name: &str| {
        v.get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let answer_root = text(body, "root_path");
    let duplicates: Vec<&Value> = body
        .get("duplicates")
        .and_then(Value::as_array)
        .map(|d| d.iter().collect())
        .unwrap_or_default();
    let mut finding = GroupFinding {
        key: pin.key.clone(),
        answer: text(body, "index_id"),
        pinned_by: pin.pinned_by.clone(),
        ..GroupFinding::default()
    };
    for dup in &duplicates {
        if dup.get("parked").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let (id, root, kind) = (
            text(dup, "index_id"),
            text(dup, "root_path"),
            text(dup, "kind"),
        );
        let shares_root = root == answer_root
            || duplicates
                .iter()
                .any(|o| text(o, "index_id") != id && text(o, "root_path") == root);
        let label = format!("`{id}` ({kind}, {root})");
        if shares_root || kind == "worktree" || kind == "orphaned" {
            finding.parkable.push(label);
        } else {
            finding.checkouts.push(label);
        }
    }
    finding
}

/// Resolve each pin's group, bounded by `limits`.
///
/// What: one `search.project.resolve {project: <pinned id>}` per pinned
/// index, in map order. A refusal marks that group unchecked and the
/// fan-out goes on; a call with no answer (timeout, dead socket) stops it and
/// marks every remaining group unchecked, as do the cap and the budget.
/// Test: `a_group_whose_resolve_fails_is_not_ok`,
/// `a_resolve_that_times_out_stops_the_fanout_and_is_not_ok`,
/// `groups_past_the_cap_are_not_checked_not_ok`.
pub(super) async fn resolve_groups(socket: &Path, pins: &[Pin], limits: FanoutLimits) -> Fanout {
    let started = Instant::now();
    let mut out = Fanout::default();
    let mut stop: Option<String> = None;
    for (i, pin) in pins.iter().enumerate() {
        if stop.is_none() && i >= limits.max_groups {
            stop = Some(format!("past the {}-group cap", limits.max_groups));
        }
        let left = limits.budget.saturating_sub(started.elapsed());
        if stop.is_none() && left.is_zero() {
            stop = Some(format!("past the {}s budget", limits.budget.as_secs()));
        }
        if let Some(reason) = &stop {
            out.unchecked.push((pin.key.clone(), reason.clone()));
            continue;
        }
        let params = serde_json::json!({ "project": pin.index_id });
        match call(socket, METHOD_RESOLVE, params, limits.per_call.min(left)).await {
            Rpc::Answer(body) => out.groups.push(classify(pin, &body)),
            Rpc::Refused { code, message, .. } => out.unchecked.push((
                pin.key.clone(),
                format!("resolve refused ({code}): {message}"),
            )),
            Rpc::Unreachable(e) => {
                out.unchecked
                    .push((pin.key.clone(), format!("no answer: {e}")));
                stop = Some("the daemon stopped answering".to_string());
            }
        }
    }
    out
}

/// Run the four project-map rows against the daemon's socket.
///
/// Why: the entry `run_doctor` calls; see the module doc.
/// What: resolves the socket, then [`check_search_project_at`].
/// Test: `duplicate_index_group_is_warn`, `unreadable_project_map_is_fail_not_ok`.
pub(super) async fn check_search_project(project_dir: Option<&Path>) -> Vec<DoctorCheck> {
    match search_rpc::search_socket() {
        Ok(socket) => check_search_project_at(&socket, project_dir, DEFAULT_LIMITS).await,
        Err(e) => all_not_checked(&format!("cannot resolve the trusty-search socket: {e:#}")),
    }
}

/// [`check_search_project`] against an explicit socket and limits.
///
/// What: reads the map; a daemon that does not answer makes all four rows
/// Warn "not checked" with no further call. Otherwise the doctor's project is
/// resolved and, when the map is readable, every pinned group.
/// Test: `a_healthy_daemon_reports_four_ok_rows`, `a_down_daemon_is_not_checked_on_every_row`.
pub(super) async fn check_search_project_at(
    socket: &Path,
    project_dir: Option<&Path>,
    limits: FanoutLimits,
) -> Vec<DoctorCheck> {
    let map = read_map(call(socket, METHOD_MAP, serde_json::json!({}), PROBE_TIMEOUT).await);
    if let MapRead::Down(e) = &map {
        return all_not_checked(&format!(
            "trusty-search did not answer at {} ({e})",
            socket.display()
        ));
    }
    let no_index = match project_dir {
        Some(dir) => {
            let root = trusty_common::resolve_project_root(dir);
            let params = serde_json::json!({ "project": root.to_string_lossy() });
            let rpc = call(socket, METHOD_RESOLVE, params, PROBE_TIMEOUT).await;
            build_no_index(Some(&root), Some(&rpc))
        }
        None => build_no_index(None, None),
    };
    let fanout = match &map {
        MapRead::Readable { pins, .. } => Some(resolve_groups(socket, pins, limits).await),
        _ => None,
    };
    let rows = build_group_rows(&map, fanout.as_ref());
    let mut out = vec![no_index];
    out.extend(rows);
    out
}

/// The four rows, each Warn "not checked" for `reason`.
fn all_not_checked(reason: &str) -> Vec<DoctorCheck> {
    [NO_INDEX, DUPLICATE, TWO_CHECKOUTS, MAP_UNREADABLE]
        .into_iter()
        .map(|name| DoctorCheck::new(name, CheckStatus::Warn, format!("not checked: {reason}")))
        .collect()
}

/// `data.error` of a refusal, when the daemon sent one.
fn error_kind(data: Option<&Value>) -> Option<&str> {
    data.and_then(|d| d.get("error")).and_then(Value::as_str)
}

/// The [`NO_INDEX`] row for the doctor's project `root` (pure).
///
/// What: `Ok` when the root resolves to an index; `Warn` when no index covers
/// it, when only worktree or orphaned indexes do (`no_live_index`), or when it
/// matches more than one repo; `Warn` "not checked" with no project, with no
/// answer, or with any other refusal — never `Ok` without an index id.
/// Test: `a_project_with_no_index_is_warn`,
/// `a_project_with_only_worktree_indexes_is_warn`,
/// `a_down_daemon_is_not_checked_on_every_row`.
pub(super) fn build_no_index(root: Option<&Path>, rpc: Option<&Rpc>) -> DoctorCheck {
    let (Some(root), Some(rpc)) = (root, rpc) else {
        return DoctorCheck::new(
            NO_INDEX,
            CheckStatus::Warn,
            "not checked: no project directory supplied",
        );
    };
    let at = root.display();
    let (status, message) = match rpc {
        Rpc::Answer(body) => match body.get("index_id").and_then(Value::as_str) {
            Some(id) => (
                CheckStatus::Ok,
                format!("{at} resolves to trusty-search index `{id}`"),
            ),
            None => (
                CheckStatus::Warn,
                format!("not checked: resolving {at} answered without an index id"),
            ),
        },
        Rpc::Refused { code, data, .. }
            if *code == CODE_NOT_FOUND && error_kind(data.as_ref()) == Some("no_live_index") =>
        {
            (
                CheckStatus::Warn,
                format!(
                    "{at} has only worktree or orphaned trusty-search indexes, so no live index \
                     answers for it. Index its main checkout with `trusty-search index <path>`"
                ),
            )
        }
        Rpc::Refused { code, .. } if *code == CODE_NOT_FOUND => (
            CheckStatus::Warn,
            format!(
                "no trusty-search index covers {at}, so a search by this project misses. \
                 Create one with `trusty-search index {at}`"
            ),
        ),
        Rpc::Refused { code, message, .. } if *code == CODE_CONFLICT => (
            CheckStatus::Warn,
            format!("{at} matches more than one repo's indexes: {message}"),
        ),
        Rpc::Refused { code, message, .. } if *code == CODE_INTERNAL_ERROR => (
            CheckStatus::Warn,
            format!(
                "not checked: the daemon could not resolve {at} ({message}); see `{MAP_UNREADABLE}`"
            ),
        ),
        Rpc::Refused { code, message, .. } => (
            CheckStatus::Warn,
            format!("not checked: resolving {at} was refused ({code}): {message}"),
        ),
        Rpc::Unreachable(e) => (
            CheckStatus::Warn,
            format!("not checked: resolving {at} got no answer ({e})"),
        ),
    };
    DoctorCheck::new(NO_INDEX, status, message)
}

/// At most five items, then a count of the rest.
fn summarize(items: &[String]) -> String {
    let mut out = items.iter().take(5).cloned().collect::<Vec<_>>().join("; ");
    if items.len() > 5 {
        out.push_str(&format!("; and {} more", items.len() - 5));
    }
    out
}

/// One group row from its findings (pure).
///
/// What: `Warn` naming each flagged group and the remedy; else `Warn` "not
/// checked" when any group did not resolve; else `Ok`.
fn group_row(
    name: &str,
    flagged: &[String],
    remedy: &str,
    unchecked: &[String],
    ok: String,
) -> DoctorCheck {
    let note = (!unchecked.is_empty()).then(|| {
        format!(
            "not checked for {} group(s): {}",
            unchecked.len(),
            summarize(unchecked)
        )
    });
    if !flagged.is_empty() {
        let mut msg = format!("{}. {remedy}", summarize(flagged));
        if let Some(note) = &note {
            msg.push_str(&format!("; {note}"));
        }
        DoctorCheck::new(name, CheckStatus::Warn, msg)
    } else if let Some(note) = note {
        DoctorCheck::new(name, CheckStatus::Warn, note)
    } else {
        DoctorCheck::new(name, CheckStatus::Ok, ok)
    }
}

/// The [`DUPLICATE`], [`TWO_CHECKOUTS`] and [`MAP_UNREADABLE`] rows (pure).
///
/// Why: these three read the whole map, so they share one read and one
/// fan-out; a map that was not read leaves the first two "not checked".
/// What: [`MAP_UNREADABLE`] is `Fail` on the daemon's fail-closed answer and
/// `Ok` only on a readable map. A group with a live parkable duplicate flags
/// [`DUPLICATE`] (remedy `project park`). A group with a second live checkout
/// flags [`TWO_CHECKOUTS`] (remedy `project pin`) unless an operator pinned
/// its answer — the pin is that remedy, so it clears the row.
/// Test: `duplicate_index_group_is_warn`, `unreadable_project_map_is_fail_not_ok`,
/// `two_main_checkouts_warn_until_an_operator_pins_one`,
/// `a_group_whose_resolve_fails_is_not_ok`.
pub(super) fn build_group_rows(map: &MapRead, fanout: Option<&Fanout>) -> [DoctorCheck; 3] {
    let map_row = match map {
        MapRead::Readable { path, pins } => DoctorCheck::new(
            MAP_UNREADABLE,
            CheckStatus::Ok,
            format!("{path} is readable; {} project(s) pinned", pins.len()),
        ),
        MapRead::Unreadable(msg) => DoctorCheck::new(
            MAP_UNREADABLE,
            CheckStatus::Fail,
            format!(
                "trusty-search cannot read its project map: {msg}. Every resolve by project \
                 name, owner/repo or path fails until it is repaired; `trusty-search project \
                 rebuild-map --yes` moves the file aside and migrates again"
            ),
        ),
        MapRead::Down(reason) | MapRead::NotChecked(reason) => DoctorCheck::new(
            MAP_UNREADABLE,
            CheckStatus::Warn,
            format!("not checked: {reason}"),
        ),
    };
    let Some(fanout) = fanout.filter(|_| matches!(map, MapRead::Readable { .. })) else {
        let reason = format!("not checked: the project map was not read; see `{MAP_UNREADABLE}`");
        return [
            DoctorCheck::new(DUPLICATE, CheckStatus::Warn, reason.clone()),
            DoctorCheck::new(TWO_CHECKOUTS, CheckStatus::Warn, reason),
            map_row,
        ];
    };
    let unchecked: Vec<String> = fanout
        .unchecked
        .iter()
        .map(|(key, why)| format!("`{key}` ({why})"))
        .collect();
    let resolved = fanout.groups.len();
    let duplicates: Vec<String> = fanout
        .groups
        .iter()
        .filter(|g| !g.parkable.is_empty())
        .map(|g| {
            format!(
                "group `{}` (live `{}`) also holds {}",
                g.key,
                g.answer,
                g.parkable.join(", ")
            )
        })
        .collect();
    let checkouts: Vec<String> = fanout
        .groups
        .iter()
        // #9169: an operator pin is the remedy for two checkouts, so it clears the row.
        .filter(|g| !g.checkouts.is_empty() && g.pinned_by != "operator")
        .map(|g| {
            format!(
                "group `{}` has more than one live checkout: `{}` (pinned by {}) and {}",
                g.key,
                g.answer,
                if g.pinned_by.is_empty() {
                    "nobody"
                } else {
                    &g.pinned_by
                },
                g.checkouts.join(", ")
            )
        })
        .collect();
    [
        group_row(
            DUPLICATE,
            &duplicates,
            "Park each duplicate with `trusty-search project park <id>`",
            &unchecked,
            format!(
                "{resolved} group(s) resolved; none holds a live worktree, orphaned or same-root duplicate"
            ),
        ),
        group_row(
            TWO_CHECKOUTS,
            &checkouts,
            "Choose the one to answer with `trusty-search project pin <id>`",
            &unchecked,
            format!(
                "{resolved} group(s) resolved; none has a second live checkout without an operator pin"
            ),
        ),
        map_row,
    ]
}

#[cfg(test)]
#[path = "doctor_search_project_tests.rs"]
mod tests;
