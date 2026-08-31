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

/// Every archived backup of `dest`, found by NAME rather than by calling the
/// implementation's own path builder (#4461).
///
/// Why: a test that asks `stale_backup_path` where to look would agree with
/// the implementation by construction, including when the implementation is
/// wrong — the whole defect was a backup that silently never got written.
/// Scanning the directory for `<name>.stale.*.bak` also matches the legacy
/// fixed `<name>.stale.bak`, so these tests measure how many distinct
/// contents are recoverable, not what the files happen to be called.
/// What: reads `dest`'s parent directory and returns every sibling whose name
/// starts with `<dest file name>.stale.` and ends with `.bak`, sorted.
/// Test: used by `refresh_backs_up_differing_content`,
/// `existing_stale_backup_is_never_clobbered`,
/// `repeated_divergence_is_recoverable_across_reprovisions`,
/// `identical_content_reuses_one_backup_path`.
fn stale_backups_of(dest: &Path) -> Vec<PathBuf> {
    let dir = dest.parent().expect("dest sits inside the target dir");
    let name = dest
        .file_name()
        .and_then(|s| s.to_str())
        .expect("bundled paths are UTF-8");
    let prefix = format!("{name}.stale.");
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("the package directory exists after a deploy")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".bak"))
        })
        .collect();
    found.sort();
    found
}

/// The archived content of every backup of `dest`, in path order.
fn stale_backup_contents(dest: &Path) -> Vec<String> {
    stale_backups_of(dest)
        .iter()
        .map(|path| std::fs::read_to_string(path).expect("a backup must be readable"))
        .collect()
}

/// Every `*.bak` file anywhere under `root` (#4461).
///
/// Why: "an untouched file must not litter backups" is a claim about the
/// WHOLE deploy tree, not one package — a per-file check would miss a backup
/// written beside some other bundled agent.
/// What: walks `root` depth-first and collects every file whose name ends
/// with `.bak`.
/// Test: used by `first_run_writes_no_backups`,
/// `pristine_files_are_reprovisioned_without_littering_backups`.
fn all_backup_files(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut found = Vec::new();
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.ends_with(".bak"))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

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
        Some(current_bundle_stamp::<BundledAgents>().unwrap().as_str()),
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

    let backups = stale_backups_of(&assistant_toml);
    assert_eq!(backups.len(), 1, "exactly one archived copy: {backups:?}");
    let backup_content = std::fs::read_to_string(&backups[0]).unwrap();
    assert_eq!(
        backup_content, hand_edited,
        "backup must preserve the pre-refresh content verbatim"
    );
}

/// Why (#4461 — the defect this test would have caught): the backup used to
/// be written only when NO backup existed yet, so the first reprovision
/// preserved a hand-edit and every later one overwrote the file while
/// skipping the backup. Two edit-then-reprovision cycles lost the second
/// edit with no error, no warning, and no recoverable copy — which is how a
/// `cto-assistant` tool grant was destroyed on 2026-07-31.
/// What: edits a bundled file, reprovisions, edits it differently,
/// reprovisions again, then asserts BOTH edits are still readable from
/// archived backups. Against the pre-fix code the second pass reports
/// `backed_up == 0` and the second edit is unrecoverable.
/// Test: itself.
#[test]
fn repeated_divergence_is_recoverable_across_reprovisions() {
    let tmp = tempfile::tempdir().unwrap();
    deploy_bundled_agents(tmp.path()).unwrap();
    let assistant_toml = tmp.path().join("assistant").join("agent.toml");

    let first_edit = "# edit one — an owner-requested tool grant\n[agent]\nname = \"assistant\"\n";
    std::fs::write(&assistant_toml, first_edit).unwrap();
    let first_pass = force_reprovision_bundled_agents(tmp.path()).unwrap();
    assert_eq!(first_pass.backed_up, 1, "the first edit must be archived");

    let second_edit =
        "# edit two — made after the first reprovision\n[agent]\nname = \"assistant\"\n";
    std::fs::write(&assistant_toml, second_edit).unwrap();
    let second_pass = force_reprovision_bundled_agents(tmp.path()).unwrap();
    assert_eq!(
        second_pass.backed_up, 1,
        "the second edit is new content and must be archived too"
    );
    assert_eq!(second_pass.refreshed, 1);

    let archived = stale_backup_contents(&assistant_toml);
    assert!(
        archived.iter().any(|c| c == first_edit),
        "the first edit must still be recoverable: {archived:?}"
    );
    assert!(
        archived.iter().any(|c| c == second_edit),
        "the second edit must still be recoverable — this is the data loss \
         #4461 reported: {archived:?}"
    );
}

