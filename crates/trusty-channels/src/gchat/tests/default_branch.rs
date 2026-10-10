//! gchat's load gate is the Db1 gate (#8454 S3a; Bob Db1, Architect G2/G3).
//!
//! Why: each refusal arm must deny the route load with a named reason, and
//! no send path may proceed past a refused load (the fail-open check).
//! What: one test per arm, each on a real temp repo. Every refusal is
//! checked at `load_routes`, `LoadStatus`, an ask, a notify, the audit log
//! and the mock server's request count. No test reaches Google.
//! Test: this module is the test.

use wiremock::MockServer;

use super::{all_requests, git, Project, THREE_ROUTES};
use crate::gchat::api::client::Endpoints;
use crate::gchat::channel::LoadStatus;
use crate::gchat::doctor::run_doctor;
use crate::gchat::error::{RouteError, SendError};
use crate::gchat::routes::load_routes;
use crate::policy::GateError;

const ROUTES: &str = ".trusty-channels/routes.toml";

/// The gate reason a refused load carries; panics on an accepted load or a
/// non-gate error.
fn gate_reason(project: &Project) -> (RouteError, GateError) {
    let err = load_routes(project.dir()).expect_err("the route load must be refused");
    match &err {
        RouteError::Gate { reason, .. } => {
            let reason = reason.clone();
            (err, reason)
        }
        other => panic!("expected a gate refusal, got {other:?}"),
    }
}

/// Assert `project`'s load is refused for `matches_arm`, and that an ask and
/// a notify each return that refusal with no request and an audit line.
async fn assert_refused_everywhere(project: &Project, matches_arm: impl Fn(&GateError) -> bool) {
    let (err, reason) = gate_reason(project);
    assert!(matches_arm(&reason), "wrong gate reason: {err}");
    let server = MockServer::start().await;
    let channel = project.channel(&server);
    assert_eq!(
        channel.load_status(),
        LoadStatus::Refused {
            reason: err.to_string()
        }
    );
    let ask = channel
        .send_question("janet", "Ship it?")
        .await
        .expect_err("an ask on a refused load must be refused");
    let notify = channel
        .send_review_notice("janet", "Review", "https://example.com/pr/1")
        .await
        .expect_err("a notify on a refused load must be refused");
    for sent in [ask, notify] {
        assert!(
            matches!(&sent, SendError::RoutesUnavailable { reason } if *reason == err.to_string()),
            "the send did not return the load refusal: {sent:?}"
        );
    }
    assert_eq!(all_requests(&server).await, 0, "a request left the gate");
    let lines = project.audit_lines();
    assert_eq!(lines.len(), 2, "one audit line per refused send");
    assert!(lines.iter().all(|l| l["reason"] == "routes_unavailable"));
}

#[tokio::test]
async fn feature_branch_refuses_load_and_every_send() {
    // The routes are committed on a feature branch only.
    let project = Project::uncommitted(THREE_ROUTES);
    git(project.dir(), &["checkout", "-q", "-b", "feature"]);
    git(project.dir(), &["add", ROUTES]);
    git(project.dir(), &["commit", "-q", "-m", "routes"]);
    assert_refused_everywhere(&project, |r| {
        *r == GateError::NotOnDefaultBranch {
            head: "feature".into(),
            default: "main".into(),
        }
    })
    .await;
}

#[tokio::test]
async fn detached_head_at_the_default_tip_refuses_load_and_every_send() {
    let project = Project::committed(THREE_ROUTES);
    git(project.dir(), &["checkout", "-q", "--detach"]);
    assert_refused_everywhere(&project, |r| *r == GateError::DetachedHead).await;
}

#[tokio::test]
async fn both_main_and_master_without_origin_head_refuse_load_and_every_send() {
    let project = Project::committed(THREE_ROUTES);
    git(project.dir(), &["branch", "master"]);
    assert_refused_everywhere(&project, |r| *r == GateError::DefaultBranchUnknown).await;
}

#[tokio::test]
async fn neither_main_nor_master_refuses_load_and_every_send() {
    let project = Project::committed(THREE_ROUTES);
    git(project.dir(), &["branch", "-m", "main", "trunk"]);
    assert_refused_everywhere(&project, |r| *r == GateError::DefaultBranchUnknown).await;
}

#[tokio::test]
async fn untracked_and_dirty_arms_refuse_load_and_every_send() {
    let untracked = Project::uncommitted(THREE_ROUTES);
    assert_refused_everywhere(&untracked, |r| matches!(r, GateError::NotCommitted { .. })).await;
    let dirty = Project::committed(THREE_ROUTES);
    let edited = std::fs::read_to_string(dirty.routes_file()).expect("read") + "# edit\n";
    std::fs::write(dirty.routes_file(), edited).expect("edit");
    assert_refused_everywhere(&dirty, |r| matches!(r, GateError::ContentDiffers { .. })).await;
}

#[tokio::test]
async fn doctor_load_row_names_the_gate_reason() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    git(project.dir(), &["checkout", "-q", "-b", "feature"]);
    let report = run_doctor(project.dir(), Endpoints::single_host(&server.uri()), true).await;
    let text = report.render();
    assert!(!report.ok(), "{text}");
    assert_eq!(report.load.status, "failed", "{text}");
    assert!(
        report
            .load
            .detail
            .contains("HEAD is on feature, not the default branch main"),
        "{text}"
    );
    assert_eq!(all_requests(&server).await, 0);
}

#[tokio::test]
async fn doctor_load_row_says_default_branch_when_accepted() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    let report = run_doctor(project.dir(), Endpoints::single_host(&server.uri()), true).await;
    assert_eq!(report.load.status, "ok", "{}", report.render());
    assert_eq!(
        report.load.detail,
        "3 route(s), committed on the default branch",
        "{}",
        report.render()
    );
}
