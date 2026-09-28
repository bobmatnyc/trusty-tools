//! Tests that a stopped builder's slot index is not reissued while the agent
//! may resume (#8548 critic round).
//!
//! Why: a stop frees the slot's capacity at once, but a resumed agent goes on
//! building in the directory it was given. Each test drives only the surfaces
//! that predate the fix — the sweep, the hook tracker, the claim — so it
//! compiles and fails against the tree before it.
//! Test: this file IS the test module.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::core::agent::{Delegation, DelegationStatus, ModelTier};
use crate::core::builder_slot_pool::{SEED_MARKER, SlotPool, test_support::write_checkout};
use crate::core::hook::HookEvent;
use crate::core::session::SessionId;
use crate::daemon::services::delegation_tracker::observe;

use super::builder_slots::{BUILDER_LEASE_TTL_SECS, BuilderSlotGrant};
use super::core::DaemonState;

const AGENT_ID: &str = "a77c0ffeebuilder";
const TOOL_USE_ID: &str = "toolu_01Qstopped";

/// A running builder on slot 0, dispatched from `<root>/proj/sess.jsonl`.
fn builder_on_slot_zero(state: &DaemonState, root: &Path) -> Delegation {
    let mut d = Delegation::new(
        SessionId::new(),
        None,
        "rust-engineer",
        ModelTier::Sonnet,
        "build it",
    );
    d.status = DelegationStatus::Running;
    d.started_at = Some(chrono::Utc::now() - chrono::Duration::minutes(29));
    d.agent_id = Some(AGENT_ID.to_string());
    d.tool_use_id = Some(TOOL_USE_ID.to_string());
    d.transcript_path = Some(root.join("proj").join("sess.jsonl"));
    d.cwd = Some(root.join("repo"));
    d.builder_slot = Some(0);
    state.upsert_delegation(d.clone());
    d
}

/// Write the agent's sidecar with `stoppedByUser` set to `stopped`.
fn write_sidecar(root: &Path, stopped: bool) -> PathBuf {
    let dir = root.join("proj").join("sess").join("subagents");
    std::fs::create_dir_all(&dir).expect("sidecar dir");
    let path = dir.join(format!("agent-{AGENT_ID}.meta.json"));
    let body = format!(r#"{{"toolUseId":"{TOOL_USE_ID}","stoppedByUser":{stopped}}}"#);
    std::fs::write(&path, body).expect("sidecar");
    path
}

fn record(state: &DaemonState, d: &Delegation) -> Delegation {
    state
        .delegations_for(d.session)
        .into_iter()
        .find(|r| r.id == d.id)
        .expect("record kept")
}

/// A new running builder dispatch carrying `tool_use_id`, not yet recorded.
fn new_builder(tool_use_id: &str) -> Delegation {
    let mut d = Delegation::new(
        SessionId::new(),
        None,
        "python-engineer",
        ModelTier::Sonnet,
        "b",
    );
    d.status = DelegationStatus::Running;
    d.started_at = Some(chrono::Utc::now());
    d.tool_use_id = Some(tool_use_id.to_string());
    d
}

/// Claim a builder slot for a new dispatch; returns (admitted, slot index).
fn claim(state: &DaemonState, cap: u32, tool_use_id: &str) -> (bool, Option<u32>) {
    let d = new_builder(tool_use_id);
    let (_, claimed) = state.claim_builder_slot(
        cap,
        Some(tool_use_id),
        true,
        |s| s.upsert_delegation(d.clone()),
        |_| {},
    );
    (claimed, record(state, &d).builder_slot)
}

/// The BLOCK: the user stops a builder, resumes it, and before the daemon sees
/// the resume a new builder claims. It must get the freed capacity but NOT the
/// stopped builder's directory.
#[test]
fn a_resumed_user_stopped_builder_keeps_its_slot_index_8548() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = DaemonState::new();
    let holder = builder_on_slot_zero(&state, tmp.path());
    write_sidecar(tmp.path(), true);
    state.sweep_delegations();
    assert_eq!(record(&state, &holder).status, DelegationStatus::Cancelled);

    write_sidecar(tmp.path(), false);
    let (claimed, index) = claim(&state, 1, "toolu_NEXT");

    assert!(claimed, "the stop frees the capacity at once");
    assert_eq!(
        index,
        Some(1),
        "slot 0 belongs to an agent that may resume into it"
    );
    state.sweep_delegations();
    assert_eq!(record(&state, &holder).status, DelegationStatus::Running);
    assert_eq!(record(&state, &holder).builder_slot, Some(0));
}

