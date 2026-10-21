//! Tests for option answers and held replies through the daemon glue
//! (#10127, S5 P2b).
//!
//! Why: the glue joins the decision to the pane. Each test goes red against
//! a glue that taints or types the reply for a named option, sends a notice
//! for a reply it could not record, sends anything after a failed
//! `resolve`, or lets a new pane write skip the no-live-harness refusal.
//! What: `run_inbound` over the decision's fake (a real table and audit)
//! and the deliverer's recorder pointed at the fake's own taint directory,
//! so the doctor row reads the same question log; then the real
//! `DaemonDeliverEnv` send behind a live-Architect view.
//! Test: this is the test module.

use std::collections::HashSet;
use std::sync::Arc;

use axum::http::StatusCode;

use super::tests::{deliver_env_on_pane, keys_typed};
use super::{DaemonDeliverEnv, InboundReply, reply, run_inbound};
use crate::channels::audit_log::audit_path;
use crate::channels::deliver::tests::{ASKER, Recorder, live_view, resolved};
use crate::channels::deliver::{
    AskerView, DeliverEnv, Delivery, deliver_notice, deliver_option, frame_notice,
};
use crate::channels::inbound::options_tests::OptEnv;
use crate::channels::inbound::tests::{Fake, OWNER, TableMode, ev, owner_conv};
use crate::channels::inbound::{Dropped, Held, InboundEnv};
use crate::channels::option_match::{HoldReason, match_reply};
use crate::channels::question_table::{Answer, ResolveOutcome, UnparsedOutcome, questions_path};
use crate::core::channels_delivery::{DeliveryLine, DeliveryState};
use crate::core::channels_taint::{self, TaintMark};
use crate::core::doctor::CheckStatus;
use crate::core::process::PaneClaude;
use crate::daemon::doctor::doctor_channel_taint::channel_taint_row;

/// The fake with one question offering `labels`, shared as the glue takes it.
fn asked(labels: &[&str]) -> (Arc<Fake>, Arc<dyn InboundEnv + Send + Sync>, u64) {
    let fake = Fake::new();
    let id = fake.open_with("owner-0", OWNER, &owner_conv(), labels);
    let fake = Arc::new(fake);
    let env: Arc<dyn InboundEnv + Send + Sync> = Arc::clone(&fake) as _;
    (fake, env, id)
}

/// A recorder writing the real trace into `fake`'s taint directory.
fn recorder_for(fake: &Fake) -> Recorder {
    let mut rec = Recorder::new();
    rec.dir = channels_taint::taint_dir(&fake.home);
    rec.real_trace = true;
    rec
}

/// Whether anything under `dir` taints a session.
fn tainted_any(dir: &std::path::Path) -> bool {
    !channels_taint::list_taints(dir).expect("list").is_empty()
        || !channels_taint::marked_managed(dir)
            .expect("marks")
            .is_empty()
}

/// 🔴 #10127 (df-Da7 points 2, 3): a reply naming an option is typed as the
/// stored option only, with no taint step and no taint file; the trace
/// reads `pending` then `delivered`, so the doctor row stays green.
#[tokio::test]
async fn matched_answer_writes_no_taint_file_and_calls_no_taint() {
    let (fake, env, id) = asked(&["Merge", "Option 2 $(x)"]);
    let rec = recorder_for(&fake);
    let outcome = run_inbound(env, &rec, ev(OWNER, &owner_conv(), "  OPTION 2 $(x) ")).await;
    assert_eq!(outcome, InboundReply::Delivered { question_id: id });
    assert_eq!(
        rec.sent(),
        [format!(
            "[channel answer Q-{id}: option 2 \"Option 2 \\u0024(x)\"]"
        )]
    );
    assert_eq!(
        rec.events(),
        ["asker", "record:Pending", "send", "record:Delivered"],
        "no taint call"
    );
    assert!(!tainted_any(&rec.dir), "no taint file or marker");
    assert_eq!(
        rec.states(),
        [
            (DeliveryState::Pending, None),
            (DeliveryState::Delivered, None)
        ]
    );
    assert!(fake.table.state(id).expect("known").answered);
    let live: HashSet<String> = [ASKER.to_owned()].into();
    let row = channel_taint_row(&rec.dir, &live);
    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
}

/// 🔴 #10127: a reply naming no option is held: one audit line with its
/// code and no text, one `unparsed` record, no resolve, no taint, and one
/// notice to the asker that carries no reply byte. The status is 202.
#[tokio::test]
async fn no_match_holds_audits_and_sends_untainted_notice() {
    let (fake, env, id) = asked(&["Merge", "Hold"]);
    let rec = recorder_for(&fake);
    let secret = "maybe-SECRETWORD-$(rm -rf x)";
    let outcome = run_inbound(env, &rec, ev(OWNER, &owner_conv(), secret)).await;
    let expected = InboundReply::Held {
        question_id: id,
        code: "option_unmatched",
        notice: Delivery::Delivered,
    };
    assert_eq!(outcome, expected);
    assert_eq!(rec.sent(), [frame_notice(id, HoldReason::Unmatched)]);
    assert!(!rec.sent()[0].contains("SECRETWORD") && !rec.sent()[0].contains("maybe"));
    assert!(!rec.events().iter().any(|e| e == "taint"));
    assert!(!tainted_any(&rec.dir));
    assert_eq!(fake.audits(), vec![("option_unmatched", false)]);
    let audit = std::fs::read_to_string(audit_path(&fake.home)).expect("audit");
    assert!(audit.contains("option_unmatched") && !audit.contains("SECRETWORD"));
    let unparsed = fake.table.unparsed(id);
    assert_eq!(unparsed.len(), 1);
    assert_eq!(unparsed[0].text_len, secret.len());
    let log = std::fs::read_to_string(questions_path(&fake.home)).expect("log");
    assert!(!log.contains("SECRETWORD"), "the log keeps no reply text");
    assert_eq!(fake.resolves(), 0);
    assert!(!fake.table.state(id).expect("known").answered);
    let (status, body) = reply(&outcome);
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body.0["held"], "option_unmatched");
    assert_eq!(body.0["resolved"], false);
    assert_eq!(body.0["notice_delivered"], true);
}

