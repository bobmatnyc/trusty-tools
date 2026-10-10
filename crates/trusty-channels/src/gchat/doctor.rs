//! `gchat-mcp doctor` (#9448 D6): one row per route, reporting each check
//! the operator must pass before a session can send.
//!
//! Why: a Chat route fails in several independent places — the routes file,
//! its load gate, the key file, the token grant, the route's space — and
//! the operator needs to see which one, per route, without reading logs.
//! While a server holds the state lock, doctor must still report everything
//! that needs no lock.
//! What: [`run_doctor`] opens the project's channel only to read its state
//! columns, then loads the routes and builds its own client with no lock
//! held; when a running `gchat-mcp` holds the state lock the state columns
//! read "in use by a running gchat-mcp", which is not a failure.
//! [`report_for_channel`] builds the same report from a channel already
//! open; the `gchat_doctor` tool uses it, and a poller error fails it.
//! A space route's space column reads `configured` from the routes file, so
//! it needs no state lock; a DM route's reads `bound` or `pending`.
//! No check sends a message; the token mint is skipped with `offline`.
//! Test: `doctor_prints_one_row_per_route_and_passes`,
//! `doctor_fails_on_a_broken_key_mode`,
//! `doctor_mints_a_token_online_and_reports_in_use_state`,
//! `doctor_fails_without_a_routes_file`,
//! `doctor_reports_a_space_route_as_configured`.

use std::path::Path;

use serde::Serialize;

use crate::gchat::api::client::{Endpoints, GchatClient};
use crate::gchat::api::constants::REQUIRED_KEY_FILE_MODE;
use crate::gchat::channel::{ChannelHealth, GchatChannel};
use crate::gchat::error::{RouteError, StateError};
use crate::gchat::poller::PollStatus;
use crate::gchat::routes::{load_routes, routes_path, RouteTable};

/// The state columns' text while another process holds the state lock.
pub const IN_USE: &str = "in use by a running gchat-mcp";

/// One check's outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Cell {
    /// `ok`, `failed`, `skipped`, `pending`, `in_use` or `configured`.
    /// Only `failed` fails the report.
    pub status: &'static str,
    /// A short reason. Never key material, a token or message text.
    pub detail: String,
}

impl Cell {
    fn new(status: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: detail.into(),
        }
    }
    fn ok(detail: impl Into<String>) -> Self {
        Self::new("ok", detail)
    }
    fn failed(detail: impl Into<String>) -> Self {
        Self::new("failed", detail)
    }
    fn skipped(detail: impl Into<String>) -> Self {
        Self::new("skipped", detail)
    }
    /// True when this check failed.
    pub fn is_failed(&self) -> bool {
        self.status == "failed"
    }
    fn render(&self) -> String {
        if self.detail.is_empty() {
            self.status.to_string()
        } else {
            format!("{} ({})", self.status, self.detail)
        }
    }
}

/// One route's row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorRow {
    /// Route name.
    pub route: String,
    /// Recipient email.
    pub recipient: String,
    /// Allowed kinds, schema spelling.
    pub kinds: Vec<&'static str>,
    /// The routes file passed the load gate (committed on the default branch).
    pub gate: Cell,
    /// The key file exists, has mode 0600 and parses.
    pub key_file: Cell,
    /// An access token was minted (skipped offline).
    pub token: Cell,
    /// The route's space: `configured` in the routes file, else the
    /// learned DM space (`bound`) or `pending`.
    pub space: Cell,
    /// Open questions on this route.
    pub open_questions: Cell,
    /// The Pub/Sub subscription resource name.
    pub subscription: String,
}

/// The whole doctor report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    /// The project directory checked.
    pub project_dir: String,
    /// The routes-file load, including its gate.
    pub load: Cell,
    /// The connection's key-file check.
    pub key_file: Cell,
    /// The connection's token mint.
    pub token: Cell,
    /// The state directory: open, or in use by a running server.
    pub state: Cell,
    /// One row per route, in file order.
    pub rows: Vec<DoctorRow>,
    /// The serving process's poller, when the report comes from it.
    pub poller: Option<PollStatus>,
}

impl DoctorReport {
    /// True when the load, every check in every row, and the poller (when
    /// the report carries one) are healthy.
    pub fn ok(&self) -> bool {
        let row_failed = |r: &DoctorRow| {
            [&r.gate, &r.key_file, &r.token, &r.space, &r.open_questions]
                .iter()
                .any(|c| c.is_failed())
        };
        ![&self.load, &self.key_file, &self.token, &self.state]
            .iter()
            .any(|c| c.is_failed())
            && !self.rows.iter().any(row_failed)
            // #9448 review: a failing poller (withheld messages included)
            // is not a healthy server.
            && self.poller.as_ref().is_none_or(PollStatus::is_healthy)
    }

