//! `tm session disk` — per-session disk usage (#7313 slice 2).
//!
//! Why: #7313 slice 1 taught the `disk_survey` MCP tool to attribute every
//! managed worktree to the session that created it and to roll the rows up by
//! session. Nothing in the shell could ask for that — an operator wanting to
//! know which session is holding the bytes had the console Disk view or
//! nothing. This is the CLI half, beside `ls`/`rename`/`pause`/`resume`/`stop`
//! where session lifecycle already lives.
//! What: [`session_disk`] issues ONE `disk_survey` call over the daemon's
//! loopback `POST /rpc` with `group_by: "session"`, then renders what came
//! back. It runs no survey of its own: the walk, the classification, the
//! attribution and the by-session fold are all slice 1's, in the daemon, where
//! the shared size index and the live claim set are. A second walker here
//! would be a second answer to the same question. The survey attributes by raw
//! session UUID, so `<id-or-name>` is resolved against the daemon's session
//! list through the canonical [`resolve_target`] before anything is filtered by
//! it, and that same list labels the listing's rows.
//! Test: `session_disk_tests`, and `crates/trusty-mpm/tests/tm_session_disk_cli.rs`
//! for the built binary against a stub daemon.
//!
//! READ-ONLY. Nothing here removes, prunes, or writes anything.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use trusty_mpm::client::{Resolvable, resolve_target};
use trusty_mpm::disk::human_bytes;

#[cfg(test)]
#[path = "session_disk_tests.rs"]
mod session_disk_tests;

/// `tm session disk [<id-or-name>] [--json]` (#7313).
///
/// Why: see the module docs.
/// What: resolves the argument to a session id, calls the daemon once, folds
/// the response into [`DiskReport`], and prints it — JSON when `json`,
/// otherwise the table. An argument naming no session and a session holding
/// nothing are two DIFFERENT errors, so a typo can never read as "that session
/// holds nothing".
/// Test: `cli_parses_session_disk`, `sessions_report_sorts_by_bytes_descending`,
/// `an_unknown_session_is_an_error`, and the binary-level
/// `session_disk_json_reports_every_session` / `session_disk_rejects_an_unknown_session`.
pub(crate) async fn session_disk(
    client: &reqwest::Client,
    url: &str,
    id_or_name: Option<String>,
    json_out: bool,
    budget_seconds: Option<u64>,
) -> anyhow::Result<()> {
    // #7313: the survey attributes by UUID, so the operator's `<id-or-name>`
    // is resolved against the daemon's session list BEFORE anything is filtered
    // by it. The same list supplies the friendly names the listing renders.
    let directory = session_directory(client, url).await;
    let target = match id_or_name {
        Some(ref arg) => Some(resolve_session_id(&directory, arg)?),
        None => None,
    };
    // A named session's worktrees are not confined to one repository — an
    // isolation worktree, an install/verify throwaway tree and a `jobs/<id>/`
    // tree can sit under three — so the breakdown surveys the whole workspace.
    // The listing scopes to the current project, which is what an operator
    // standing in a checkout is asking about.
    let project = match target {
        Some(_) => None,
        None => current_project_filter(&std::env::current_dir()?),
    };
    let survey = fetch_survey(client, url, project.as_deref(), budget_seconds).await?;
    let names = session_names(&directory);
    let report = match target {
        Some(ref id) => session_report(&survey, id, &names)?,
        None => sessions_report(&survey, project.clone(), &names),
    };
    if json_out {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render(&report));
    }
    if survey.partial {
        // stderr, so a `--json` consumer's stdout stays a clean document while
        // the operator still learns the figures are a floor rather than a total.
        eprintln!(
            "warning: the survey hit its {}s budget — every figure here is a floor, not a total",
            budget_seconds.unwrap_or(trusty_mpm::daemon::mcp_disk::MAX_BUDGET_SECONDS)
        );
    }
    Ok(())
}

/// UUID → friendly name for the sessions the daemon still lists.
pub(crate) type SessionNames = HashMap<String, String>;

