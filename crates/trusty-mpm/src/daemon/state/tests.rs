use std::path::PathBuf;

use crate::core::hook::HookEvent;
use crate::core::hook::HookEventRecord;
use crate::core::memory::MemoryPressure;
use crate::core::memory::MemoryUsage;
use crate::core::overseer_config::OverseerConfig;
use crate::core::paths::FrameworkPaths;
use crate::core::session::{ControlModel, SessionStatus};

use super::core::{DaemonState, HOOK_HISTORY_LIMIT, ReapResult};
use super::overseer::build_overseer;

use crate::core::session::{Session, SessionId};

/// Build a [`DaemonState`] rooted under an empty temp directory.
///
/// Why: tests that assert overseer/LLM config defaults must NOT read from the
/// real `~/.trusty-mpm/framework/hooks/` — on a dev machine the operator's
/// `overseer.toml` may have `[llm] enabled = true` with a live API key, which
/// would make "disabled by default" assertions fail (#1571). Pointing
/// [`DaemonState::with_paths`] at a freshly-created, empty temp directory
/// guarantees that `overseer.toml` and `optimizer.toml` are absent, so the
/// daemon falls back to its disabled/default policy regardless of the host
/// environment. The `TempDir` is returned so the caller can hold it alive for
/// the test's duration; the paths it contains are only consulted at
/// construction time, so the directory can be dropped afterwards without
/// affecting the in-memory state.
/// What: creates a `tempfile::TempDir`, builds `FrameworkPaths::under` it, and
/// calls `DaemonState::with_paths`, returning both.
/// Test: `new_overseer_is_disabled_when_file_missing`, `overseer_is_accessible`,
/// `llm_overseer_is_none_without_key`, `overseer_handler_reports_strategy`.
fn hermetic_state() -> (DaemonState, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir for hermetic DaemonState");
    let paths = FrameworkPaths::under(dir.path());
    let state = DaemonState::with_paths(&paths);
    (state, dir)
}

fn sample_session() -> Session {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut s = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
    s.tmux_name = format!("tmpm-test-{n}");
    s.status = SessionStatus::Active;
    s
}

#[test]
fn register_and_list_sessions() {
    let state = DaemonState::new();
    let s = sample_session();
    let id = s.id;
    state.register_session(s);
    assert_eq!(state.list_sessions().len(), 1);
    assert!(state.session(id).is_some());
    assert!(state.remove_session(id).is_some());
    assert!(state.list_sessions().is_empty());
}

#[test]
fn update_session_mutates_existing() {
    let state = DaemonState::new();
    let s = sample_session();
    let id = s.id;
    state.register_session(s);
    let ran = state.update_session(&id, |session| {
        session.status = SessionStatus::Paused;
        session.pause_summary = Some("note".to_string());
    });
    assert!(ran);
    let updated = state.session(id).expect("session exists");
    assert_eq!(updated.status, SessionStatus::Paused);
    assert_eq!(updated.pause_summary.as_deref(), Some("note"));
}

#[test]
fn update_session_missing_is_false() {
    let state = DaemonState::new();
    let ran = state.update_session(&SessionId::new(), |_| {});
    assert!(!ran);
}

#[test]
fn register_and_list_projects() {
    let state = DaemonState::new();
    assert!(state.list_projects().is_empty());
    let info = state.register_project(PathBuf::from("/work/demo"));
    assert_eq!(info.name, "demo");
    assert_eq!(state.list_projects().len(), 1);
    // Re-registering the same path replaces rather than duplicates.
    state.register_project(PathBuf::from("/work/demo"));
    assert_eq!(state.list_projects().len(), 1);
    state.register_project(PathBuf::from("/work/other"));
    assert_eq!(state.list_projects().len(), 2);
}

#[test]
fn project_lookup_by_path() {
    let state = DaemonState::new();
    state.register_project(PathBuf::from("/work/demo"));
    assert!(state.project(std::path::Path::new("/work/demo")).is_some());
    assert!(
        state
            .project(std::path::Path::new("/work/missing"))
            .is_none()
    );
}

