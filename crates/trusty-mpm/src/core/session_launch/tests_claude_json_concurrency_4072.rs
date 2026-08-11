//! Issue #4072 regression coverage: concurrent `~/.claude.json` trust seeds
//! must MERGE, never clobber each other.
//!
//! Why: [`super::settings::preseed_workspace_trust`] and
//! [`crate::core::home_trust_seed::preseed_home_trust`] both perform an
//! unsynchronised load → mutate → store cycle over the SAME `~/.claude.json`,
//! and the daemon runs them concurrently — the workspace provisioner calls the
//! latter and then, through `prepare_session`, the former, once per session,
//! from independent tokio tasks. Overlapping cycles are a lost update: the
//! slower writer serialises the snapshot it read BEFORE the faster writer's
//! store, silently deleting that session's whole `projects.<workspace>` entry
//! (trust acceptance AND its `enabledMcpjsonServers` approval) with nothing
//! logged — both writes "succeeded". The operator then hits the very startup
//! dialog the seed exists to dismiss. The same race is what made
//! `tests_manifest_toggle_trust_3934::prepare_session_excludes_spoofed_trusty_search_when_injector_disabled`
//! a recurring CI flake (red on PRs #4057 and #4067, green in isolation): the
//! lib test binary runs `#[serial]` tests that redirect the process-global
//! `$HOME` to a temp dir alongside non-serial tests that resolve `$HOME` at
//! call time and drive the real provisioner, so a foreign test's seed landed
//! in — and clobbered — the serial test's temp `.claude.json`.
//!
//! These tests drive the REAL seeders against ONE file from multiple threads,
//! which is the only way to prove the fix: a single-threaded unit test of
//! either seeder passes both before and after `core::claude_json_guard`
//! exists.
//!
//! Why a dedicated file: `settings.rs` sits at 485/500 production SLOC and
//! `tests.rs` at 1482/1500 test SLOC, so this coverage goes in its own
//! `tests_*.rs` module — the split pattern `tests_roster.rs` /
//! `tests_launch_trust_3926.rs` / `tests_manifest_toggle_trust_3934.rs`
//! already establish here.
//!
//! What:
//! - `concurrent_seeds_preserve_every_workspace_entry`: N threads seeding N
//!   distinct workspaces into one `.claude.json` must leave ALL N entries
//!   present, each with its own `enabledMcpjsonServers`.
//! - `concurrent_home_and_workspace_seeds_preserve_both_entries`: the exact
//!   daemon pairing — `preseed_home_trust` racing
//!   `preseed_workspace_trust` — must leave both entries intact.
//!
//! Test: this is the test module.

use super::settings::preseed_workspace_trust;
use super::tests::EnvVarGuard;
use crate::core::home_trust_seed::preseed_home_trust;
use std::collections::BTreeSet;
use std::path::Path;
use tempfile::tempdir;

/// How many writer threads the concurrency tests drive.
///
/// Why: the lost update only manifests when two cycles actually interleave.
/// Eight threads each performing 40 read-modify-write cycles over one file
/// reproduced the pre-fix failure on every observed run (both macOS and
/// ubuntu CI) while keeping the test well under a second.
const WRITERS: usize = 8;

/// Read-modify-write cycles each writer thread performs.
///
/// Why every cycle seeds a DIFFERENT workspace key: re-seeding one key in a
/// loop makes a lost update self-healing — the next round re-adds the entry
/// the clobber dropped, so the final state converges and the bug hides. In
/// production each entry is written once (one workspace, one session), so a
/// clobber is permanent. Distinct keys per round reproduce that.
const ROUNDS: usize = 40;

/// Read back the `projects` keys recorded in `claude_json`.
fn project_keys(claude_json: &Path) -> BTreeSet<String> {
    let text = std::fs::read_to_string(claude_json).expect("the seeded file must exist");
    let value: serde_json::Value =
        serde_json::from_str(&text).expect("a seeded .claude.json must always be valid JSON");
    value["projects"]
        .as_object()
        .expect("projects is an object")
        .keys()
        .cloned()
        .collect()
}