/// One session the daemon knows, flattened from whichever store holds it.
///
/// Why one row type over both stores: `owning_session` can be either a
/// `ManagedSessionId` (a session worktree's sentinel, and the case every
/// attributed row on a live machine turns out to be) or a project `SessionId`
/// (an agent sentinel's `parent_session_id`). Flattening them means the ONE
/// [`resolve_target`] sees every candidate at once, so an id-exact match always
/// outranks a name-exact match in the other store — which two separate lookups,
/// tried in order, would get backwards.
/// Test: `a_friendly_name_resolves_to_its_session_uuid`.
#[derive(Debug, Clone)]
pub(crate) struct DirectoryRow {
    /// The session's canonical id, as the survey attributes by.
    pub id: String,
    /// Its friendly name — `tmux_name` for a project session, `name` for a
    /// managed one.
    pub name: String,
}

impl Resolvable for DirectoryRow {
    fn id_matches(&self, query: &str) -> bool {
        self.id == query
    }

    fn resolve_name(&self) -> &str {
        &self.name
    }
}

/// Every session the daemon knows, for id-or-name resolution and labelling.
///
/// Why: `disk_survey` charges every worktree to a raw session UUID (slice 1
/// reads `owner_session_id` or `agent.parent_session_id`), and neither an
/// operator nor the listing wants to read UUIDs. Both stores are asked because
/// either can hold the attributed id; the lists are also what labels the rows,
/// so the same fetch serves the resolution and the table.
/// What: the managed list and the project list, through the shared
/// [`DaemonClient`] — the same two calls
/// [`resolve_managed_summary`](super::managed_route::resolve_managed_summary)
/// and [`resolve_project_session_id`](super::managed_route::resolve_project_session_id)
/// make. An unreachable store contributes nothing: the survey call that follows
/// reports an outage far better than a name lookup could, so this declines
/// rather than pre-empting it.
/// Test: the binary-level `session_disk_resolves_a_friendly_name` drives the
/// real HTTP path against a stub daemon.
///
/// [`DaemonClient`]: trusty_mpm::client::DaemonClient
async fn session_directory(client: &reqwest::Client, url: &str) -> Vec<DirectoryRow> {
    let executor = super::managed_route::executor(client, url);
    let managed = executor
        .client()
        .list_managed_sessions()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|s| DirectoryRow {
            id: s.id,
            name: s.name,
        });
    let project = executor
        .client()
        .sessions()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|r| DirectoryRow {
            id: r.id.0.to_string(),
            name: r.tmux_name,
        });
    managed.chain(project).collect()
}

/// Resolve the operator's `<id-or-name>` to the id the survey attributes by.
///
/// Why: `owning_session` is ALWAYS a UUID, so matching the operator's string
/// against it directly meant a friendly name resolved to nothing and drew the
/// same "no worktree attributed" message as a session that genuinely holds
/// none — two different facts, one message, and the operator cannot tell a typo
/// from an empty session. Resolution goes through the one canonical
/// [`resolve_target`] (id-exact → name-exact → unambiguous prefix), the same
/// precedence `stop`, `resume` and `events` already use.
/// What: the matched row's UUID. A UUID the daemon no longer lists passes
/// through unchanged, because an ENDED session's leftovers are still attributed
/// by their ownership sentinel and are exactly what an operator asks about.
/// Anything else is an error naming the argument, distinct from the
/// holds-nothing arm in [`session_report`].
/// Test: `a_friendly_name_resolves_to_its_session_uuid`,
/// `an_ended_sessions_uuid_resolves_without_the_daemon_listing_it`,
/// `an_unknown_name_is_a_distinct_error`.
fn resolve_session_id(directory: &[DirectoryRow], id_or_name: &str) -> anyhow::Result<String> {
    if let Some(row) = resolve_target(directory, id_or_name) {
        return Ok(row.id.clone());
    }
    if uuid::Uuid::parse_str(id_or_name).is_ok() {
        return Ok(id_or_name.to_string());
    }
    bail!(
        "no session named `{id_or_name}` — `tm sessions ls` lists the sessions \
         the daemon knows, and `tm session disk` with no argument lists the \
         ones holding bytes"
    )
}