#[test]
fn list_sessions_for_project_filters() {
    let state = DaemonState::new();
    let mut in_proj = sample_session();
    in_proj.project_path = Some(PathBuf::from("/work/demo"));
    let mut other_proj = sample_session();
    other_proj.project_path = Some(PathBuf::from("/work/other"));
    let no_proj = sample_session();
    state.register_session(in_proj.clone());
    state.register_session(other_proj);
    state.register_session(no_proj);

    let listed = state.list_sessions_for_project(std::path::Path::new("/work/demo"));
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, in_proj.id);
}

#[test]
fn find_session_by_id_or_name() {
    let state = DaemonState::new();
    let s = sample_session();
    let id = s.id;
    let name = s.tmux_name.clone();
    state.register_session(s);

    assert!(state.find_session(&id.0.to_string()).is_some());
    assert!(state.find_session(&name).is_some());
    assert!(state.find_session("tmpm-no-such-name").is_none());
    assert!(
        state
            .find_session(&SessionId::new().0.to_string())
            .is_none()
    );
}

#[test]
fn breaker_tracks_outcomes() {
    let state = DaemonState::new();
    // Default threshold is 3 consecutive failures.
    for _ in 0..3 {
        state.record_outcome("research", false);
    }
    let cb = state.breaker("research");
    assert!(!cb.allows_delegation());
    // A success resets the counter (after an attempt_reset path it closes).
    state.record_outcome("research", true);
    assert_eq!(state.breaker("research").consecutive_failures, 0);
}

#[test]
fn memory_pressure_is_classified() {
    let state = DaemonState::new();
    let id = SessionId::new();
    let pressure = state.record_memory(
        id,
        MemoryUsage {
            used_tokens: 900,
            window_tokens: 1000,
        },
    );
    assert_eq!(pressure, MemoryPressure::Compact);
    assert!(state.memory_for(id).is_some());
}

#[test]
fn trusty_addrs_round_trip() {
    let state = DaemonState::new();
    assert!(state.trusty_addrs().is_none());
    let addrs = crate::daemon::discover::TrustyAddrs {
        memory: "127.0.0.1:3038".parse().unwrap(),
        search: "127.0.0.1:7878".parse().unwrap(),
    };
    state.set_trusty_addrs(addrs);
    let got = state.trusty_addrs().expect("addrs stored");
    assert_eq!(got.memory, "127.0.0.1:3038".parse().unwrap());
    assert_eq!(got.search, "127.0.0.1:7878".parse().unwrap());
}

#[test]
fn reap_dead_sessions() {
    // Three registered sessions; tmux reports only two of them alive.
    // `reap_against` (the testable core of `reap_dead_sessions`) must drop
    // exactly the one whose tmux_name is absent from the live set.
    let state = DaemonState::new();
    let alive_a = sample_session();
    let alive_b = sample_session();
    let dead = sample_session();
    let (id_a, id_b, id_dead) = (alive_a.id, alive_b.id, dead.id);
    state.register_session(alive_a.clone());
    state.register_session(alive_b.clone());
    state.register_session(dead);
    assert_eq!(state.list_sessions().len(), 3);

    let live: std::collections::HashSet<String> =
        [alive_a.tmux_name.clone(), alive_b.tmux_name.clone()]
            .into_iter()
            .collect();
    let result = state.reap_against(&live);

    assert_eq!(result.reaped, 1);
    assert_eq!(result.stopped, 0);
    assert!(state.session(id_a).is_some());
    assert!(state.session(id_b).is_some());
    assert!(state.session(id_dead).is_none());

    // Reaping again is idempotent — nothing left to remove.
    assert_eq!(state.reap_against(&live), ReapResult::default());
}

#[test]
fn reap_against_empty_live_removes_all_tmux_sessions() {
    // An empty live set (e.g. tmux server fully stopped) drops every
    // tmux-hosted entry.
    let state = DaemonState::new();
    state.register_session(sample_session());
    state.register_session(sample_session());
    let result = state.reap_against(&std::collections::HashSet::new());
    assert_eq!(result.reaped, 2);
    assert!(state.list_sessions().is_empty());
}