#[test]
fn concurrent_seeds_preserve_every_workspace_entry() {
    // THE RACE (issue #4072): every seed targets a DISTINCT workspace key, so
    // no write is a no-op idempotent skip and no later round can re-add what
    // an earlier clobber dropped — each one is a genuine, one-shot load →
    // mutate → store cycle over the shared file, exactly as production does
    // it (one entry per provisioned session). A merge-preserving seeder ends
    // with every key present; the pre-fix seeder ends with only the survivors
    // of the interleavings.
    let tmp = tempdir().unwrap();
    let claude_json = tmp.path().join(".claude.json");

    let expected: BTreeSet<String> = (0..WRITERS)
        .flat_map(|writer| (0..ROUNDS).map(move |round| format!("/workspace/w{writer}-s{round}")))
        .collect();

    std::thread::scope(|scope| {
        for writer in 0..WRITERS {
            let claude_json = claude_json.clone();
            scope.spawn(move || {
                for round in 0..ROUNDS {
                    let workspace = format!("/workspace/w{writer}-s{round}");
                    preseed_workspace_trust(&claude_json, Path::new(&workspace))
                        .expect("seeding a workspace must never fail");
                }
            });
        }
    });

    let actual = project_keys(&claude_json);
    let lost: Vec<&String> = expected.difference(&actual).collect();
    assert!(
        lost.is_empty(),
        "concurrent trust seeds lost {} of {} workspace entries — a read-modify-write \
         cycle stored a snapshot taken before a sibling's store (issue #4072). Lost: {lost:?}",
        lost.len(),
        expected.len()
    );

    // Every surviving entry must carry its full payload, not just the key.
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
    for workspace in &expected {
        let entry = &value["projects"][workspace];
        assert_eq!(
            entry["hasTrustDialogAccepted"],
            serde_json::json!(true),
            "{workspace} lost its trust acceptance"
        );
        // // #4181: `enabledMcpjsonServers` is no longer written, so the
        // payload a lost-update would destroy is the onboarding pair. The
        // concurrency defect this file exists for is unchanged — two writers
        // load-mutate-storing the same file — only what they write is.
        assert_eq!(
            entry["hasCompletedProjectOnboarding"],
            serde_json::json!(true),
            "{workspace} lost its onboarding acceptance"
        );
    }
}

#[test]
#[serial_test::serial]
fn concurrent_home_and_workspace_seeds_preserve_both_entries() {
    // The exact pairing the daemon performs per provisioned session:
    // `provisioner::workspace` calls `preseed_home_trust`, then
    // `prepare_session` calls `preseed_workspace_trust` — two DIFFERENT
    // seeders, same `~/.claude.json`. Serial because `preseed_home_trust`
    // resolves the process-global `$HOME`.
    let home = tempdir().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", home.path());
    let claude_json = home.path().join(".claude.json");

    // Distinct key per round, same reason as the sibling test: a re-seeded key
    // self-heals a clobber and hides the bug.
    let expected: BTreeSet<String> = (0..ROUNDS)
        .flat_map(|round| {
            [
                format!("/workspace/home-seeded-{round}"),
                format!("/workspace/launch-seeded-{round}"),
            ]
        })
        .collect();

    std::thread::scope(|scope| {
        scope.spawn(|| {
            for round in 0..ROUNDS {
                let workspace = format!("/workspace/home-seeded-{round}");
                preseed_home_trust(Path::new(&workspace)).expect("home trust seed must not fail");
            }
        });
        scope.spawn(|| {
            for round in 0..ROUNDS {
                let workspace = format!("/workspace/launch-seeded-{round}");
                preseed_workspace_trust(&claude_json, Path::new(&workspace))
                    .expect("workspace trust seed must not fail");
            }
        });
    });

    let actual = project_keys(&claude_json);
    let lost: Vec<&String> = expected.difference(&actual).collect();
    assert!(
        lost.is_empty(),
        "the two seeders clobbered {} of {} entries when run concurrently against one \
         ~/.claude.json (issue #4072). Lost: {lost:?}",
        lost.len(),
        expected.len()
    );
}