/// Index the directory by UUID so a row can be labelled by name.
///
/// Test: `the_listing_renders_the_friendly_name_when_one_is_known`.
fn session_names(directory: &[DirectoryRow]) -> SessionNames {
    directory
        .iter()
        .filter(|row| !row.name.is_empty())
        .map(|row| (row.id.clone(), row.name.clone()))
        .collect()
}

/// The `<owner>/<repo>` label for the project the caller is standing in.
///
/// Why: `disk_survey`'s `project` filter matches that label, and deriving it
/// from the path is what makes the answer right from inside an agent worktree
/// — `<root>/<owner>/<repo>/.claude/worktrees/<x>` is still that project.
/// What: the first two components of `cwd` relative to the managed workspace
/// root, or `None` when the caller is outside it, which surveys everything
/// rather than filtering to a project that does not exist.
/// Test: `a_worktree_cwd_still_names_its_project`,
/// `a_cwd_outside_the_workspace_root_has_no_filter`.
fn current_project_filter(cwd: &Path) -> Option<String> {
    let root = trusty_mpm::core::trusty_tools_config::workspace_root(
        &trusty_mpm::core::trusty_tools_config::TrustyToolsConfig::load(),
    );
    project_filter_within(cwd, &root)
}

/// The pure core of [`current_project_filter`], with the root injected.
///
/// Test: `a_worktree_cwd_still_names_its_project`,
/// `a_cwd_outside_the_workspace_root_has_no_filter`.
fn project_filter_within(cwd: &Path, root: &Path) -> Option<String> {
    let rest = cwd.strip_prefix(root).ok()?;
    let mut parts = rest.components();
    let owner = parts.next()?.as_os_str().to_string_lossy().into_owned();
    let repo = parts.next()?.as_os_str().to_string_lossy().into_owned();
    Some(format!("{owner}/{repo}"))
}

/// Call `disk_survey` once, with the per-session roll-up asked for.
///
/// Why: `POST /rpc` is the daemon's MCP dispatch, and DOC-73 §16.4 makes the
/// tool the only way in — there is no HTTP route for the survey, and adding one
/// would be a second surface over one implementation.
/// What: a `tools/call` envelope; the tool's result arrives as a JSON document
/// inside an MCP text block, which is unwrapped here. `budget_seconds` defaults
/// to the daemon's clamp so the pass always finishes inside
/// [`DISK_SURVEY_REQUEST_TIMEOUT`](trusty_mpm::client::http_client::DISK_SURVEY_REQUEST_TIMEOUT).
/// Test: the binary-level `session_disk_json_reports_every_session` drives this
/// against a stub `POST /rpc`; `a_tool_error_is_reported_not_swallowed` covers
/// the `isError` arm.
async fn fetch_survey(
    client: &reqwest::Client,
    url: &str,
    project: Option<&str>,
    budget_seconds: Option<u64>,
) -> anyhow::Result<Survey> {
    let mut arguments = json!({
        "group_by": "session",
        "budget_seconds": budget_seconds
            .unwrap_or(trusty_mpm::daemon::mcp_disk::MAX_BUDGET_SECONDS),
    });
    if let (Some(project), Value::Object(map)) = (project, &mut arguments) {
        map.insert("project".to_string(), Value::String(project.to_string()));
    }
    let body: Value = client
        .post(format!("{url}/rpc"))
        .timeout(trusty_mpm::client::http_client::DISK_SURVEY_REQUEST_TIMEOUT)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "disk_survey", "arguments": arguments },
        }))
        .send()
        .await
        .with_context(|| format!("POST {url}/rpc: the trusty-mpm daemon is not reachable"))?
        .error_for_status()?
        .json()
        .await
        .context("the daemon's disk_survey response was not JSON")?;
    survey_from_rpc(&body)
}