#[test]
fn reap_keeps_native_sessions() {
    // Native (Terminal.app) sessions have no tmux session; the tmux-based
    // reaper must never delete them, even against an empty live set.
    let state = DaemonState::new();
    let mut native = sample_session();
    native.origin = crate::core::session::SessionHost::Native;
    native.pid = Some(9999);
    let native_id = native.id;
    let tmux = sample_session();
    let tmux_id = tmux.id;
    state.register_session(native);
    state.register_session(tmux);

    let result = state.reap_against(&std::collections::HashSet::new());

    // Only the tmux-hosted session is reaped.
    assert_eq!(result.reaped, 1);
    assert!(state.session(native_id).is_some());
    assert!(state.session(tmux_id).is_none());
}

#[test]
fn set_session_pid_updates_field() {
    // Registering a session leaves `pid` unset; set_session_pid records it.
    // Use a hermetic state so the PID-file write lands under a temp dir, never
    // the operator's real `~/.trusty-mpm/pids`.
    let (state, _tmp) = hermetic_state();
    let s = sample_session();
    let id = s.id;
    state.register_session(s);
    assert_eq!(state.session(id).unwrap().pid, None);

    assert!(state.set_session_pid(id, 4242));
    assert_eq!(state.session(id).unwrap().pid, Some(4242));

    // An unknown id is reported as not updated.
    assert!(!state.set_session_pid(SessionId::new(), 1));
}

#[test]
fn pid_registry_is_under_framework_root() {
    // The PID registry must resolve to `<framework_root>/pids` so the spawn,
    // drop, and sweep paths all agree on one directory.
    let (state, _tmp) = hermetic_state();
    let expected = state.framework_root().join("pids");
    assert_eq!(state.pid_registry().dir(), expected.as_path());
}

#[test]
fn set_session_pid_writes_pidfile() {
    // Recording a PID must also write a `<uuid>.pid` file to the registry so a
    // future orphan-GC sweep can find this claude process even after the tmux
    // pane and in-memory entry are gone (§10.3).
    let (state, _tmp) = hermetic_state();
    let s = sample_session();
    let id = s.id;
    state.register_session(s);
    assert!(state.set_session_pid(id, 4242));

    let entries = state.pid_registry().entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session_id, id.0.to_string());
    assert_eq!(entries[0].pid, 4242);

    // Removing the session must clear its PID file (no orphan left behind).
    state.remove_session(id);
    assert!(state.pid_registry().entries().unwrap().is_empty());
}

#[tokio::test]
async fn gather_live_session_ids_unions_both() {
    // The live-id set the PID sweep uses must include every legacy DaemonState
    // session id (the SM store starts empty in a hermetic state, so the legacy
    // ids are what must be present).
    let (state, _tmp) = hermetic_state();
    let a = sample_session();
    let b = sample_session();
    let (id_a, id_b) = (a.id, b.id);
    state.register_session(a);
    state.register_session(b);

    let ids = state.gather_live_session_ids().await;
    assert!(ids.contains(&id_a.0.to_string()));
    assert!(ids.contains(&id_b.0.to_string()));
}

#[test]
fn reap_marks_stopped_when_pid_dead() {
    // A tmux session that is still alive but whose tracked `claude` process
    // has exited (u32::MAX is a guaranteed-dead PID) must be marked Stopped
    // — not removed — so the operator can still see it.
    let state = DaemonState::new();
    let mut session = sample_session();
    session.pid = Some(u32::MAX);
    let id = session.id;
    let tmux_name = session.tmux_name.clone();
    state.register_session(session);

    let live: std::collections::HashSet<String> = [tmux_name].into_iter().collect();
    let result = state.reap_against(&live);

    assert_eq!(result.reaped, 0);
    assert_eq!(result.stopped, 1);
    let after = state.session(id).expect("session is kept, not removed");
    assert_eq!(after.status, SessionStatus::Stopped);
}

#[test]
fn new_reads_default_when_optimizer_file_missing() {
    // With no framework installed (the optimizer.toml file absent), the
    // daemon must still construct, falling back to the default policy.
    let state = DaemonState::new();
    assert_eq!(
        state.optimizer_config().default_level,
        crate::core::compress::CompressionLevel::Trim
    );
}