/// The same hole through the PM's `TaskStop`, re-armed by the resumed agent's
/// own tool call.
#[test]
fn a_resumed_task_stopped_builder_keeps_its_slot_index_8548() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = DaemonState::new();
    let holder = builder_on_slot_zero(&state, tmp.path());
    let stop = serde_json::json!({"tool": "TaskStop", "input": {"task_id": AGENT_ID}});
    observe(&state, holder.session, HookEvent::PostToolUse, &stop);
    assert_eq!(record(&state, &holder).status, DelegationStatus::Cancelled);

    let (claimed, index) = claim(&state, 1, "toolu_NEXT");
    assert!(claimed, "the stop frees the capacity at once");
    assert_eq!(index, Some(1), "slot 0 belongs to a stopped agent");

    let activity = serde_json::json!({
        "agent_id": AGENT_ID,
        "cwd": tmp.path().join("repo"),
        "tool": "Bash",
        "tool_use_id": "toolu_sub",
    });
    // A tool call inside the grace window predates the stop: no re-arm.
    observe(&state, holder.session, HookEvent::PreToolUse, &activity);
    assert_eq!(record(&state, &holder).status, DelegationStatus::Cancelled);
    state.mutate_delegation(holder.id, |d| {
        d.ended_at = Some(chrono::Utc::now() - chrono::Duration::seconds(60));
    });
    observe(&state, holder.session, HookEvent::PreToolUse, &activity);
    assert_eq!(
        record(&state, &holder).status,
        DelegationStatus::Running,
        "the resumed agent's tool call re-arms its lease"
    );
}

/// The sidecar flipping to `stoppedByUser: false` re-arms the released lease.
#[test]
fn a_resume_marker_rearms_the_released_lease_8548() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = DaemonState::new();
    let holder = builder_on_slot_zero(&state, tmp.path());
    write_sidecar(tmp.path(), true);
    state.sweep_delegations();
    assert!(state.builder_slot_holders(None).is_empty(), "precondition");

    write_sidecar(tmp.path(), false);
    state.sweep_delegations();

    let now = record(&state, &holder);
    assert_eq!(now.status, DelegationStatus::Running);
    assert_eq!(now.ended_at, None);
    assert_eq!(state.builder_slot_holders(None).len(), 1);
}

/// The quarantine is bounded by the lease TTL: past it, the index is free.
#[test]
fn a_quarantined_index_is_reusable_after_the_ttl_8548() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = DaemonState::new();
    let holder = builder_on_slot_zero(&state, tmp.path());
    write_sidecar(tmp.path(), true);
    state.sweep_delegations();

    let (_, inside) = claim(&state, 1, "toolu_B");
    assert_eq!(inside, Some(1), "inside the TTL slot 0 is quarantined");
    let b = state
        .all_delegations()
        .into_iter()
        .find(|d| d.tool_use_id.as_deref() == Some("toolu_B"))
        .expect("B recorded");
    state.terminate_delegation(b.id, DelegationStatus::Completed);
    state.mutate_delegation(holder.id, |d| {
        d.started_at =
            Some(chrono::Utc::now() - chrono::Duration::seconds(BUILDER_LEASE_TTL_SECS + 60));
    });

    let (claimed, after) = claim(&state, 1, "toolu_C");
    assert!(claimed);
    assert_eq!(after, Some(0), "past the TTL the index is reusable");
}

/// A pool under `root` whose handovers resolve packages from `checkout`.
fn pool_at(root: &Path, checkout: PathBuf) -> SlotPool {
    let identity = trusty_common::github_path::GithubPath {
        owner: "acme".to_string(),
        repo: "widgets".to_string(),
    };
    SlotPool::new(root.join("pool"), identity, 4).with_checkout(checkout)
}

/// The `served:` line of slot `index`'s marker.
fn served(pool: &SlotPool, index: u32) -> String {
    std::fs::read_to_string(pool.slot_path(index).join(SEED_MARKER))
        .expect("marker")
        .lines()
        .find(|line| line.starts_with("served: "))
        .unwrap_or_default()
        .to_string()
}