/// Unwrap one `tools/call` response into the survey it carries.
///
/// Why: separated from the request so the three failure shapes — a JSON-RPC
/// error, an `isError` tool result, and a payload that will not deserialize —
/// are testable without a daemon. Each becomes a distinct message; none becomes
/// an empty report, which would read as "nothing on disk".
/// Test: `a_tool_error_is_reported_not_swallowed`,
/// `a_jsonrpc_error_is_reported_not_swallowed`, `a_survey_payload_round_trips`.
fn survey_from_rpc(body: &Value) -> anyhow::Result<Survey> {
    if let Some(err) = body.get("error") {
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown daemon error");
        bail!("disk_survey failed: {message}");
    }
    let text = body
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the daemon returned no disk_survey result"))?;
    if body
        .pointer("/result/isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        bail!("disk_survey failed: {text}");
    }
    serde_json::from_str(text).context("the daemon's disk_survey payload did not parse")
}

// ── The survey, as this client reads it ─────────────────────────────────────
// Only the fields the report needs. Serde ignores the rest, so a field slice 1
// or a later slice adds costs nothing here.

/// The `disk_survey` payload, narrowed to what this command renders.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Survey {
    /// When the survey ran, RFC 3339.
    pub generated_at: String,
    /// Whether the daemon's deadline truncated the pass.
    pub partial: bool,
    /// The per-session roll-up, present because `group_by: "session"` was asked
    /// for. `None` would mean a daemon predating slice 1.
    pub by_session: Option<Vec<SurveyGroup>>,
    /// The per-project tree, which is where the individual worktree rows are.
    pub root: SurveyRoot,
}

/// The scanned workspace root.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SurveyRoot {
    /// Every project the survey covered.
    pub projects: Vec<SurveyProject>,
}

/// One project and its worktrees.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SurveyProject {
    /// `<owner>/<repo>`.
    pub name: String,
    /// The worktrees under it.
    pub worktrees: Vec<SurveyWorktree>,
}

/// One worktree row.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SurveyWorktree {
    /// The worktree directory.
    pub path: PathBuf,
    /// Which tier it renders in — `stale`, `review`, `keep`, `missing`.
    pub tier: String,
    /// Bytes on disk, absent when the index could not measure it.
    pub bytes: Option<u64>,
    /// Bytes held by its top-level `target*` directories (#7313 slice 1).
    pub build_dir_bytes: Option<u64>,
    /// The session these bytes are charged to.
    pub owning_session: Option<String>,
}

/// One session's roll-up, as slice 1's `group_by_session` produced it.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SurveyGroup {
    /// The owning session, `None` for the unattributed bucket.
    pub session_id: Option<String>,
    /// Bytes across this session's measured worktrees.
    pub bytes: u64,
    /// Build-directory bytes across them.
    pub build_dir_bytes: u64,
    /// How many worktrees it owns, measured or not.
    pub worktree_count: usize,
    /// The tier split.
    pub tiers: Tiers,
}

/// The per-tier counts, carried through both directions unchanged.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
pub(crate) struct Tiers {
    /// Worktrees safe to clear.
    pub stale: usize,
    /// Worktrees needing an operator decision.
    pub review: usize,
    /// Worktrees something forbids clearing.
    pub keep: usize,
    /// Registrations whose directory is gone.
    pub missing: usize,
}

// ── What this command reports ───────────────────────────────────────────────