#[test]
fn reload_optimizer_config_picks_up_file_changes() {
    // Reloading from an explicit temp file must overwrite the in-memory
    // policy with whatever the file declares.
    use std::io::Write;
    let state = DaemonState::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("optimizer.toml");
    let mut file = std::fs::File::create(&path).expect("create file");
    writeln!(file, "[default]\nlevel = \"caveman\"").expect("write file");

    state
        .reload_optimizer_config_from(&path)
        .expect("reload succeeds");
    assert_eq!(
        state.optimizer_config().default_level,
        crate::core::compress::CompressionLevel::Caveman
    );

    // A missing file reloads to the default policy rather than erroring.
    state
        .reload_optimizer_config_from(&dir.path().join("absent.toml"))
        .expect("missing file is not an error");
    assert_eq!(
        state.optimizer_config().default_level,
        crate::core::compress::CompressionLevel::Trim
    );
}

#[test]
fn new_overseer_is_disabled_when_file_missing() {
    // With no framework installed (overseer.toml absent in a hermetic temp
    // dir), the overseer must be present but disabled — oversight is opt-in.
    // Uses `hermetic_state()` so the real `~/.trusty-mpm/overseer.toml` — if
    // present on a dev machine with an API key — cannot enable the LLM
    // overseer and make this assertion spuriously fail (#1571).
    let (state, _dir) = hermetic_state();
    assert!(!state.overseer().is_enabled());
}

#[test]
fn overseer_is_deterministic_without_llm() {
    // With the `[llm]` section absent/disabled, the overseer is the plain
    // deterministic strategy and (with no rules) reports disabled.
    let cfg = OverseerConfig::default();
    let build = build_overseer(cfg);
    assert!(!build.overseer.is_enabled());
    assert_eq!(build.handler, "deterministic");
    assert!(build.llm.is_none());
}

#[test]
fn overseer_falls_back_when_llm_key_missing() {
    // `[llm] enabled = true` but no API key resolves: the daemon must not
    // panic — it falls back to the deterministic overseer.
    let mut cfg = OverseerConfig::default();
    cfg.llm.enabled = true;
    cfg.llm.api_key_env = "TRUSTY_MPM_DEFINITELY_NOT_SET".to_string(); // pragma: allowlist secret
    let build = build_overseer(cfg);
    // Deterministic with no rules and disabled top-level flag → disabled.
    assert!(!build.overseer.is_enabled());
    assert_eq!(build.handler, "deterministic");
    assert!(build.llm.is_none());
}

#[test]
fn llm_overseer_is_none_without_key() {
    // A daemon that starts from an empty framework root (no overseer.toml
    // → no [llm] config) must not build an LLM chat handler, regardless of
    // any API keys present in the operator's environment (#1571).
    let (state, _dir) = hermetic_state();
    assert!(state.llm_overseer().is_none());
}

#[test]
fn overseer_handler_reports_strategy() {
    // A daemon built from an empty framework root reports the deterministic
    // handler — the default when no overseer.toml is installed (#1571).
    let (state, _dir) = hermetic_state();
    assert_eq!(state.overseer_handler(), "deterministic");
}

#[test]
fn overseer_is_accessible() {
    // The shared overseer can be cloned out and queried; with no overseer.toml
    // in the hermetic temp dir the overseer must report disabled (#1571).
    let (state, _dir) = hermetic_state();
    let overseer = state.overseer();
    assert!(!overseer.is_enabled());
}

#[test]
fn audit_logger_is_accessible() {
    let state = DaemonState::new();
    // The audit logger resolves a dated JSONL path under `logs/overseer`.
    let audit = state.audit();
    assert_eq!(
        audit.path().extension().and_then(|e| e.to_str()),
        Some("jsonl")
    );
}

#[test]
fn hook_history_is_bounded() {
    let state = DaemonState::new();
    let id = SessionId::new();
    for _ in 0..(HOOK_HISTORY_LIMIT + 50) {
        state.push_hook_event(HookEventRecord::now(
            id,
            HookEvent::PreToolUse,
            serde_json::Value::Null,
        ));
    }
    assert_eq!(state.recent_hook_events().len(), HOOK_HISTORY_LIMIT);
    assert_eq!(state.hook_events_for(id).len(), HOOK_HISTORY_LIMIT);
}

