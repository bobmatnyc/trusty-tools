//! Tests for the channel policy (#8454 S1).
//!
//! Why: each S1 rule needs a test that a fail-open implementation fails.
//! What: shared builders for route specs and a settable test clock. Pure
//! data; no file, network or sleep.
//! Test: this module is the test.

mod bucket;
mod build;
mod egress;
mod gate;
mod host;
mod host_refs;
mod inbound;
mod load;
mod merge;
mod project_file;
mod reload;
mod repo;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use crate::policy::{
    parse_host, parse_project_file, Channel, ChannelPolicy, Clock, FileState, HostCeiling,
    HostError, LoadReport, MessageKind, PolicySpec, ProjectInput, RateLimitSpec, RouteSpec,
};

// ── S2a fixtures (#8454): host ceilings and project files as text ──

/// The home directory every S2a parse is given.
pub(super) const HOME: &str = "/home/t";
/// Two project directories, both listed on every channel in [`HOST_ALL`].
pub(super) const PROJ_A: &str = "/work/a";
pub(super) const PROJ_B: &str = "/work/b";

/// A ceiling enabling every channel for both projects, with no gchat
/// connection and the built-in default rate limit spelled out.
pub(super) const HOST_ALL: &str = "\
unrelated: { ignored: true }
channels:
  version: 1
  rate_limit: { limit: 100, window_secs: 60 }
  gchat:
    enabled: true
    projects: [/work/a, /work/b]
  slack:
    enabled: true
    connection: { bot_ref: slack }
    projects: [/work/a, /work/b]
  telegram:
    enabled: true
    projects: [/work/a, /work/b]
";

/// Project A: one Slack and one Telegram route (v2).
pub(super) const A_V2: &str = r#"version = 2

[[slack.routes]]
name = "bob-dm"
recipient = "U0ABCDEF1"
kinds = ["question"]

[[telegram.routes]]
name = "bob-tg"
recipient = "123456789"
kinds = ["question"]
"#;

/// Project B: one gchat route (v1, today's schema).
pub(super) const B_V1: &str = r#"version = 1

[gchat.connection]
project_id = "p"
subscription = "s"
key_file = "/k.json"

[[gchat.routes]]
name = "janet"
recipient = "janet@example.com"
kinds = ["question", "review_notice"]
"#;

/// Parse a host ceiling with [`HOME`] as the home directory.
pub(super) fn host(yaml: &str) -> Result<HostCeiling, HostError> {
    parse_host(yaml, Some(Path::new(HOME)))
}

/// A project input whose file is `<dir>/.trusty-channels/routes.toml`.
pub(super) fn input(dir: &str, toml: &str) -> ProjectInput {
    ProjectInput {
        project_dir: PathBuf::from(dir),
        file: routes_file(dir),
        parsed: parse_project_file(toml, Some(Path::new(HOME))),
    }
}

/// The route file path for a project directory.
pub(super) fn routes_file(dir: &str) -> PathBuf {
    Path::new(dir).join(".trusty-channels/routes.toml")
}

/// The names of the routes in effect, in order.
pub(super) fn names(report: &LoadReport) -> Vec<&str> {
    report.policy.routes().iter().map(|r| r.name()).collect()
}

/// The state of the file for project `dir`.
pub(super) fn state(report: &LoadReport, dir: &str) -> FileState {
    report
        .per_file
        .iter()
        .find(|s| s.project_dir == Path::new(dir))
        .map(|s| s.state)
        .expect("a status for every input file")
}

/// Assert the whole load was denied: no route, `denied`, no file effective.
pub(super) fn assert_denied(report: &LoadReport) {
    assert!(report.denied, "load not denied: {report:#?}");
    assert!(report.policy.is_empty(), "routes survived a deny");
    for s in &report.per_file {
        assert!(
            !matches!(s.state, FileState::Effective { .. }),
            "{s:?} is effective in a denied load"
        );
    }
}

pub(super) const JANET: &str = "janet@example.com";
pub(super) const BOB_SLACK: &str = "U0ABCDEF1";
pub(super) const BOB_TG: &str = "123456789";

/// A route spec with no rate limit.
pub(super) fn spec(
    channel: Channel,
    name: &str,
    recipient: &str,
    kinds: &[MessageKind],
) -> RouteSpec {
    RouteSpec {
        channel,
        name: name.to_string(),
        recipient: recipient.to_string(),
        kinds: kinds.to_vec(),
        rate_limit: None,
    }
}

/// One route per channel: gchat `janet` (question, review_notice), Slack
/// `bob-dm` and Telegram `bob-tg` (question).
pub(super) fn three_routes() -> Vec<RouteSpec> {
    use MessageKind::{Question, ReviewNotice};
    vec![
        spec(Channel::Gchat, "janet", JANET, &[Question, ReviewNotice]),
        spec(Channel::Slack, "bob-dm", BOB_SLACK, &[Question]),
        spec(Channel::Telegram, "bob-tg", BOB_TG, &[Question]),
    ]
}

/// Build a policy from routes that must be valid.
pub(super) fn policy(routes: Vec<RouteSpec>) -> ChannelPolicy {
    ChannelPolicy::build(PolicySpec {
        rate_limit: None,
        routes,
    })
    .expect("test routes are valid")
}

/// A rate-limit spec.
pub(super) fn limit(limit: i64, window_secs: i64) -> RateLimitSpec {
    RateLimitSpec { limit, window_secs }
}

/// A clock the test sets by hand.
#[derive(Debug, Clone, Default)]
pub(super) struct TestClock(Rc<Cell<Duration>>);

impl TestClock {
    pub(super) fn set_ms(&self, ms: u64) {
        self.0.set(Duration::from_millis(ms));
    }
}

impl Clock for TestClock {
    fn now(&self) -> Duration {
        self.0.get()
    }
}