/// The report, in the one shape `--json` emits and the table renders.
///
/// Why an internally-tagged enum rather than one struct with optional halves:
/// the two views answer different questions and share no rows, so a struct
/// would carry a `sessions` and a `worktrees` field that are never both
/// populated, and a consumer would have to guess which one it got. `view` says
/// which it got.
/// Test: `sessions_report_sorts_by_bytes_descending`,
/// `a_session_report_splits_build_bytes_from_the_rest`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "view", rename_all = "snake_case")]
pub(crate) enum DiskReport {
    /// One row per session, for the listing.
    Sessions {
        /// When the underlying survey ran.
        generated_at: String,
        /// Whether the survey's deadline truncated the pass.
        partial: bool,
        /// The project the listing was scoped to, `None` for the whole
        /// workspace.
        project: Option<String>,
        /// The rows, bytes descending.
        sessions: Vec<SessionRow>,
        /// Every row folded together.
        total: Totals,
    },
    /// One session's breakdown.
    Session {
        /// When the underlying survey ran.
        generated_at: String,
        /// Whether the survey's deadline truncated the pass.
        partial: bool,
        /// The session this breaks down, as the survey attributes it.
        session_id: String,
        /// Its friendly name, when the daemon still lists the session.
        session_name: Option<String>,
        /// The byte split by class, largest first.
        classes: Vec<ClassRow>,
        /// The worktrees behind it, bytes descending.
        worktrees: Vec<WorktreeRow>,
        /// Every worktree folded together.
        total: Totals,
    },
}

/// One session's row in the listing.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SessionRow {
    /// The owning session, `None` for the unattributed bucket.
    ///
    /// Always the raw UUID, in `--json` as well as in the fold — a consumer
    /// keys on it, and the name beside it can change or disappear.
    pub session_id: Option<String>,
    /// Its friendly name, when the daemon still lists the session. The table
    /// renders this in place of the UUID; `--json` carries both.
    pub session_name: Option<String>,
    /// Bytes across its measured worktrees.
    pub bytes: u64,
    /// Build-directory bytes across them.
    pub build_dir_bytes: u64,
    /// Everything that is not a build directory.
    pub source_bytes: u64,
    /// How many worktrees it owns.
    pub worktree_count: usize,
    /// The tier split.
    pub tiers: Tiers,
}

/// One class of bytes within a session.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ClassRow {
    /// `build` or `source` — the two classes slice 1's survey distinguishes.
    pub class: String,
    /// Bytes in this class.
    pub bytes: u64,
}

/// One worktree's row in a session's breakdown.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct WorktreeRow {
    /// The worktree directory.
    pub path: PathBuf,
    /// The project it sits under.
    pub project: String,
    /// Its tier.
    pub tier: String,
    /// Bytes on disk, `None` when the survey could not measure it.
    pub bytes: Option<u64>,
    /// Its build directories' bytes.
    pub build_dir_bytes: Option<u64>,
    /// Everything else, when both figures are known.
    pub source_bytes: Option<u64>,
}

/// The bottom line of either view.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub(crate) struct Totals {
    /// Bytes across every row.
    pub bytes: u64,
    /// Build-directory bytes across every row.
    pub build_dir_bytes: u64,
    /// Everything that is not a build directory.
    pub source_bytes: u64,
    /// How many worktrees the rows cover.
    pub worktree_count: usize,
}

/// Fold the survey's by-session roll-up into the listing.
///
/// Why: pure, so the ordering and the total are testable without a daemon. The
/// ORDER is slice 1's — `group_by_session` already sorts bytes descending with
/// the unattributed bucket last — and is preserved rather than recomputed, so
/// the CLI and the console cannot disagree about which session is first.
/// What: one row per group, with `source_bytes` derived as the remainder, plus
/// the total across every row.
/// Test: `sessions_report_sorts_by_bytes_descending`,
/// `sessions_report_totals_every_row`, `a_daemon_without_the_rollup_reports_nothing`.
pub(crate) fn sessions_report(
    survey: &Survey,
    project: Option<String>,
    names: &SessionNames,
) -> DiskReport {
    let groups = survey.by_session.clone().unwrap_or_default();
    let sessions: Vec<SessionRow> = groups
        .into_iter()
        .map(|g| SessionRow {
            session_name: g
                .session_id
                .as_ref()
                .and_then(|id| names.get(id.as_str()).cloned()),
            session_id: g.session_id,
            bytes: g.bytes,
            build_dir_bytes: g.build_dir_bytes,
            source_bytes: g.bytes.saturating_sub(g.build_dir_bytes),
            worktree_count: g.worktree_count,
            tiers: g.tiers,
        })
        .collect();
    let total = sessions.iter().fold(Totals::default(), |mut t, r| {
        t.bytes = t.bytes.saturating_add(r.bytes);
        t.build_dir_bytes = t.build_dir_bytes.saturating_add(r.build_dir_bytes);
        t.source_bytes = t.source_bytes.saturating_add(r.source_bytes);
        t.worktree_count += r.worktree_count;
        t
    });
    DiskReport::Sessions {
        generated_at: survey.generated_at.clone(),
        partial: survey.partial,
        project,
        sessions,
        total,
    }
}

