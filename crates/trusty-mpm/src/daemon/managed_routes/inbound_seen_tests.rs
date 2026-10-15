//! #9877 (closure 1): a lost `SessionEnd` never lets an inbound answer reach
//! a Claude id that has no taint file.
//!
//! Why: when `SessionEnd(X)` never arrives, the managed record stays pinned
//! to X, and the `/clear` that started Y is refused by the #4337 guard while
//! X's transcript exists. The deliverer reads X twice and sends to the pane
//! that now runs Y; the egress guard reads only `<Y>.json`.
//! What: the daemon's real `SessionStart` path (`ingest_hook`) under an
//! isolated `HOME`, then the real `deliver` over a deliverer that reads the
//! real record and taints through the real `taint_managed_session`. Only the
//! asker's kind and workspace (the record is a plain session in its cwd) and
//! the pane are faked.
//! Test: this is the test module.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serial_test::serial;
use trusty_channels::policy::Channel;

use crate::channels::deliver::{AskerView, DeliverEnv, Delivery, deliver};
use crate::channels::inbound::Resolved;
use crate::channels::question_table::{Ask, Conversation, Question};
use crate::core::channels_delivery::DeliveryLine;
use crate::core::channels_taint::{self, TaintMark};
use crate::core::hook::HookEvent;
use crate::daemon::api::HookPost;
use crate::daemon::rpc::sessions_legacy_ops::ingest_hook;
use crate::daemon::state::DaemonState;
use crate::session_manager::{ManagedSessionId, SessionKind};

/// The Claude id the record holds; its `SessionEnd` is lost.
const X: &str = "550e8400-e29b-41d4-a716-446655449877";
/// The Claude id `/clear` starts in the same pane.
const Y: &str = "550e8400-e29b-41d4-a716-446655449878";

/// `HOME` pointed at a temp dir for one `#[serial]` test, restored on drop.
struct HomeGuard {
    prev: Option<String>,
    tmp: tempfile::TempDir,
}

