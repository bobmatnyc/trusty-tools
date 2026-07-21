//! Unit tests for the bundled-agent deploy/refresh module (#3556).
//!
//! Why: pins the original "never overwrite" deploy contract (still used by
//! `deploy_bundled_agents` directly) AND the new stamp-aware refresh
//! behavior — a stale stamp must refresh exactly the bundled files whose
//! content changed, back up any differing on-disk copy first, leave a
//! non-bundled user file completely untouched, and the explicit
//! `force_reprovision_bundled_agents` escape hatch must bypass the stamp
//! check entirely.
//! What: exercises `deploy_bundled_agents`, `ensure_bundled_agents_deployed_in`,
//! and `force_reprovision_bundled_agents` against real embedded bundled
//! content, written to a tempdir so no test touches the real filesystem
//! outside the sandbox.
//! Test: this module IS the test surface.

use super::*;

/// Why: the core contract — a fresh empty target directory gets every
/// embedded file, and the count matches what was actually written.
/// Test: itself.
#[test]
fn deploy_writes_missing_files_only() {
    let tmp = tempfile::tempdir().unwrap();
    let written = deploy_bundled_agents(tmp.path()).unwrap();
    assert!(written > 0, "expected at least one bundled file deployed");
    assert!(
        tmp.path().join("analysis-agent.toml").is_file(),
        "flat .toml bundled agent should be deployed"
    );
}

/// Why: directory-package agents (`assistant/agent.toml` +
/// `assistant/persona.md`) are the primary format the REPL's
/// `/switch assistant` resolves — the exact case the owner's clean-shell
/// repro hit. Both files inside the package directory must land.
/// Test: itself.
#[test]
fn deploy_writes_directory_package_agents() {
    let tmp = tempfile::tempdir().unwrap();
    deploy_bundled_agents(tmp.path()).unwrap();
    assert!(
        tmp.path().join("assistant").join("agent.toml").is_file(),
        "assistant/agent.toml must be deployed"
    );
    assert!(
        tmp.path().join("assistant").join("persona.md").is_file(),
        "assistant/persona.md must be deployed"
    );
}

/// Why: re-running the deploy (every process startup) must not re-write
/// files that already exist — the idempotency contract.
/// Test: itself.
#[test]
fn deploy_is_idempotent_on_rerun() {
    let tmp = tempfile::tempdir().unwrap();
    let first = deploy_bundled_agents(tmp.path()).unwrap();
    assert!(first > 0);
    let second = deploy_bundled_agents(tmp.path()).unwrap();
    assert_eq!(second, 0, "re-running the deploy must write nothing new");
}

/// Why: a user who has customized a bundled agent (or a prior deploy's
/// output) must never be silently overwritten by the plain `deploy_bundled_
/// agents` entry point — the "never clobber" contract that lets a user
/// safely edit their deployed copy without opting into the stamp-aware
/// refresh path.
/// Test: itself.
#[test]
fn deploy_never_overwrites_existing_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("analysis-agent.toml"),
        "# user customized\n",
    )
    .unwrap();
    deploy_bundled_agents(tmp.path()).unwrap();
    let contents = std::fs::read_to_string(tmp.path().join("analysis-agent.toml")).unwrap();
    assert_eq!(contents, "# user customized\n");
}

/// Why: `PathBuf` returned by `deploy_bundled_agents` must be usable
/// directly as an `agents_dir_candidates()`/REPL `$HOME` tier — assert
/// the deployed layout round-trips through `AgentConfig::by_name_in`
/// exactly like a real `$HOME/.trusty-agents/agents` would.
/// Test: itself.
#[test]
fn deployed_assistant_resolves_via_by_name_in() {
    let tmp = tempfile::tempdir().unwrap();
    deploy_bundled_agents(tmp.path()).unwrap();
    let cfg = crate::agents::AgentConfig::by_name_in(&[tmp.path().to_path_buf()], "assistant")
        .expect("deployed assistant package must resolve");
    assert_eq!(cfg.agent.name, "assistant");
}