/// Fold one session's worktrees into its breakdown.
///
/// Why: an operator who knows which session is expensive next asks WHERE — and
/// the answer that matters is how much of it is a build directory, because that
/// is the part `cargo clean` reclaims without touching any work.
/// What: every worktree the survey charged to `target`, across every project,
/// bytes descending; the class split; and the total. An unmeasured worktree
/// contributes nothing to the totals and is still listed, so the two disagreeing
/// is visible rather than silently absorbed.
/// Test: `a_session_report_splits_build_bytes_from_the_rest`,
/// `a_session_report_spans_projects`, `an_unknown_session_is_an_error`,
/// `an_unmeasured_worktree_is_listed_and_adds_nothing`.
pub(crate) fn session_report(
    survey: &Survey,
    target: &str,
    names: &SessionNames,
) -> anyhow::Result<DiskReport> {
    let mut worktrees: Vec<WorktreeRow> = Vec::new();
    for project in &survey.root.projects {
        for wt in &project.worktrees {
            if wt.owning_session.as_deref() != Some(target) {
                continue;
            }
            worktrees.push(WorktreeRow {
                path: wt.path.clone(),
                project: project.name.clone(),
                tier: wt.tier.clone(),
                bytes: wt.bytes,
                build_dir_bytes: wt.build_dir_bytes,
                source_bytes: match (wt.bytes, wt.build_dir_bytes) {
                    (Some(b), Some(d)) => Some(b.saturating_sub(d)),
                    _ => None,
                },
            });
        }
    }
    if worktrees.is_empty() {
        // #7313: an error, never an empty report. "0 B across 0 worktrees"
        // would be indistinguishable from a session that genuinely holds
        // nothing, and the operator would act on the wrong one. A name that
        // resolved to no session at all fails earlier, in
        // `resolve_session_id`, with its own message — this arm is only for a
        // session that exists and holds nothing.
        let label = names.get(target).map_or(target, String::as_str);
        bail!(
            "session `{label}` holds no worktrees in the survey — \
             `tm session disk` with no argument lists the sessions that hold bytes"
        );
    }
    worktrees.sort_by(|a, b| {
        b.bytes
            .unwrap_or(0)
            .cmp(&a.bytes.unwrap_or(0))
            .then_with(|| a.path.cmp(&b.path))
    });
    let total = worktrees.iter().fold(Totals::default(), |mut t, r| {
        t.bytes = t.bytes.saturating_add(r.bytes.unwrap_or(0));
        t.build_dir_bytes = t
            .build_dir_bytes
            .saturating_add(r.build_dir_bytes.unwrap_or(0));
        t.source_bytes = t.source_bytes.saturating_add(r.source_bytes.unwrap_or(0));
        t.worktree_count += 1;
        t
    });
    let mut classes = vec![
        ClassRow {
            class: "build".to_string(),
            bytes: total.build_dir_bytes,
        },
        ClassRow {
            class: "source".to_string(),
            bytes: total.source_bytes,
        },
    ];
    classes.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.class.cmp(&b.class)));
    Ok(DiskReport::Session {
        generated_at: survey.generated_at.clone(),
        partial: survey.partial,
        session_id: target.to_string(),
        session_name: names.get(target).cloned(),
        classes,
        worktrees,
        total,
    })
}