/// Claim a slot from `pool` for new builder `d` on its own thread; `done` is
/// set once the claim has returned.
fn claim_on_thread(
    state: &Arc<DaemonState>,
    pool: SlotPool,
    d: Delegation,
    done: Arc<AtomicBool>,
) -> std::thread::JoinHandle<BuilderSlotGrant> {
    let state = Arc::clone(state);
    std::thread::spawn(move || {
        let tool_use_id = d.tool_use_id.clone();
        let grant = state.claim_builder_slot_with_pool(
            2,
            tool_use_id.as_deref(),
            true,
            Some(&pool),
            move |s| s.upsert_delegation(d),
            |_| {},
        );
        done.store(true, Ordering::SeqCst);
        grant
    })
}

/// #8548 r2: #8548 stop releases that land while a claim is mid-handover
/// cannot let a second claim hand over the same slot. The handover runs under
/// the claim mutex, so the second claim waits for it, and each stopped record
/// keeps its index. The claimant's checkout `Cargo.lock` is a FIFO: it parks
/// `hand_over` inside the claim until the test writes it, so no sleep orders
/// the steps. Moving `hand_over` outside the mutex fails the first assert.
#[test]
fn a_stop_release_mid_handover_cannot_free_the_slot_for_a_second_claim_8548() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(DaemonState::new());
    let checkout = write_checkout(&tmp.path().join("checkout"));
    let pool = pool_at(tmp.path(), checkout.clone());
    for index in [0, 1] {
        pool.seed(index, None).expect("a seeded slot");
    }
    let holder = builder_on_slot_zero(&state, tmp.path());
    pool.hand_over(0, TOOL_USE_ID).expect("A holds slot 0");
    let parked = tmp.path().join("parked");
    std::fs::create_dir_all(&parked).expect("parked checkout");
    let fifo = parked.join("Cargo.lock");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo failed");

    let b = new_builder("toolu_B");
    let b_id = b.id;
    let b_done = Arc::new(AtomicBool::new(false));
    let claim_b = claim_on_thread(
        &state,
        pool_at(tmp.path(), parked.clone()),
        b,
        Arc::clone(&b_done),
    );
    // A non-blocking write-open succeeds only once B's `hand_over` holds the
    // FIFO open for reading; a claim that returned first fails, never hangs.
    let mut writer = loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
        {
            Ok(writer) => break writer,
            Err(e) if e.raw_os_error() == Some(libc::ENXIO) => {
                assert!(
                    !b_done.load(Ordering::SeqCst),
                    "claim B returned without reaching hand_over"
                );
                std::thread::yield_now();
            }
            Err(e) => panic!("open the FIFO: {e}"),
        }
    };
    assert!(
        state.builder_claim.try_lock().is_none(),
        "hand_over must run under the builder claim mutex"
    );

    // Mid-handover: the user stops A (what a concurrent claim route runs
    // first), and a TaskStop ends B's own record.
    write_sidecar(tmp.path(), true);
    state.reconcile_builder_stop_markers();
    assert_eq!(record(&state, &holder).status, DelegationStatus::Cancelled);
    assert!(
        state.cancel_task_stopped(b_id),
        "B's record is stop-released"
    );
    let c_done = Arc::new(AtomicBool::new(false));
    let claim_c = claim_on_thread(&state, pool.clone(), new_builder("toolu_C"), c_done);

    let lock = std::fs::read(checkout.join("Cargo.lock")).expect("lock body");
    writer.write_all(&lock).expect("release B's hand_over");
    drop(writer);
    let b_grant = claim_b.join().expect("claim B");
    let c_grant = claim_c.join().expect("claim C");

    assert_eq!(b_grant.slot_dir, Some(pool.slot_path(1)), "{b_grant:?}");
    assert!(c_grant.claimed, "the stops free capacity at once");
    assert_eq!(
        c_grant.slot_dir, None,
        "slots 0 and 1 belong to agents that may resume: {c_grant:?}"
    );
    assert_eq!(c_grant.seed_index, Some(2), "{c_grant:?}");
    assert!(served(&pool, 0).starts_with(&format!("served: {TOOL_USE_ID} ")));
    assert!(served(&pool, 1).starts_with("served: toolu_B "));
}