/// Why (#3556 — the exact gap that shipped): a deploy target whose stamp is
/// missing/mismatched must refresh whichever bundled files actually differ
/// from the current embedded template — the mechanism that lets a shipped
/// template change (e.g. a new tool grant) actually reach a machine that
/// already deployed an older copy.
/// Test: itself.
#[test]
fn stale_stamp_triggers_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    // Baseline deploy, as if performed by an older binary build.
    deploy_bundled_agents(tmp.path()).unwrap();

    // Simulate a stale on-disk `assistant/agent.toml` — e.g. deployed before
    // the `delegate_to_agent` grant existed on the bundled template.
    let assistant_toml = tmp.path().join("assistant").join("agent.toml");
    let stale_content = "[agent]\nname = \"assistant\"\nrole = \"assistant\"\n\n[tools]\nallow = [\"web_search\"]\n";
    std::fs::write(&assistant_toml, stale_content).unwrap();

    // A stamp that does not match the current binary's bundle content.
    stamp::write(tmp.path(), "deadbeef-not-the-real-stamp").unwrap();

    let report = ensure_bundled_agents_deployed_in(tmp.path()).unwrap();

    assert_eq!(report.written, 0, "every path already existed on disk");
    assert_eq!(
        report.refreshed, 1,
        "only the deliberately-staled assistant/agent.toml should refresh"
    );
    assert_eq!(report.backed_up, 1);

    let refreshed = std::fs::read_to_string(&assistant_toml).unwrap();
    assert_ne!(refreshed, stale_content, "stale content must be replaced");
    assert!(
        refreshed.contains("delegate_to_agent"),
        "refreshed assistant/agent.toml must match the CURRENT embedded \
         template, got: {refreshed}"
    );

    let new_stamp = stamp::read(tmp.path());
    assert_eq!(
        new_stamp.as_deref(),
        Some(current_bundle_stamp().unwrap().as_str()),
        "stamp must be updated to the current bundle content hash"
    );
}

/// Why: the refresh path must never silently discard a user's hand edit to a
/// bundled file — it archives the differing on-disk content to
/// `<file>.stale.bak` BEFORE overwriting, so it's always recoverable.
/// Test: itself.
#[test]
fn refresh_backs_up_differing_content() {
    let tmp = tempfile::tempdir().unwrap();
    deploy_bundled_agents(tmp.path()).unwrap();

    let assistant_toml = tmp.path().join("assistant").join("agent.toml");
    let hand_edited =
        "# hand-edited by a user, never committed upstream\n[agent]\nname = \"assistant\"\n";
    std::fs::write(&assistant_toml, hand_edited).unwrap();

    let report = force_reprovision_bundled_agents(tmp.path()).unwrap();
    assert_eq!(report.backed_up, 1);
    assert_eq!(report.refreshed, 1);

    let backup_path = tmp.path().join("assistant").join("agent.toml.stale.bak");
    let backup_content = std::fs::read_to_string(&backup_path).unwrap();
    assert_eq!(
        backup_content, hand_edited,
        "backup must preserve the pre-refresh content verbatim"
    );
}

/// Why (#3556 code-critic follow-up, HIGH): a `.stale.bak` is a one-shot
/// recovery copy — if a SECOND refresh pass (e.g. a later process recovering
/// from an earlier pass's crash, or simply another edit) finds a backup
/// already there, it must NOT overwrite it with whatever is on disk NOW.
/// Without this, a genuine hand-edit backed up by pass 1 could be silently
/// destroyed by pass 2 backing up torn or already-refreshed content over it.
/// Test: itself.
#[test]
fn existing_stale_backup_is_never_clobbered() {
    let tmp = tempfile::tempdir().unwrap();
    deploy_bundled_agents(tmp.path()).unwrap();

    let assistant_toml = tmp.path().join("assistant").join("agent.toml");
    let first_edit =
        "# first hand edit — this is the one that must survive\n[agent]\nname = \"assistant\"\n";
    std::fs::write(&assistant_toml, first_edit).unwrap();

    let first_pass = force_reprovision_bundled_agents(tmp.path()).unwrap();
    assert_eq!(first_pass.backed_up, 1);
    assert_eq!(first_pass.refreshed, 1);

    let backup_path = tmp.path().join("assistant").join("agent.toml.stale.bak");
    assert_eq!(std::fs::read_to_string(&backup_path).unwrap(), first_edit);

    // A second stale pass — standing in for a crash-recovering later process
    // or another hand edit — must not clobber the FIRST backup.
    let second_edit = "# a later pass's content — must NOT clobber the first backup\n";
    std::fs::write(&assistant_toml, second_edit).unwrap();

    let second_pass = force_reprovision_bundled_agents(tmp.path()).unwrap();
    assert_eq!(
        second_pass.backed_up, 0,
        "a backup already exists — a second pass must not overwrite it"
    );
    assert_eq!(second_pass.refreshed, 1, "dest itself is still refreshed");
    assert_eq!(
        std::fs::read_to_string(&backup_path).unwrap(),
        first_edit,
        "the ORIGINAL backup must survive untouched"
    );
}