#[test]
fn pairing_round_trip() {
    // A freshly-generated code confirms once, binds the chat id, and is
    // then consumed so the same code cannot validate twice. The state is
    // rooted at a temp dir so the persisted record never touches HOME.
    let dir = tempfile::tempdir().expect("temp dir");
    let state = DaemonState::with_root(dir.path().to_path_buf());
    assert_eq!(state.paired_chat_id(), None);
    let code = state.generate_pair_code();
    assert_eq!(code.len(), 6);
    assert!(code.chars().all(|c| c.is_ascii_alphanumeric()));
    assert!(state.confirm_pair_code(&code, 12345678));
    assert_eq!(state.paired_chat_id(), Some(12345678));
    // The code was consumed; confirming it again must fail.
    assert!(!state.confirm_pair_code(&code, 999));
}

#[test]
fn wrong_pair_code_is_rejected() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = DaemonState::with_root(dir.path().to_path_buf());
    let _code = state.generate_pair_code();
    assert!(!state.confirm_pair_code("ZZZZZZ", 12345678));
    assert_eq!(state.paired_chat_id(), None);
}

#[test]
fn pairing_persists_to_disk() {
    // Confirming a code writes pairing.json; a fresh state rooted at the
    // same directory restores the binding without a new handshake.
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().to_path_buf();
    let state = DaemonState::with_root(root.clone());
    let code = state.generate_pair_code();
    assert!(state.confirm_pair_code(&code, 555));
    // The on-disk record exists.
    assert_eq!(
        crate::daemon::pairing_store::load(&root).map(|r| r.chat_id),
        Some(555)
    );
    // A new state restores the pairing from disk.
    let restored = DaemonState::with_root(root);
    assert_eq!(restored.paired_chat_id(), Some(555));
}

#[test]
fn pairing_reset_clears_disk() {
    // clear_pairing drops the binding in memory and removes pairing.json.
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().to_path_buf();
    let state = DaemonState::with_root(root.clone());
    let code = state.generate_pair_code();
    assert!(state.confirm_pair_code(&code, 777));
    state.clear_pairing();
    assert_eq!(state.paired_chat_id(), None);
    assert!(crate::daemon::pairing_store::load(&root).is_none());
}

#[test]
fn pairing_code_persists_to_disk() {
    // Minting a code writes the shared pending_pair.json, the single source of
    // truth that every confirm surface validates against (#1500).
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().to_path_buf();
    let state = DaemonState::with_root(root.clone());
    let code = state.generate_pair_code();
    let pending = crate::daemon::pairing_store::load_pending(&root).expect("pending code on disk");
    assert_eq!(pending.code, code);
}

#[test]
fn pairing_confirms_shared_disk_code() {
    // THE #1500 REGRESSION GUARD: a code minted on one DaemonState instance is
    // confirmed on a DIFFERENT instance rooted at the same framework root, even
    // though the second instance never held the code in its in-memory mutex.
    // Before the shared on-disk pending store this returned false ("invalid
    // code") and operators had to pre-seed pairing.json.
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().to_path_buf();

    let minting = DaemonState::with_root(root.clone());
    let code = minting.generate_pair_code();

    // A second, independent daemon instance (e.g. the ephemeral-port duplicate
    // from #1499) shares the framework root but has an empty in-memory pair_code.
    let confirming = DaemonState::with_root(root.clone());
    assert!(
        confirming.confirm_pair_code(&code, 31415),
        "code minted on a sibling instance must validate via the shared store"
    );
    assert_eq!(confirming.paired_chat_id(), Some(31415));

    // The shared pending code was consumed: a third instance cannot replay it.
    let replay = DaemonState::with_root(root);
    assert!(!replay.confirm_pair_code(&code, 999));
}