impl HomeGuard {
    fn set() -> Self {
        let tmp = crate::test_support::hermetic_temp_dir();
        let prev = std::env::var("HOME").ok();
        // SAFETY: the caller is #[serial], so no other HOME writer runs.
        unsafe { std::env::set_var("HOME", tmp.path()) };
        Self { prev, tmp }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.prev.take() {
            // SAFETY: as in `set`.
            Some(prev) => unsafe { std::env::set_var("HOME", prev) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

/// X's transcript, live under the managed config dir, so the #4337 guard
/// refuses Y while it exists.
fn seed_live_transcript(home: &Path, cwd: &Path, id: &str) {
    let dir = crate::core::trusty_tools_config::managed_claude_config_dir_at(home)
        .join("projects")
        .join(crate::runtime::encode_project_dir(cwd));
    std::fs::create_dir_all(&dir).expect("projects dir");
    std::fs::write(dir.join(format!("{id}.jsonl")), "{}").expect("transcript");
}

fn session_start(id: &str, ws: &Path) -> HookPost {
    HookPost {
        session_id: id.to_owned(),
        event: HookEvent::SessionStart,
        payload: serde_json::json!({ "cwd": ws.display().to_string() }),
    }
}

/// The asker's view from the daemon's real record, as the Architect.
struct LiveRecord {
    state: Arc<DaemonState>,
    dir: PathBuf,
    /// Whether Y read tainted at each send.
    y_at_send: Mutex<Vec<bool>>,
    lines: Mutex<Vec<DeliveryLine>>,
}

impl DeliverEnv for LiveRecord {
    async fn asker(&self, managed: &str) -> Result<AskerView, String> {
        let id = ManagedSessionId(uuid::Uuid::parse_str(managed).map_err(|e| e.to_string())?);
        let mgr = self.state.session_manager().await;
        let record = mgr.get(&id).await.map_err(|e| e.to_string())?;
        // `create` records the workspace as the cwd only.
        Ok(AskerView {
            kind: SessionKind::Architect,
            workspace: Some(record.workspace_path.clone().unwrap_or(record.cwd.clone())),
            ..AskerView::of(&record)
        })
    }

    async fn taint(&self, managed: &str, claude: &str, mark: &TaintMark) -> Result<(), String> {
        channels_taint::taint_managed_session(&self.dir, managed, claude, mark)
            .map_err(|e| e.to_string())
    }

    async fn record(&self, line: &DeliveryLine) -> Result<(), String> {
        self.lines.lock().expect("lines").push(line.clone());
        Ok(())
    }

    async fn send(&self, _managed: &str, _line: &str) -> Result<(), String> {
        let tainted = channels_taint::read_state(&self.dir, Y).is_tainted();
        self.y_at_send.lock().expect("sends").push(tainted);
        Ok(())
    }
}

/// A resolved answer to a question `asker` asked.
fn resolved_for(asker: &str) -> Resolved {
    let question = Question {
        id: 9877,
        opened_at: "2026-10-14T00:00:00.000Z".into(),
        ask: Ask {
            channel: Channel::Slack,
            route: "owner-0".into(),
            recipient: "U0OWNER1".into(),
            asker_managed_session: asker.to_owned(),
            conversation: Conversation {
                dm_channel_id: "D0OWNERDM".into(),
                thread_ts: "1700000000.000100".into(),
            },
            message_ts: "1700000000.000100".into(),
        },
    };
    Resolved::for_test(question, "U0OWNER1", "approve")
}

/// 🔴 #9877 (closure 1): SessionEnd(X) is never delivered, Y starts, an
/// answer is resolved; Y has a taint file before the send, or nothing is
/// sent and the failure is recorded.
#[tokio::test]
#[serial]
async fn lost_session_end_new_id_is_tainted_before_the_send() {
    let home = HomeGuard::set();
    let root = crate::test_support::hermetic_temp_dir();
    let ws_dir = crate::test_support::hermetic_temp_dir();
    let ws = std::fs::canonicalize(ws_dir.path()).expect("canonical ws");
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let mgr = state.session_manager().await;
    let rec = mgr
        .create("asker".into(), Some(ws.clone()), None, None, None, None)
        .await
        .expect("create");
    mgr.stop(&rec.id).await.expect("stop");
    mgr.resume(&rec.id).await.expect("resume");

    ingest_hook(&state, session_start(X, &ws))
        .await
        .expect("SessionStart(X)");
    seed_live_transcript(home.tmp.path(), &ws, X);
    // SessionEnd(X) is lost. `/clear` starts Y in the same pane.
    ingest_hook(&state, session_start(Y, &ws))
        .await
        .expect("SessionStart(Y)");
    let pinned = mgr.get(&rec.id).await.expect("record").claude_session_id;
    assert_eq!(
        pinned.as_deref(),
        Some(X),
        "precondition: the record stays on X"
    );

    let env = LiveRecord {
        dir: channels_taint::taint_dir(&state.user_home().expect("pinned home")),
        state: Arc::clone(&state),
        y_at_send: Mutex::new(Vec::new()),
        lines: Mutex::new(Vec::new()),
    };
    let outcome = deliver(&env, &resolved_for(&rec.id.to_string())).await;

    let sends = env.y_at_send.lock().expect("sends").clone();
    match outcome {
        Delivery::Delivered => assert_eq!(sends, [true], "Y untainted at the send"),
        // Only a refusal the taint or the re-read makes counts, never an
        // asker this fixture failed to build.
        Delivery::Failed { code, sent } => {
            assert!(
                ["taint_failed", "asker_changed"].contains(&code),
                "refused before the taint: {code}"
            );
            assert!(
                !sent && sends.is_empty(),
                "a refused delivery sent: {sends:?}"
            );
            assert!(
                !env.lines.lock().expect("lines").is_empty(),
                "the refusal is not recorded"
            );
        }
    }
}