/// Why: the refresh pass only ever iterates the embedded bundle's OWN rel
/// paths — a user-authored agent that isn't part of the bundle at all must
/// never be read, backed up, or written, even during a stale-stamp refresh.
/// Test: itself.
#[test]
fn non_bundled_user_file_untouched_by_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    deploy_bundled_agents(tmp.path()).unwrap();

    let user_file = tmp.path().join("my-custom-agent.toml");
    let user_content = "[agent]\nname = \"my-custom-agent\"\n";
    std::fs::write(&user_file, user_content).unwrap();

    // Force a stale-stamp refresh pass, as a real binary upgrade would.
    stamp::write(tmp.path(), "deadbeef").unwrap();
    ensure_bundled_agents_deployed_in(tmp.path()).unwrap();

    let after = std::fs::read_to_string(&user_file).unwrap();
    assert_eq!(
        after, user_content,
        "a non-bundled user file must never be touched by a refresh pass"
    );
    assert!(
        !tmp.path().join("my-custom-agent.toml.stale.bak").exists(),
        "no backup should be created for a file the bundle doesn't own"
    );
}

/// Why: a matching stamp is the steady-state, hot-path case (every process
/// startup after the first refresh) — it must do zero disk writes.
/// Test: itself.
#[test]
fn matching_stamp_is_a_fast_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let first = ensure_bundled_agents_deployed_in(tmp.path()).unwrap();
    assert!(
        first.total_touched() > 0,
        "first pass establishes the roster"
    );

    let second = ensure_bundled_agents_deployed_in(tmp.path()).unwrap();
    assert_eq!(
        second,
        ReprovisionReport::default(),
        "a matching stamp must be a true no-op"
    );
}

/// Why: a target directory deployed by the pre-#3556 code path (or by the
/// plain `deploy_bundled_agents` entry point) has no stamp file at all —
/// that must be treated as stale exactly once, establishing a baseline
/// stamp, WITHOUT rewriting files whose content already matches.
/// Test: itself.
#[test]
fn missing_stamp_establishes_baseline_without_rewriting_matching_content() {
    let tmp = tempfile::tempdir().unwrap();
    deploy_bundled_agents(tmp.path()).unwrap();
    assert_eq!(stamp::read(tmp.path()), None, "sanity: no stamp yet");

    let report = ensure_bundled_agents_deployed_in(tmp.path()).unwrap();
    assert_eq!(
        report,
        ReprovisionReport::default(),
        "on-disk content already matches the embedded template; nothing to touch"
    );
    assert!(
        stamp::read(tmp.path()).is_some(),
        "a missing stamp must be established on this pass"
    );
}

/// Why: `tagent agents repair` (the explicit manual escape hatch) must
/// force-refresh bundled files even when the stamp already matches — e.g. a
/// user suspects a deploy was interrupted and wants to be sure.
/// Test: itself.
#[test]
fn force_reprovision_overwrites_even_when_stamp_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let first = ensure_bundled_agents_deployed_in(tmp.path()).unwrap();
    assert!(first.total_touched() > 0);

    let assistant_toml = tmp.path().join("assistant").join("agent.toml");
    let edited = "# user edit, stamp untouched\n[agent]\nname = \"assistant\"\n";
    std::fs::write(&assistant_toml, edited).unwrap();

    // The automatic path would treat this as up to date (matching stamp)
    // and leave the edit alone.
    let noop = ensure_bundled_agents_deployed_in(tmp.path()).unwrap();
    assert_eq!(noop, ReprovisionReport::default());
    assert_eq!(std::fs::read_to_string(&assistant_toml).unwrap(), edited);

    // `force_reprovision_bundled_agents` bypasses the stamp check entirely.
    let repaired = force_reprovision_bundled_agents(tmp.path()).unwrap();
    assert_eq!(repaired.refreshed, 1);
    assert_eq!(repaired.backed_up, 1);
    let restored = std::fs::read_to_string(&assistant_toml).unwrap();
    assert_ne!(restored, edited, "repair must restore the current template");
}