    /// The process exit code: 0 when [`DoctorReport::ok`], else 1.
    pub fn exit_code(&self) -> u8 {
        u8::from(!self.ok())
    }

    /// The report as text, one line per route.
    ///
    /// Why: the CLI's output and the tool's `text` field.
    /// What: header lines, then `route=<name> …` per row, then the poller
    /// line when present, then `result: ok` or `result: FAILED`.
    /// Test: `doctor_prints_one_row_per_route_and_passes`.
    pub fn render(&self) -> String {
        let mut out = format!("gchat doctor: {}\n", self.project_dir);
        out.push_str(&format!("load: {}\n", self.load.render()));
        out.push_str(&format!("key_file: {}\n", self.key_file.render()));
        out.push_str(&format!("token: {}\n", self.token.render()));
        out.push_str(&format!("state: {}\n", self.state.render()));
        for r in &self.rows {
            out.push_str(&format!(
                "route={} recipient={} kinds={} gate={} key_file={} token={} space={} \
                 open_questions={} subscription={}\n",
                r.route,
                r.recipient,
                r.kinds.join(","),
                r.gate.render(),
                r.key_file.render(),
                r.token.render(),
                r.space.render(),
                r.open_questions.render(),
                r.subscription,
            ));
        }
        if let Some(p) = &self.poller {
            out.push_str(&format!(
                "poller: ticks={} answered={} withheld={} last_ok_at={} \
                 consecutive_failures={} last_error={}\n",
                p.ticks,
                p.answered,
                p.withheld,
                p.last_ok_at.as_deref().unwrap_or("never"),
                p.consecutive_failures,
                p.last_error.as_deref().unwrap_or("none"),
            ));
        }
        out.push_str(if self.ok() {
            "result: ok\n"
        } else {
            "result: FAILED\n"
        });
        out
    }
}

/// Where the state columns come from.
enum StateView {
    Health(ChannelHealth),
    InUse,
    Failed(String),
}

/// Run doctor for `project_dir`.
///
/// Why: the `gchat-mcp doctor` entry point.
/// What: opens the channel only to read its state columns and closes it at
/// once, so the state lock is never held across the token mint (a
/// `gchat-mcp` started meanwhile would exit). A held lock reports the state
/// columns as [`IN_USE`] (not a failure); any other open error fails the
/// state column. The routes, client and token checks then run with no lock.
/// Test: `doctor_prints_one_row_per_route_and_passes`,
/// `doctor_mints_a_token_online_and_reports_in_use_state`,
/// `doctor_releases_the_state_lock_before_the_token_mint`.
pub async fn run_doctor(project_dir: &Path, endpoints: Endpoints, offline: bool) -> DoctorReport {
    // #9448 review: the channel (and its lock) drops at the end of this
    // statement, before any network call.
    let state = match GchatChannel::open_with(project_dir, endpoints.clone()) {
        Ok(channel) => StateView::Health(channel.health()),
        Err(StateError::Locked { .. }) => StateView::InUse,
        Err(e) => StateView::Failed(e.to_string()),
    };
    let routes = load_routes(project_dir);
    let client = routes
        .as_ref()
        .ok()
        .and_then(|t| t.connection.as_ref())
        .map(|c| GchatClient::with_endpoints(&c.key_file, endpoints).map_err(|e| e.to_string()));
    let client_ref = match &client {
        Some(Ok(c)) => Ok(c),
        Some(Err(e)) => Err(e.clone()),
        None => Err(no_connection()),
    };
    let token = token_cell(client_ref.as_ref().ok().copied(), offline).await;
    build(
        project_dir,
        &routes,
        client_ref.map(|_| ()),
        token,
        state,
        None,
    )
}

/// The doctor report for a channel already open.
///
/// Why: the `gchat_doctor` tool reads the server's own channel, so it
/// bypasses no gate and needs no second lock.
/// What: the load outcome, key-file and client status, a token mint through
/// the channel's client (skipped when `offline`), and per route the learned
/// space and open-question count.
/// Test: `add_on_batch_is_a_loud_poller_error_and_shows_in_gchat_doctor`.
pub async fn report_for_channel(
    channel: &GchatChannel,
    offline: bool,
    poller: Option<PollStatus>,
) -> DoctorReport {
    let client = channel.client();
    let token = token_cell(client.as_ref().ok().copied(), offline).await;
    let health = channel.health();
    let client_status = health.client.clone();
    build(
        channel.project_dir(),
        &channel.routes,
        client_status,
        token,
        StateView::Health(health),
        poller,
    )
}