/// Why (#4461): naming a backup after its content is what bounds the set —
/// re-archiving bytes that are already archived must reuse the same path
/// rather than adding a generation. Without this, the fix would trade silent
/// data loss for an unbounded pile of files.
/// What: applies the SAME edit twice, with a reprovision after each, and
/// asserts exactly one backup exists holding that content.
/// Test: itself.
#[test]
fn identical_content_reuses_one_backup_path() {
    let tmp = tempfile::tempdir().unwrap();
    deploy_bundled_agents(tmp.path()).unwrap();
    let assistant_toml = tmp.path().join("assistant").join("agent.toml");

    let edit =
        "# the same hand edit, re-applied after each repair\n[agent]\nname = \"assistant\"\n";
    for _ in 0..2 {
        std::fs::write(&assistant_toml, edit).unwrap();
        force_reprovision_bundled_agents(tmp.path()).unwrap();
    }

    let backups = stale_backups_of(&assistant_toml);
    assert_eq!(
        backups.len(),
        1,
        "identical content must archive to one path, not one per pass: {backups:?}"
    );
    assert_eq!(std::fs::read_to_string(&backups[0]).unwrap(), edit);
}

/// Why (#4461): a reprovision over files the user never touched must leave no
/// backups at all — an operator who runs `tagent agents repair` routinely
/// should never accumulate `.bak` files for it.
/// What: deploys, then force-reprovisions twice with no edit in between, and
/// asserts no `*.bak` file exists anywhere under the target directory.
/// Test: itself.
#[test]
fn pristine_files_are_reprovisioned_without_littering_backups() {
    let tmp = tempfile::tempdir().unwrap();
    deploy_bundled_agents(tmp.path()).unwrap();

    for _ in 0..2 {
        let report = force_reprovision_bundled_agents(tmp.path()).unwrap();
        assert_eq!(report.backed_up, 0);
        assert_eq!(report.refreshed, 0, "on-disk content already matches");
    }

    let backups = all_backup_files(tmp.path());
    assert!(
        backups.is_empty(),
        "an untouched deploy must produce no backups: {backups:?}"
    );
}

/// Why (#4461): on a machine with no deployed roster yet there is nothing to
/// overwrite, so the backup path must not run at all.
/// What: points a stamp-aware deploy at an empty directory and asserts files
/// were written, none were backed up, and no `*.bak` exists.
/// Test: itself.
#[test]
fn first_run_writes_no_backups() {
    let tmp = tempfile::tempdir().unwrap();
    let report = ensure_bundled_agents_deployed_in(tmp.path()).unwrap();

    assert!(report.written > 0, "a first run establishes the roster");
    assert_eq!(report.backed_up, 0, "nothing existed to back up");
    let backups = all_backup_files(tmp.path());
    assert!(backups.is_empty(), "no backups on a first run: {backups:?}");
}