/// 🔴 #10127 (plan §6): an `unparsed` append that fails drops the reply as
/// `table_poisoned` and sends no notice; the question stays open.
#[tokio::test]
async fn unparsed_write_failure_sends_no_notice_and_keeps_question_open() {
    let fake = Fake::new();
    let id = fake.open_with("owner-0", OWNER, &owner_conv(), &["Merge", "Hold"]);
    let rec = recorder_for(&fake);
    let mut opt = OptEnv::new(fake);
    opt.faults.mark = Some(UnparsedOutcome::Refused(ResolveOutcome::Poisoned));
    let opt = Arc::new(opt);
    let env: Arc<dyn InboundEnv + Send + Sync> = Arc::clone(&opt) as _;
    let outcome = run_inbound(env, &rec, ev(OWNER, &owner_conv(), "maybe")).await;
    let dropped = InboundReply::Dropped(Dropped {
        code: "table_poisoned",
        first: None,
    });
    assert_eq!(outcome, dropped);
    assert_eq!(opt.marks(), 1);
    assert!(rec.events().is_empty(), "no asker read, no notice");
    assert_eq!(opt.fake.audits(), vec![("table_poisoned", false)]);
    assert!(opt.fake.table.unparsed(id).is_empty());
    assert!(!opt.fake.table.state(id).expect("known").answered);
}

/// 🔴 #10127 (plan §6): a named option whose `resolve` is poisoned, unknown
/// or already answered sends no frame and taints nothing.
#[tokio::test]
async fn matched_reply_with_poisoned_table_delivers_nothing() {
    let first = Answer {
        answerer: OWNER.into(),
        answered_at: "2026-10-21T00:00:00.000Z".into(),
        text_sha256: "0".repeat(64),
        text_len: 4,
    };
    for (refused, code) in [
        (ResolveOutcome::Poisoned, "table_poisoned"),
        (ResolveOutcome::Unknown, "unknown_question"),
        (
            ResolveOutcome::AlreadyResolved { first },
            "already_resolved",
        ),
    ] {
        let mut fake = Fake::new();
        fake.table_mode = TableMode::Resolves(refused);
        fake.open_with("owner-0", OWNER, &owner_conv(), &["Merge", "Hold"]);
        let rec = recorder_for(&fake);
        let fake = Arc::new(fake);
        let env: Arc<dyn InboundEnv + Send + Sync> = Arc::clone(&fake) as _;
        let outcome = run_inbound(env, &rec, ev(OWNER, &owner_conv(), "Hold")).await;
        let InboundReply::Dropped(d) = &outcome else {
            panic!("{code}: {outcome:?}");
        };
        assert_eq!(d.code, code);
        assert_eq!(fake.resolves(), 1, "{code}");
        assert!(rec.events().is_empty(), "{code}: nothing sent");
        assert!(!rec.dir.exists(), "{code}: no taint, no trace");
    }
}

/// A live-Architect view over the daemon's real deliverer, so the untainted
/// paths reach `SessionManager::inject`.
struct LiveArchitect {
    env: DaemonDeliverEnv,
}

impl DeliverEnv for LiveArchitect {
    async fn asker(&self, _managed: &str) -> Result<AskerView, String> {
        Ok(live_view())
    }

    async fn taint(&self, _: &str, _: &str, _: &TaintMark) -> Result<(), String> {
        panic!("the untainted paths never taint");
    }

    async fn record(&self, line: &DeliveryLine) -> Result<(), String> {
        self.env.record(line).await
    }

    async fn send(&self, managed: &str, line: &str) -> Result<(), String> {
        self.env.send(managed, line).await
    }
}

/// 🔴 #10127 (df-Da4): the option line and the notice go through the same
/// `inject` as an answer, so a pane whose harness has exited types neither.
#[tokio::test]
async fn new_paths_keep_the_no_live_harness_refusal() {
    for verdict in [PaneClaude::Absent, PaneClaude::Unknown, PaneClaude::Present] {
        let dir = crate::test_support::hermetic_temp_dir();
        let (env, driver, id) = deliver_env_on_pane(&dir).await;
        *driver.pane_claude_override.lock().expect("probe") = Some(verdict);
        let env = LiveArchitect { env };
        let mut question = resolved("unused").question().clone();
        question.ask.asker_managed_session = id;
        let options = vec!["Merge".to_owned(), "Hold".to_owned()];
        let choice = match_reply(&options, "2").expect("option 2");
        let answered = deliver_option(&env, &question, &choice).await;
        let held = Held::for_test(question, HoldReason::Unmatched);
        let noticed = deliver_notice(&env, &held).await;
        if verdict == PaneClaude::Present {
            assert_eq!(
                (answered, noticed),
                (Delivery::Delivered, Delivery::Delivered)
            );
            assert_eq!(keys_typed(&driver), 2, "both lines typed");
            continue;
        }
        let refused = |code| Delivery::Failed { code, sent: false };
        assert_eq!(answered, refused("send_failed"), "{verdict:?}");
        assert_eq!(noticed, refused("notice_send_failed"), "{verdict:?}");
        assert_eq!(keys_typed(&driver), 0, "{verdict:?}: nothing typed");
    }
}
