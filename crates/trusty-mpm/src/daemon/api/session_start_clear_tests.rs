//! #10594: a `SessionStart` with `source=clear` in the Architect's directory
//! moves `last.architect-conversation` to the post-clear conversation id.

use std::sync::Arc;

use crate::core::architect_conversation::{
    ConversationStart, conversation_path, project_folder, record_conversation, resolve_conversation,
};
use crate::core::hook::HookEvent;
use crate::daemon::api::HookPost;
use crate::daemon::rpc::sessions_legacy_ops::ingest_hook;
use crate::daemon::state::DaemonState;

const OLD: &str = "550e8400-e29b-41d4-a716-446655440001";
const NEW: &str = "550e8400-e29b-41d4-a716-446655440002";

fn start(id: &str, cwd: &std::path::Path, source: &str) -> HookPost {
    HookPost {
        session_id: id.to_owned(),
        event: HookEvent::SessionStart,
        payload: serde_json::json!({ "cwd": cwd.display().to_string(), "source": source }),
    }
}

/// Claude Code's transcript for `id`, so the resolver accepts a resume.
fn transcript(config: &std::path::Path, dir: &std::path::Path, id: &str) {
    let folder = config.join("projects").join(project_folder(dir));
    std::fs::create_dir_all(&folder).expect("project folder");
    std::fs::write(folder.join(format!("{id}.jsonl")), "{}\n").expect("transcript");
}

/// The daemon's root, the Architect dir and a claude config dir, with the
/// Architect's record on `OLD` and both transcripts present.
async fn architect() -> (Arc<DaemonState>, tempfile::TempDir, std::path::PathBuf) {
    let root = crate::test_support::hermetic_temp_dir();
    let arch = std::fs::canonicalize(root.path())
        .expect("canon")
        .join("arch");
    std::fs::create_dir_all(&arch).expect("arch dir");
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    record_conversation(state.framework_root(), &arch, OLD).expect("record");
    let config = root.path().join("claude-config");
    transcript(&config, &arch, OLD);
    transcript(&config, &arch, NEW);
    (state, root, arch)
}

fn resolve(
    state: &DaemonState,
    root: &tempfile::TempDir,
    arch: &std::path::Path,
) -> ConversationStart {
    let config = root.path().join("claude-config");
    resolve_conversation(state.framework_root(), arch, Some(&config))
}

/// 🔴 #10594: after `/clear` in the Architect, a relaunch resumes the
/// post-clear conversation, though the pre-clear transcript is still live.
#[tokio::test]
async fn architect_clear_moves_the_conversation_record_to_the_new_id() {
    let (state, root, arch) = architect().await;
    ingest_hook(&state, start(NEW, &arch, "clear"))
        .await
        .expect("SessionStart accepted");
    assert_eq!(
        resolve(&state, &root, &arch),
        ConversationStart::Resume(NEW.to_owned())
    );
}

/// #10594 Fail-Open Check: a `clear` in another directory, or a non-clear
/// source in the Architect's, leaves the record on the pre-clear id.
#[tokio::test]
async fn non_architect_or_non_clear_session_start_leaves_the_record() {
    let (state, root, arch) = architect().await;
    let other = arch.parent().expect("parent").join("worker");
    std::fs::create_dir_all(&other).expect("worker dir");
    ingest_hook(&state, start(NEW, &other, "clear"))
        .await
        .expect("SessionStart accepted");
    ingest_hook(&state, start(NEW, &arch, "startup"))
        .await
        .expect("SessionStart accepted");
    assert!(conversation_path(state.framework_root()).is_file());
    assert_eq!(
        resolve(&state, &root, &arch),
        ConversationStart::Resume(OLD.to_owned())
    );
}