/// Why (#3556 code-critic follow-up, HIGH; naming reworked by #4461): a
/// second refresh pass — a later process recovering from an earlier pass's
/// crash, or simply another edit — must never overwrite an existing backup
/// with whatever is on disk NOW. #3556 bought that by refusing to write a
/// second backup at all, which is exactly what destroyed the second edit
/// (#4461); the digest-named path buys the same protection without the loss,
/// because torn or already-refreshed content resolves to its own path.
/// What: backs up one hand edit, then reprovisions again over DIFFERENT
/// content and asserts the first backup is still byte-identical.
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

    let first_backup = stale_backups_of(&assistant_toml)
        .pop()
        .expect("the first edit must be archived");
    assert_eq!(std::fs::read_to_string(&first_backup).unwrap(), first_edit);

    // A second stale pass — standing in for a crash-recovering later process
    // or another hand edit — must not clobber the FIRST backup.
    let second_edit = "# a later pass's content — must NOT clobber the first backup\n";
    std::fs::write(&assistant_toml, second_edit).unwrap();

    let second_pass = force_reprovision_bundled_agents(tmp.path()).unwrap();
    assert_eq!(second_pass.refreshed, 1, "dest itself is still refreshed");
    assert_eq!(
        std::fs::read_to_string(&first_backup).unwrap(),
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
    let backups = stale_backups_of(&user_file);
    assert!(
        backups.is_empty(),
        "no backup should be created for a file the bundle doesn't own: {backups:?}"
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
        Some(current_bundle_stamp::<BundledAgents>().unwrap().as_str()),
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

/// Every embedded seed spelling of a demo persona, as
/// `(asset_path, agent_name)`.
///
/// Why: each persona ships TWO embedded files — the directory PACKAGE
/// (`<name>/agent.toml`, the higher-ranked form `scan_agents_dir_tiered`
/// actually dispatches) and the flat SHADOW (`<name>.toml`). The shadow is
/// not dead weight: its own header documents it as the load-bearing
/// `extends`-shadow fallback, so the two must stay in sync. Asserting only
/// the package would let a future edit update one and silently leave the
/// other behind — which is precisely the drift these seeds are most exposed
/// to, since the reprovision path rewrites both. Driving the store/tool
/// assertions off ONE shared table is what makes that drift mechanically
/// impossible to miss (#3878 code-critic MEDIUM-1). The agent name is carried
/// explicitly rather than derived from the path because the two layouts spell
/// it differently (`izzie/agent.toml` vs `izzie.toml`).
const SEED_SPELLINGS: &[(&str, &str)] = &[
    ("izzie/agent.toml", "izzie"),
    ("izzie.toml", "izzie"),
    ("cto-assistant/agent.toml", "cto-assistant"),
    ("cto-assistant.toml", "cto-assistant"),
];

/// The OKG store binding each persona's seeds must declare, keyed by agent
/// name: `(agent_name, index, palace, tree)`.
const EXPECTED_BINDINGS: &[(&str, &str, &str, &str)] = &[
    ("izzie", "bob-kb", "owner-profile", "okg://izzie"),
    (
        "cto-assistant",
        "cto-assistant",
        "cto",
        "okg://cto-assistant",
    ),
];

/// #3816/#3864: every embedded seed spelling of each demo persona must carry
/// its OKG store binding.
///
/// Why: the bindings are what make `vector_search` route to the agent's own
/// corpus and what the GUI's OKG Stores card renders. They live in files that
/// the reprovision path rewrites (#3556, and the reprovision-clobber class of
/// bug generally), so a silent drop here would degrade both surfaces with no
/// compile error anywhere — exactly the failure mode #3864 documented. Covers
/// BOTH the package and the flat shadow (see [`SEED_SPELLINGS`]) so the two
/// cannot drift apart. Asserts against the EMBEDDED bytes (the provisioning
/// source of truth), not against `~/.trusty-agents`, which this crate must
/// never read in a test.
/// Test: itself.
#[test]
fn bundled_personas_carry_their_okg_store_bindings() {
    #[derive(serde::Deserialize)]
    struct Partial {
        #[serde(default)]
        stores: crate::stores::StoresConfig,
    }

    for (file, agent_name) in SEED_SPELLINGS {
        let (_, index, palace, tree) = EXPECTED_BINDINGS
            .iter()
            .find(|(name, ..)| name == agent_name)
            .unwrap_or_else(|| panic!("no expected binding declared for agent `{agent_name}`"));

        let raw =
            BundledAgents::get(file).unwrap_or_else(|| panic!("bundled asset missing: {file}"));
        let text = std::str::from_utf8(&raw.data).unwrap();
        let parsed: Partial =
            toml::from_str(text).unwrap_or_else(|e| panic!("{file} is not valid TOML: {e}"));
        let binding = parsed
            .stores
            .primary()
            .unwrap_or_else(|| panic!("{file} declares no [[stores]] binding"));
        assert_eq!(binding.resolved_index(), *index, "{file} index");
        assert_eq!(binding.palace.as_deref(), Some(*palace), "{file} palace");
        assert_eq!(binding.resolved_tree(agent_name), *tree, "{file} tree");
        assert!(
            parsed.stores.validate().is_empty(),
            "{file} binding has config issues: {:?}",
            parsed.stores.validate()
        );
    }
}

/// Both demo personas must actually be ABLE to call `vector_search` — a
/// binding the persona's `[tools].allow` doesn't grant is inert. Checked on
/// every seed spelling (package + flat shadow) for the same anti-drift reason
/// as [`bundled_personas_carry_their_okg_store_bindings`].
#[test]
fn bundled_personas_grant_vector_search() {
    #[derive(serde::Deserialize)]
    struct Partial {
        tools: Tools,
    }
    #[derive(serde::Deserialize)]
    struct Tools {
        #[serde(default)]
        allow: Vec<String>,
    }

    for (file, _agent_name) in SEED_SPELLINGS {
        let raw =
            BundledAgents::get(file).unwrap_or_else(|| panic!("bundled asset missing: {file}"));
        let text = std::str::from_utf8(&raw.data).unwrap();
        let parsed: Partial = toml::from_str(text).unwrap();
        assert!(
            parsed.tools.allow.iter().any(|t| t == "vector_search"),
            "{file} binds an OKG store but does not grant vector_search"
        );
    }
}

/// Why (#5227): making the workflow-mode pre-flight hierarchical only helps if
/// a workflow definition actually reaches a tier on that hierarchy. The
/// bundled `prescriptive.json` used to exist only inside a checkout of this
/// repo, so a `cargo install`ed binary (or the GUI sidecar, `cwd = /`) had
/// none anywhere. Pin that the workflows tree deploys through the same
/// stamp-aware path the agent roster uses, and that `prescriptive.json` is in
/// it — that file name is what `--workflow` defaults to.
/// Test: itself.
#[test]
fn bundled_workflows_deploy_to_target() {
    let tmp = tempfile::tempdir().unwrap();
    let report = ensure_embedded_deployed_in::<BundledWorkflows>(tmp.path()).unwrap();

    assert!(report.written > 0, "no bundled workflow files written");
    assert!(
        tmp.path().join("prescriptive.json").is_file(),
        "prescriptive.json missing from the deployed workflows tree"
    );

    // Stamp-aware: a second pass over an unchanged bundle writes nothing.
    let again = ensure_embedded_deployed_in::<BundledWorkflows>(tmp.path()).unwrap();
    assert_eq!(again.total_touched(), 0, "re-deploy was not a no-op");
}

/// Why (#5227): agents and workflows share the deploy machinery but must never
/// share a stamp — a workflows-tree deploy that wrote the agents stamp would
/// make the next agent refresh believe it was up to date.
/// Test: itself.
#[test]
fn agent_and_workflow_bundles_hash_differently() {
    assert_ne!(
        current_bundle_stamp::<BundledAgents>().unwrap(),
        current_bundle_stamp::<BundledWorkflows>().unwrap()
    );
}

/// A synthetic embed tree standing in for a source asset directory that
/// picked up stray local files before a build (#5226).
///
/// Why: `RustEmbed` embeds whatever physically sits in the folder at build
/// time and ignores `.gitignore`, so a `.env`, an editor backup, or one of
/// `state_writer::atomic_write`'s `<file>.lock` sidecars left in
/// `.trusty-agents/agents/` ships inside the distributed `tagent` binary and
/// is then written into every user's home. A committed fixture directory
/// cannot express that — `.env*` and `*.bak` are gitignored — so the file set
/// is stated in code instead.
/// What: a hand-written `RustEmbed` impl (no derive, no folder) over four
/// paths: one legitimate bundled agent and three strays.
/// Test: `stray_files_in_the_asset_tree_are_never_deployed`.
struct StrayAssetTree;

/// `(path, bytes)` pairs [`StrayAssetTree`] serves.
const STRAY_TREE: &[(&str, &[u8])] = &[
    ("assistant/agent.toml", b"[agent]\nname = \"assistant\"\n"),
    (".env", b"OPENROUTER_API_KEY=sk-or-should-never-ship\n"),
    // Deliberately NOT `assistant/agent.toml.lock`: `atomic_write` leaves its
    // own sidecar at exactly that path when it writes the bundled file, so the
    // stray has to name a file this fixture never deploys.
    ("ctrl.toml.lock", b"lock-sidecar\n"),
    ("assistant/persona.md.bak", b"editor backup\n"),
];

impl rust_embed::RustEmbed for StrayAssetTree {
    fn get(file_path: &str) -> Option<rust_embed::EmbeddedFile> {
        STRAY_TREE
            .iter()
            .find(|(path, _)| *path == file_path)
            .map(|(_, bytes)| rust_embed::EmbeddedFile {
                data: std::borrow::Cow::Borrowed(bytes),
                // `Metadata` has no public constructor; this doc-hidden one is
                // what the derive macro itself emits.
                metadata: rust_embed::Metadata::__rust_embed_new([0u8; 32], None, None),
            })
    }

    fn iter() -> impl Iterator<Item = std::borrow::Cow<'static, str>> + 'static {
        STRAY_TREE
            .iter()
            .map(|(path, _)| std::borrow::Cow::Borrowed(*path))
    }
}

/// Why (#5226): the deploy loop wrote every path the embed tree offered, so a
/// stray file that reached the build reached the user's `$HOME` too — a `.env`
/// among them. The filter is what stops a non-bundled path from ever being
/// materialised.
/// What: deploys [`StrayAssetTree`] into a tempdir and asserts the legitimate
/// agent file landed while none of the three strays did, and that the report
/// counts only the file actually written.
/// Test: itself.
#[test]
fn stray_files_in_the_asset_tree_are_never_deployed() {
    let tmp = tempfile::tempdir().unwrap();
    let guard = lock::acquire(tmp.path()).unwrap();
    let report = reprovision_embedded_locked::<StrayAssetTree>(&guard, tmp.path(), false).unwrap();

    assert!(
        tmp.path().join("assistant/agent.toml").is_file(),
        "the legitimate bundled agent must still deploy"
    );
    for stray in [".env", "ctrl.toml.lock", "assistant/persona.md.bak"] {
        assert!(
            !tmp.path().join(stray).exists(),
            "{stray} must never be materialised under the deploy target"
        );
    }
    assert_eq!(report.written, 1, "only the bundled file counts as written");
}

/// Why (#5226): the allowlist is the whole defence, so its edges are pinned
/// directly — a dotfile, a lock sidecar, an editor backup and a dotted
/// directory component must all be rejected, and the three extensions the
/// bundle actually uses must all be accepted at any depth.
/// Test: itself.
#[test]
fn bundled_asset_predicate_fails_closed() {
    for accepted in [
        "pm.toml",
        "assistant/agent.toml",
        "assistant/persona.md",
        "izzie/events/gmail.md",
        "prescriptive.json",
    ] {
        assert!(is_bundled_asset(accepted), "{accepted} must be bundled");
    }
    for rejected in [
        ".env",
        ".env.local",
        "assistant/.env",
        "assistant/agent.toml.lock",
        "assistant/persona.md.bak",
        ".DS_Store",
        ".gitignore",
        "assistant/agent.toml.stale.abc123.bak",
        "notes",
        "script.sh",
    ] {
        assert!(!is_bundled_asset(rejected), "{rejected} must be rejected");
    }
}

/// Every file under `dir`, as a `/`-joined path relative to it.
fn source_tree_paths(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if entry.path().is_dir() {
                walk(&entry.path(), &rel, out);
            } else {
                out.push(rel);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out);
    out.sort();
    out
}

/// Why (#5226): the `#[include]` globs are the build-time half of the filter.
/// Nothing that fails [`is_bundled_asset`] may reach the embedded set — and the
/// globs must not have quietly dropped part of the real roster while excluding
/// strays, which an anchor-file spot check would miss.
/// What: compares each embedded set against the source tree it is built from,
/// filtered by the same predicate — so the assertion states the whole expected
/// file list without hard-coding one that rots on the next roster change. A
/// stray file a developer happens to have locally is filtered out of BOTH
/// sides, so it cannot fail this test spuriously.
/// Test: itself.
#[test]
fn embedded_trees_carry_only_bundled_assets() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join(".trusty-agents");

    for (tree, embedded) in [
        ("agents", {
            let mut v: Vec<String> = BundledAgents::iter().map(|r| r.to_string()).collect();
            v.sort();
            v
        }),
        ("workflows", {
            let mut v: Vec<String> = BundledWorkflows::iter().map(|r| r.to_string()).collect();
            v.sort();
            v
        }),
    ] {
        let expected: Vec<String> = source_tree_paths(&source_root.join(tree))
            .into_iter()
            .filter(|rel| is_bundled_asset(rel))
            .collect();
        assert!(!expected.is_empty(), "{tree}: no source assets found");
        assert_eq!(
            embedded, expected,
            "{tree}: the embedded set does not match the bundled source files"
        );
    }
}

/// Why (#5226): the stamp and the write loop must read the SAME bundle. A
/// stray file that moved the stamp but was never written would leave every
/// startup seeing a stale stamp and refreshing forever.
/// What: hashes the stray tree and asserts the digest equals the one its single
/// bundled file alone produces.
/// Test: itself.
#[test]
fn stray_files_do_not_move_the_bundle_stamp() {
    let bundled_only = stamp::compute(vec![(
        "assistant/agent.toml".to_string(),
        b"[agent]\nname = \"assistant\"\n".to_vec(),
    )]);
    assert_eq!(
        current_bundle_stamp::<StrayAssetTree>().unwrap(),
        bundled_only,
        "the stamp must hash the bundled file set, not the raw embed tree"
    );
}