/// Why (#3556 code-critic follow-up, MEDIUM): `ensure_bundled_agents_deployed_in`
/// must now treat `stamp::read` -> is-stale-decide -> refresh -> `stamp::write`
/// as ONE critical section under the single pass lock it acquires up front —
/// not just the refresh loop. Proves this by racing several threads' FULL
/// `ensure_bundled_agents_deployed_in` calls over the same seeded-stale
/// `target_dir`: full serialization means exactly ONE caller can possibly
/// observe the stale-before state and perform the actual refresh; every
/// other caller, however it interleaves, must see a fully-consistent
/// already-refreshed state (a true no-op report) — never a redundant
/// refresh, and never a torn/partial one.
/// Test: itself.
#[test]
fn concurrent_ensure_calls_over_stale_target_converge_to_one_consistent_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    deploy_bundled_agents(tmp.path()).unwrap();

    let assistant_toml = tmp.path().join("assistant").join("agent.toml");
    let stale_content = "[agent]\nname = \"assistant\"\nrole = \"assistant\"\n\n[tools]\nallow = [\"web_search\"]\n";
    std::fs::write(&assistant_toml, stale_content).unwrap();
    stamp::write(tmp.path(), "deadbeef-not-the-real-stamp").unwrap();

    let dir = std::sync::Arc::new(tmp.path().to_path_buf());
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let dir = std::sync::Arc::clone(&dir);
            std::thread::spawn(move || ensure_bundled_agents_deployed_in(&dir).unwrap())
        })
        .collect();
    let reports: Vec<ReprovisionReport> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let refreshing: Vec<_> = reports.iter().filter(|r| r.refreshed > 0).collect();
    assert_eq!(
        refreshing.len(),
        1,
        "exactly one concurrent caller must perform the actual refresh under \
         full serialization, got: {reports:?}"
    );
    assert_eq!(refreshing[0].refreshed, 1);
    assert_eq!(refreshing[0].backed_up, 1);
    for r in &reports {
        if r.refreshed == 0 {
            assert_eq!(
                *r,
                ReprovisionReport::default(),
                "a caller that lost the race must observe a true no-op \
                 (already-consistent state), not a partial/redundant \
                 refresh, got: {r:?}"
            );
        }
    }

    // Final on-disk state must be fully consistent: content reflects the
    // refresh AND the stamp reflects the current bundle — never one without
    // the other, which is exactly the invariant a lock spanning only the
    // refresh loop (not the stamp write) could violate.
    let final_content = std::fs::read_to_string(&assistant_toml).unwrap();
    assert!(
        final_content.contains("delegate_to_agent"),
        "final content must be the refreshed template, got: {final_content}"
    );
    assert_eq!(
        stamp::read(tmp.path()).as_deref(),
        Some(current_bundle_stamp().unwrap().as_str()),
        "final stamp must match the current bundle content hash"
    );
}

/// Why (#3556 code-critic follow-up, MEDIUM): the pass lock must guard
/// `stamp::read` itself, not just the refresh loop — proved by holding the
/// lock EXTERNALLY (standing in for another process mid-pass) and asserting
/// a concurrent `ensure_bundled_agents_deployed_in` call cannot complete
/// (i.e. cannot get past ITS OWN `lock::acquire`, which now happens BEFORE
/// `stamp::read`) until the external holder releases it.
/// Test: itself.
#[test]
fn ensure_deployed_blocks_on_externally_held_pass_lock() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let tmp = tempfile::tempdir().unwrap();
    deploy_bundled_agents(tmp.path()).unwrap();
    let dir = tmp.path().to_path_buf();

    // Simulate "another process is mid-pass right now" by holding the SAME
    // pass-level lock from this thread.
    let external_guard = lock::acquire(&dir).unwrap();

    let finished = Arc::new(AtomicBool::new(false));
    let finished2 = Arc::clone(&finished);
    let dir2 = dir.clone();
    let handle = std::thread::spawn(move || {
        let report = ensure_bundled_agents_deployed_in(&dir2).unwrap();
        finished2.store(true, Ordering::SeqCst);
        report
    });

    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(
        !finished.load(Ordering::SeqCst),
        "ensure_bundled_agents_deployed_in must block while another holder \
         has the pass-level lock, not proceed past stamp::read/write"
    );

    drop(external_guard);
    let report = handle.join().unwrap();
    assert!(
        finished.load(Ordering::SeqCst),
        "call must complete once the external lock is released"
    );
    // Content already matched the embedded template (only the stamp was
    // ever stale here, from `deploy_bundled_agents`'s no-stamp baseline), so
    // this establishes the baseline stamp without touching any file.
    assert_eq!(report, ReprovisionReport::default());
}