fn no_connection() -> String {
    "routes file names no [gchat.connection]".to_string()
}

async fn token_cell(client: Option<&GchatClient>, offline: bool) -> Cell {
    if offline {
        return Cell::skipped("--offline");
    }
    match client {
        None => Cell::skipped("no usable client"),
        Some(c) => match c.check_token().await {
            Ok(_) => Cell::ok("minted"),
            Err(e) => Cell::failed(e.to_string()),
        },
    }
}

/// Exists, is a regular file, has mode 0600; then the client built from it.
fn key_file_cell(path: &Path, client: &Result<(), String>) -> Cell {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return Cell::failed(format!("{}: {}", path.display(), e.kind())),
    };
    if !meta.is_file() {
        return Cell::failed(format!("{} is not a regular file", path.display()));
    }
    let mode = file_mode(&meta);
    if mode != REQUIRED_KEY_FILE_MODE {
        return Cell::failed(format!(
            "{} has mode {mode:o}; it must be 600",
            path.display()
        ));
    }
    match client {
        Ok(()) => Cell::ok("present, mode 600"),
        Err(e) => Cell::failed(e.clone()),
    }
}

/// Permission bits; non-Unix reports 0, which never passes (the client
/// refuses a key there too).
#[cfg(unix)]
fn file_mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn file_mode(_meta: &std::fs::Metadata) -> u32 {
    0
}

fn load_cell(project_dir: &Path, routes: &Result<RouteTable, RouteError>) -> Cell {
    match routes {
        Ok(t) if !t.file_present => Cell::failed(format!(
            "no routes file at {}",
            routes_path(project_dir).display()
        )),
        Ok(t) => Cell::ok(format!(
            "{} route(s), committed on the default branch",
            t.routes.len()
        )),
        Err(e) => Cell::failed(e.to_string()),
    }
}

fn build(
    project_dir: &Path,
    routes: &Result<RouteTable, RouteError>,
    client: Result<(), String>,
    token: Cell,
    state: StateView,
    poller: Option<PollStatus>,
) -> DoctorReport {
    let load = load_cell(project_dir, routes);
    let connection = routes.as_ref().ok().and_then(|t| t.connection.as_ref());
    let key_file = match connection {
        Some(c) => key_file_cell(&c.key_file, &client),
        None => Cell::skipped(no_connection()),
    };
    let state_cell = match &state {
        StateView::Health(_) => Cell::ok("open"),
        StateView::InUse => Cell::new("in_use", IN_USE),
        StateView::Failed(e) => Cell::failed(e.clone()),
    };
    let subscription = connection
        .map(|c| c.subscription_name())
        .unwrap_or_default();
    let rows = routes
        .as_ref()
        .map(|t| t.routes.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|r| {
            let (space, open_questions) = state_cells(&state, &r.name);
            // #9448: a configured space comes from the routes file, not state.
            let space = match &r.space {
                Some(configured) => Cell::new("configured", configured.clone()),
                None => space,
            };
            DoctorRow {
                route: r.name.clone(),
                recipient: r.recipient.clone(),
                kinds: r.kinds.iter().map(|k| k.as_str()).collect(),
                gate: Cell::ok("committed on the default branch"),
                key_file: key_file.clone(),
                token: token.clone(),
                space,
                open_questions,
                subscription: subscription.clone(),
            }
        })
        .collect();
    DoctorReport {
        project_dir: project_dir.display().to_string(),
        load,
        key_file,
        token,
        state: state_cell,
        rows,
        poller,
    }
}

/// The space and open-question cells for `route`.
fn state_cells(state: &StateView, route: &str) -> (Cell, Cell) {
    match state {
        StateView::InUse => (Cell::new("in_use", IN_USE), Cell::new("in_use", IN_USE)),
        StateView::Failed(e) => (Cell::failed(e.clone()), Cell::failed(e.clone())),
        StateView::Health(h) => match h.routes.iter().find(|r| r.name == route) {
            Some(r) => (
                match &r.space {
                    Some(_) => Cell::ok("bound"),
                    None => Cell::new("pending", "not bound: the recipient must message the app"),
                },
                Cell::ok(r.open_questions.to_string()),
            ),
            None => (Cell::failed("route missing from state"), Cell::failed("")),
        },
    }
}
