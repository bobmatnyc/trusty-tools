//! Tests for the untainted option answer and held-reply notice (#10127,
//! S5 P2b).
//!
//! Why: these lines reach the asker's pane with no taint, so each must be
//! built from the stored question alone, reach only the asker, and a failed
//! notice must stay visible and never retry. Each test goes red against a
//! frame that copies the reply, a notice sent elsewhere, or a silent loss.
//! What: the frames directly, then `deliver_notice` over the deliverer's
//! recorder, wrapped to record each send's target.
//! Test: this is the test module.

use std::collections::HashSet;
use std::sync::Mutex;

use trusty_channels::policy::Channel;

use super::tests::{ASKER, Recorder, resolved};
use super::{DeliverEnv, Delivery, deliver_notice, frame_answer, frame_notice, frame_option};
use crate::channels::inbound::Held;
use crate::channels::option_match::{HoldReason, match_reply};
use crate::core::channels_delivery::{DeliveryLine, DeliveryState};
use crate::core::channels_taint::{self, TaintMark};
use crate::core::doctor::CheckStatus;
use crate::daemon::doctor::doctor_channel_taint::channel_taint_row;

/// The recorder, also noting the managed id each send targets.
struct Targets {
    rec: Recorder,
    targets: Mutex<Vec<String>>,
}

impl DeliverEnv for Targets {
    async fn asker(&self, managed: &str) -> Result<super::AskerView, String> {
        self.rec.asker(managed).await
    }

    async fn taint(&self, managed: &str, claude: &str, mark: &TaintMark) -> Result<(), String> {
        self.rec.taint(managed, claude, mark).await
    }

    async fn record(&self, line: &DeliveryLine) -> Result<(), String> {
        self.rec.record(line).await
    }

    async fn send(&self, managed: &str, line: &str) -> Result<(), String> {
        self.targets
            .lock()
            .expect("targets")
            .push(managed.to_owned());
        self.rec.send(managed, line).await
    }
}

/// A reply to question 7 held for `reason`.
fn held(reason: HoldReason) -> Held {
    Held::for_test(resolved("unused").question().clone(), reason)
}

/// The quoted part of a frame: from its first `"` to its last.
fn quoted(line: &str) -> &str {
    let start = line.find('"').expect("an opening quote");
    let end = line.rfind('"').expect("a closing quote");
    &line[start..=end]
}

/// 🔴 #10127 (df-Da7 point 2): the option line holds the id, the option's
/// number and its stored label, escaped as `frame_answer` escapes; no byte
/// the reply alone carries reaches it.
#[test]
fn matched_frame_is_built_only_from_the_stored_question() {
    let options = vec!["Merge".to_owned(), "Option 2 $(x)".to_owned()];
    let reply = "  OPTION 2 $(x) ";
    let choice = match_reply(&options, reply).expect("the reply names option 2");
    let line = frame_option(7, &choice);
    assert_eq!(
        line,
        "[channel answer Q-7: option 2 \"Option 2 \\u0024(x)\"]"
    );
    for reply_only in ["OPTION", "$", "  ", "(x) "] {
        assert!(!line.contains(reply_only), "{reply_only:?} in {line}");
    }
    let label = "Hold \"now\" \\ `id` !! $HOME \u{202e}x";
    let options = vec!["Merge".to_owned(), label.to_owned()];
    let choice = match_reply(&options, "2").expect("option 2");
    let answer = frame_answer(7, Channel::Slack, "U0OWNER1", label);
    assert_eq!(quoted(&frame_option(7, &choice)), quoted(&answer));
}

/// 🔴 #10127: a held reply's notice goes once, to the asker's pane only; it
/// names the question, carries a fixed sentence, and writes no taint.
#[tokio::test]
async fn no_match_notice_goes_only_to_the_asker_pane() {
    for (reason, why) in [
        (HoldReason::Unmatched, "the owner's reply matched no option"),
        (
            HoldReason::FreeText,
            "the question has no options, so no reply is delivered",
        ),
    ] {
        let env = Targets {
            rec: Recorder::new(),
            targets: Mutex::new(Vec::new()),
        };
        let outcome = deliver_notice(&env, &held(reason)).await;
        assert_eq!(outcome, Delivery::Delivered, "{reason:?}");
        assert_eq!(*env.targets.lock().expect("targets"), [ASKER], "{reason:?}");
        let expected = format!(
            "[channel notice Q-7: {why}; the reply is held, not delivered, and the question \
             stays open]"
        );
        assert_eq!(env.rec.sent(), std::slice::from_ref(&expected));
        assert_eq!(frame_notice(7, reason), expected);
        assert_eq!(
            env.rec.events(),
            ["asker", "record:Pending", "send", "record:Delivered"],
            "{reason:?}: no taint step"
        );
        assert!(
            channels_taint::list_taints(&env.rec.dir)
                .expect("list")
                .is_empty()
        );
    }
}

/// 🔴 #10127 (plan §6): a notice whose send fails is sent once, settled
/// `failed` as `notice_send_failed`, taints nothing, and the doctor row
/// names it.
#[tokio::test]
async fn notice_send_failure_is_recorded_and_not_retried() {
    let mut rec = Recorder::new();
    rec.send_fails = true;
    rec.real_trace = true;
    let outcome = deliver_notice(&rec, &held(HoldReason::Unmatched)).await;
    assert_eq!(
        outcome,
        Delivery::Failed {
            code: "notice_send_failed",
            sent: false
        }
    );
    assert_eq!(
        rec.events(),
        ["asker", "record:Pending", "send", "record:Failed"],
        "one send, no retry, no taint"
    );
    assert_eq!(
        rec.states().last(),
        Some(&(DeliveryState::Failed, Some("notice_send_failed".to_owned())))
    );
    assert!(
        channels_taint::list_taints(&rec.dir)
            .expect("list")
            .is_empty()
    );
    let live: HashSet<String> = [ASKER.to_owned()].into();
    let row = channel_taint_row(&rec.dir, &live);
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(
        row.message.contains("Q-7") && row.message.contains("notice_send_failed"),
        "{}",
        row.message
    );
}