#[test]
fn concurrent_pairing_confirms() {
    // THE #1506 REGRESSION GUARD: two concurrent confirm attempts race on the
    // same pending code. The atomic rename-based claim must ensure exactly ONE
    // wins; the WINNER must have the device registered in state, the LOSER must
    // not. This catches the pre-fix TOCTOU race where both reads could precede
    // both deletes.
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().to_path_buf();

    let minting = DaemonState::with_root(root.clone());
    let code = minting.generate_pair_code();

    // Use Arc so each thread can own a confirming state.
    let confirming1 = std::sync::Arc::new(DaemonState::with_root(root.clone()));
    let confirming2 = std::sync::Arc::new(DaemonState::with_root(root.clone()));
    let c1_ref = confirming1.clone();
    let c2_ref = confirming2.clone();

    let code1 = code.clone();
    let code2 = code.clone();
    let c1 = std::thread::spawn(move || c1_ref.confirm_pair_code(&code1, 111));
    let c2 = std::thread::spawn(move || c2_ref.confirm_pair_code(&code2, 222));

    let res1 = c1.join().expect("thread 1 must not panic");
    let res2 = c2.join().expect("thread 2 must not panic");

    // Exactly one confirm should succeed due to the atomic claim.
    assert!(
        res1 ^ res2,
        "exactly one concurrent confirm must win the atomic claim (got res1={res1} res2={res2})"
    );

    // The WINNER must have its chat_id registered; the LOSER must not.
    if res1 {
        assert_eq!(
            confirming1.paired_chat_id(),
            Some(111),
            "winner (thread 1) must have chat_id 111 registered"
        );
        assert_eq!(
            confirming2.paired_chat_id(),
            None,
            "loser (thread 2) must not have registered any chat_id"
        );
    } else {
        assert_eq!(
            confirming2.paired_chat_id(),
            Some(222),
            "winner (thread 2) must have chat_id 222 registered"
        );
        assert_eq!(
            confirming1.paired_chat_id(),
            None,
            "loser (thread 1) must not have registered any chat_id"
        );
    }

    // The pending code file must be gone — consumed by the winner's claim.
    assert!(
        crate::daemon::pairing_store::load_pending(&root).is_none(),
        "pending code file must be absent after a winning confirm"
    );
}

// ─── #1744 managed reap tests ───────────────────────────────────────────────

/// Inline minimal [`crate::session_manager::ManagedTmuxDriver`] for reap tests.
struct MinFakeDriver {
    sessions: std::sync::Mutex<Vec<String>>,
}
impl MinFakeDriver {
    fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            sessions: std::sync::Mutex::new(Vec::new()),
        })
    }
}
impl crate::session_manager::ManagedTmuxDriver for MinFakeDriver {
    fn create_session(
        &self,
        name: &str,
        _: &str,
    ) -> Result<(), crate::session_manager::ManagedError> {
        self.sessions.lock().unwrap().push(name.to_owned());
        Ok(())
    }
    fn kill_session(&self, name: &str) -> Result<(), crate::session_manager::ManagedError> {
        self.sessions.lock().unwrap().retain(|n| n != name);
        Ok(())
    }
    fn send_line(&self, _: &str, _: &str) -> Result<(), crate::session_manager::ManagedError> {
        Ok(())
    }
    fn capture(&self, _: &str, _: usize) -> Result<String, crate::session_manager::ManagedError> {
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, crate::session_manager::ManagedError> {
        Ok(self.sessions.lock().unwrap().clone())
    }
}

#[tokio::test]
async fn reap_dead_managed_sessions_marks_stopped() {
    // Why (#1744): reap_managed_against must mark Active managed sessions Stopped
    // when their tmux_name is absent from the live set.
    // What: seed one Active session; call reap_managed_against with an empty live
    // set (simulating the tmux pane having gone away); assert Stopped.
    let tmp = tempfile::TempDir::new().unwrap();
    let fake = MinFakeDriver::new();
    let mgr = crate::session_manager::SessionManager::new(tmp.path(), fake)
        .await
        .expect("session manager");
    let mgr = std::sync::Arc::new(mgr);

    // Create a session and promote to Active.
    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/test-reap")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    let id = record.id;
    mgr.set_workspace(
        &id,
        PathBuf::from("/tmp/test-reap"),
        crate::session_manager::ManagedSessionState::Active,
    )
    .await
    .expect("set Active");

    // Wire into a DaemonState via with_session_manager.
    let state = DaemonState::with_session_manager(std::sync::Arc::clone(&mgr));

    // Simulate the tmux session being gone: pass an empty live set.
    let live = std::collections::HashSet::new();
    state.reap_managed_against(&live).await;

    let after = mgr.get(&id).await.expect("get after reap");
    assert_eq!(
        after.state,
        crate::session_manager::ManagedSessionState::Stopped,
        "reap_managed_against must mark Active session Stopped when tmux_name absent (#1744)"
    );
}