/// Render either view as the operator's table.
///
/// Why: a `String` rather than a series of `println!`s so the layout is
/// testable — the column widths and the total line are the part that breaks.
/// What: a fixed-width table, byte figures through the one
/// [`human_bytes`](trusty_mpm::disk::human_bytes) the doctor row also uses.
/// Test: `the_listing_renders_a_total_line`, `the_breakdown_names_every_class`.
pub(crate) fn render(report: &DiskReport) -> String {
    let mut out = String::new();
    match report {
        DiskReport::Sessions {
            project,
            sessions,
            total,
            ..
        } => {
            out.push_str(&match project {
                Some(name) => format!("project {name}\n"),
                None => "every managed project\n".to_string(),
            });
            out.push_str(&format!(
                "{:<SESSION_COLUMN$} {:>6} {:>11} {:>11} {:>11}\n",
                "SESSION", "TREES", "BUILD", "SOURCE", "TOTAL"
            ));
            for row in sessions {
                out.push_str(&format!(
                    "{:<SESSION_COLUMN$} {:>6} {:>11} {:>11} {:>11}\n",
                    session_label(row),
                    row.worktree_count,
                    human_bytes(row.build_dir_bytes),
                    human_bytes(row.source_bytes),
                    human_bytes(row.bytes),
                ));
            }
            out.push_str(&total_line(total));
        }
        DiskReport::Session {
            session_id,
            session_name,
            classes,
            worktrees,
            total,
            ..
        } => {
            out.push_str(&match session_name {
                Some(name) => format!("session {name} ({session_id})\n"),
                None => format!("session {session_id}\n"),
            });
            for class in classes {
                out.push_str(&format!(
                    "  {:<10} {:>11}\n",
                    class.class,
                    human_bytes(class.bytes)
                ));
            }
            out.push('\n');
            out.push_str(&format!(
                "{:<9} {:>11} {:>11}  {}\n",
                "TIER", "BUILD", "TOTAL", "WORKTREE"
            ));
            for row in worktrees {
                out.push_str(&format!(
                    "{:<9} {:>11} {:>11}  {}\n",
                    row.tier,
                    row.build_dir_bytes.map_or_else(unmeasured, human_bytes),
                    row.bytes.map_or_else(unmeasured, human_bytes),
                    row.path.display(),
                ));
            }
            out.push_str(&total_line(total));
        }
    }
    out
}

/// Width of the SESSION column, wide enough for a 36-character UUID.
const SESSION_COLUMN: usize = 38;

/// What the SESSION column says for one row.
///
/// Why: an operator reads names, not UUIDs, so the friendly name wins where the
/// daemon still knows one; the UUID is the fallback because an ENDED session's
/// leftovers have no name left to render, and `--json` carries the UUID either
/// way. The result is capped so a long name cannot shift every numeric column
/// right — the one failure that makes the table unreadable.
/// Test: `the_listing_renders_the_friendly_name_when_one_is_known`,
/// `a_long_session_name_is_truncated_to_keep_the_columns_aligned`.
fn session_label(row: &SessionRow) -> String {
    let label = row
        .session_name
        .clone()
        .or_else(|| row.session_id.clone())
        .unwrap_or_else(unattributed);
    fit(&label, SESSION_COLUMN)
}

/// Cap `label` at `width` characters, marking a cut with an ellipsis.
///
/// Test: `a_long_session_name_is_truncated_to_keep_the_columns_aligned`.
fn fit(label: &str, width: usize) -> String {
    if label.chars().count() <= width {
        return label.to_string();
    }
    let kept: String = label.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// The bottom line both views end on.
fn total_line(total: &Totals) -> String {
    format!(
        "{:<SESSION_COLUMN$} {:>6} {:>11} {:>11} {:>11}\n",
        "TOTAL",
        total.worktree_count,
        human_bytes(total.build_dir_bytes),
        human_bytes(total.source_bytes),
        human_bytes(total.bytes),
    )
}

/// The label for the bucket nothing attributed.
fn unattributed() -> String {
    "(unattributed)".to_string()
}

/// The label for a figure the survey could not measure.
///
/// Why a word rather than `0 B`: a zero would read as an empty directory, which
/// is the one thing an unmeasured worktree is not known to be.
fn unmeasured() -> String {
    "?".to_string()
}
