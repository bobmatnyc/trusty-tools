//! Unit tests for `standalone::trust_seed` — split out to keep the production
//! module under the 500-SLOC cap (mirrors the `native_mcp_tests.rs` /
//! `mcp_config_tests.rs` split pattern already used in this crate).
//! What: coverage for [`super::preseed_managed_trust`] — trust-dialog
//! marking, `enabledMcpjsonServers` seeding, idempotency, malformed-JSON
//! quarantine, the isolation invariant (never writes `$HOME/.claude*`), the
//! WI-8/#3918 builtin-name inclusion, and the issue #3934 regression: an
//! untrusted manifest disabling the `trusty-memory`/`trusty-search` injector
//! combined with a spoofed `.mcp.json` entry must not leave the name
//! pre-approved, while a legitimate operator toggle still launches cleanly.
//! Test: this file IS the test module.

use super::*;
use tempfile::TempDir;

/// Every timestamped quarantine sibling currently sitting in `cfg`.
///
/// Why (issue #4206): the quarantine name is no longer the fixed
/// `.claude.json.corrupt`, so tests must match the
/// `.claude.json.corrupt-<timestamp>` family by prefix. Centralising the match
/// keeps the "how many quarantine events happened" question answerable in one
/// place.
/// What: file names under `cfg` starting with `.claude.json.corrupt`, sorted
/// for deterministic assertion messages.
fn quarantine_files(cfg: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(cfg)
        .expect("config dir must be readable")
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|n| n.starts_with(".claude.json.corrupt"))
        .collect();
    found.sort();
    found
}

/// Build a `.claude.json` whose `projects` map is exactly `entries`.
///
/// Why: the prune tests (issue #4206) all need a pre-populated config with
/// hand-crafted project entries; inlining the JSON assembly in each obscured
/// what each test was actually varying.
/// What: writes `{"projects": {<entries>}}` to `<cfg>/.claude.json`.
fn write_config_with_projects(cfg: &Path, entries: serde_json::Value) {
    let config = serde_json::json!({ "projects": entries });
    std::fs::write(
        cfg.join(".claude.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
}

/// Read back `projects` from `<cfg>/.claude.json`.
fn read_projects(cfg: &Path) -> serde_json::Map<String, serde_json::Value> {
    let text = std::fs::read_to_string(cfg.join(".claude.json")).unwrap();
    let val: serde_json::Value = serde_json::from_str(&text).unwrap();
    val["projects"].as_object().cloned().unwrap_or_default()
}

/// A `projects` entry carrying only the four keys the seeder itself writes —
/// i.e. what a leaked test-fixture entry looks like on disk.
fn seeder_only_entry() -> serde_json::Value {
    serde_json::json!({
        "hasTrustDialogAccepted": true,
        "hasCompletedProjectOnboarding": true,
        "projectOnboardingSeenCount": 1,
        "enabledMcpjsonServers": ["trusty-mpm"],
    })
}

// WI-3 TRUST-SEED: preseed_managed_trust must write trust keys for the workspace.
#[test]
fn test_preseed_managed_trust_marks_directory() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("claude-config");
    std::fs::create_dir_all(&cfg).unwrap();
    let workspace = tmp.path().join("projects").join("my-repo").join("repo");
    std::fs::create_dir_all(&workspace).unwrap();

    preseed_managed_trust(&cfg, &workspace).unwrap();

    let text = std::fs::read_to_string(cfg.join(".claude.json")).unwrap();
    let val: serde_json::Value = serde_json::from_str(&text).unwrap();

    let key = workspace.to_string_lossy().to_string();
    let proj = val["projects"][&key].as_object().unwrap();
    assert_eq!(
        proj.get("hasTrustDialogAccepted"),
        Some(&serde_json::Value::Bool(true)),
        "hasTrustDialogAccepted must be true"
    );
    assert_eq!(
        proj.get("hasCompletedProjectOnboarding"),
        Some(&serde_json::Value::Bool(true)),
        "hasCompletedProjectOnboarding must be true"
    );
    assert!(
        proj.get("projectOnboardingSeenCount")
            .and_then(|v| v.as_i64())
            .is_some_and(|n| n >= 1),
        "projectOnboardingSeenCount must be >= 1"
    );
}

// WI-3 TRUST-SEED idempotency: calling preseed_managed_trust twice must not
// corrupt the file or duplicate entries.
#[test]
fn test_preseed_managed_trust_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("claude-config");
    std::fs::create_dir_all(&cfg).unwrap();
    let workspace = tmp.path().join("my-repo");
    std::fs::create_dir_all(&workspace).unwrap();

    preseed_managed_trust(&cfg, &workspace).unwrap();
    let after_first = std::fs::read_to_string(cfg.join(".claude.json")).unwrap();

    preseed_managed_trust(&cfg, &workspace).unwrap();
    let after_second = std::fs::read_to_string(cfg.join(".claude.json")).unwrap();

    assert_eq!(
        after_first, after_second,
        "preseed_managed_trust must be idempotent: two calls must produce identical output"
    );
}

// WI-3 TRUST-SEED: existing keys must be preserved (trust seed must not
// clobber unrelated data already in the config file).
#[test]
fn test_preseed_managed_trust_preserves_other_keys() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("claude-config");
    std::fs::create_dir_all(&cfg).unwrap();
    let workspace = tmp.path().join("repo");
    std::fs::create_dir_all(&workspace).unwrap();

    // Pre-populate .claude.json with unrelated data.
    std::fs::write(
        cfg.join(".claude.json"),
        r#"{"someOAuthToken":"keep-me","otherField":99}"#,
    )
    .unwrap();

    preseed_managed_trust(&cfg, &workspace).unwrap();

    let text = std::fs::read_to_string(cfg.join(".claude.json")).unwrap();
    let val: serde_json::Value = serde_json::from_str(&text).unwrap();

    assert_eq!(
        val.get("someOAuthToken").and_then(|v| v.as_str()),
        Some("keep-me"),
        "someOAuthToken must be preserved after trust seed"
    );
    assert_eq!(
        val.get("otherField").and_then(|v| v.as_i64()),
        Some(99),
        "otherField must be preserved after trust seed"
    );
}

// WI-3 ISOLATION: preseed_managed_trust must NEVER write anything outside
// `<claude_config_dir>`.
//
// This test uses two complementary strategies:
//
// 1. Sentinel-root check: `cfg` is a subdirectory of `root`; we assert
//    nothing lands at the `root` level (outside `cfg`). This proves the
//    function only writes through the `claude_config_dir` argument.
//
// 2. Fake-HOME redirect (serial_test): we redirect `HOME` to a second
//    empty temp dir and assert it remains empty after the call. Because
//    the test is serialised with `#[serial]`, the env mutation is sound —
//    parallel Rust test threads cannot observe the changed `HOME`.
//
// Together the two checks cover both "writes through argument" and
// "never writes through $HOME" without unsound parallel env mutation.
#[serial_test::serial]
#[test]
fn test_preseed_managed_trust_no_home_write() {
    /// RAII guard that restores $HOME to its original value on drop (including panic).
    struct HomeGuard(Option<String>);
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.0 {
                Some(ref p) => unsafe { std::env::set_var("HOME", p) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }
    }

    // Strategy 1: sentinel-root guard.
    let root = TempDir::new().unwrap();
    let cfg = root.path().join("claude-config");
    std::fs::create_dir_all(&cfg).unwrap();
    let workspace = root.path().join("repo");
    std::fs::create_dir_all(&workspace).unwrap();

    // Strategy 2: redirect $HOME to an empty temp dir; assert it stays empty.
    let fake_home = TempDir::new().unwrap();
    // SAFETY: test is serial (#[serial_test::serial]), so no other test thread
    // reads HOME concurrently during this function body. The HomeGuard restores
    // HOME even if an assertion below panics.
    let _home_guard = {
        let prev = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", fake_home.path()) };
        HomeGuard(prev)
    };

    preseed_managed_trust(&cfg, &workspace).unwrap();

    // --- Strategy 1 assertions (sentinel-root) ---

    // The expected output file must exist inside cfg.
    assert!(
        cfg.join(".claude.json").exists(),
        "preseed_managed_trust must write .claude.json inside claude_config_dir"
    );

    // No .claude.json should exist directly under root (outside cfg).
    assert!(
        !root.path().join(".claude.json").exists(),
        "preseed_managed_trust must NOT write .claude.json outside claude_config_dir \
         (isolation invariant)"
    );

    // No .claude directory should exist directly under root (outside cfg).
    assert!(
        !root.path().join(".claude").exists(),
        "preseed_managed_trust must NOT create .claude/ outside claude_config_dir \
         (isolation invariant)"
    );

    // --- Strategy 2 assertions (fake-HOME stays empty) ---

    // The fake home directory must contain no .claude.json and no .claude/
    // sub-directory (the two locations Claude Code reads global config from).
    assert!(
        !fake_home.path().join(".claude.json").exists(),
        "preseed_managed_trust must NOT write .claude.json to $HOME \
         (isolation invariant)"
    );
    assert!(
        !fake_home.path().join(".claude").exists(),
        "preseed_managed_trust must NOT create $HOME/.claude/ \
         (isolation invariant)"
    );

    // _home_guard drops here and restores HOME.
}

// WI-3 TRUST-SEED robustness: a pre-existing malformed .claude.json must be
// quarantined (renamed to .claude.json.corrupt) and seeding must proceed from
// a fresh `{}` — so the managed session never gets stuck in a permanent
// "trust dialog" loop because of a corrupt file.
#[test]
fn test_preseed_managed_trust_quarantines_malformed_json() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("claude-config");
    std::fs::create_dir_all(&cfg).unwrap();
    let workspace = tmp.path().join("repo");
    std::fs::create_dir_all(&workspace).unwrap();

    // Write deliberately malformed JSON into .claude.json.
    std::fs::write(cfg.join(".claude.json"), b"{ this is not valid json !!!").unwrap();

    // preseed_managed_trust must succeed (no error).
    preseed_managed_trust(&cfg, &workspace).unwrap();

    // The quarantine sibling must exist (corrupt file was renamed, not deleted).
    // Issue #4206: the name is now TIMESTAMPED (`.claude.json.corrupt-<stamp>`)
    // rather than the fixed `.claude.json.corrupt`, so match by prefix.
    let quarantined = quarantine_files(&cfg);
    assert_eq!(
        quarantined.len(),
        1,
        "exactly one timestamped quarantine file must exist after malformed-JSON \
         quarantine, found: {quarantined:?}"
    );

    // The new .claude.json must be valid JSON containing the seeded trust keys.
    let text = std::fs::read_to_string(cfg.join(".claude.json"))
        .expect(".claude.json must exist after quarantine + fresh seed");
    let val: serde_json::Value =
        serde_json::from_str(&text).expect(".claude.json must be valid JSON after fresh seed");

    let key = workspace.to_string_lossy().to_string();
    let proj = val["projects"][&key]
        .as_object()
        .expect("projects.<workspace> must be an object");
    assert_eq!(
        proj.get("hasTrustDialogAccepted"),
        Some(&serde_json::Value::Bool(true)),
        "hasTrustDialogAccepted must be true after quarantine + fresh seed"
    );
    assert_eq!(
        proj.get("hasCompletedProjectOnboarding"),
        Some(&serde_json::Value::Bool(true)),
        "hasCompletedProjectOnboarding must be true after quarantine + fresh seed"
    );
}

// ─── issue #4206: bounded growth (the prune) ──────────────────────────────

// Issue #4206 TEST 2 — a `projects` entry whose directory is DEFINITIVELY
// absent (a plain ENOENT: the path simply does not exist) and which carries
// nothing but the four keys this seeder writes is a leaked test-fixture
// dropping. It must be removed during the read-modify-write that is already
// happening, so the file can shrink instead of only ever growing. Before this
// fix `preseed_managed_trust` had no removal path at all and the reporting
// operator's config had reached 2,541 entries, 2,445 of them tempdir paths.
#[test]
fn prune_drops_entry_when_directory_definitively_absent() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("claude-config");
    std::fs::create_dir_all(&cfg).unwrap();
    let workspace = tmp.path().join("live-repo");
    std::fs::create_dir_all(&workspace).unwrap();

    // A path that has never existed and whose parent DOES exist, so the OS
    // reports a clean ENOENT rather than anything ambiguous.
    let vanished = tmp.path().join("vanished-fixture-dir");
    assert!(!vanished.exists(), "precondition: the path must not exist");

    write_config_with_projects(
        &cfg,
        serde_json::json!({
            vanished.to_string_lossy(): seeder_only_entry(),
            workspace.to_string_lossy(): seeder_only_entry(),
        }),
    );

    preseed_managed_trust(&cfg, &workspace).unwrap();

    let projects = read_projects(&cfg);
    assert!(
        !projects.contains_key(vanished.to_string_lossy().as_ref()),
        "a pure-seeder entry for a definitively-absent directory must be pruned: {:?}",
        projects.keys().collect::<Vec<_>>()
    );
    assert!(
        projects.contains_key(workspace.to_string_lossy().as_ref()),
        "the live workspace entry must survive the prune"
    );
}

// Issue #4206 TEST 3 — THE ANTI-OVER-DELETION TEST, and the one that matters
// most. An entry whose path cannot be resolved for an AMBIGUOUS reason must be
// KEPT. "The directory did not stat" is NOT the same claim as "the directory
// does not exist": an unmounted volume, a down network mount, a permission
// error, or an I/O fault all fail to stat while the user's real project is
// perfectly intact behind them. Deleting on that signal would silently destroy
// live trust state. This project shipped two bugs in the opposite direction
// (an ambiguous observation treated as a definite negative) on the same day
// this fix was written, so the prune treats ONLY a clean `NotFound` as absent.
//
// The ambiguity here is produced by ENOTDIR — a regular FILE occupying what
// the entry key uses as a parent directory, so `symlink_metadata` on the child
// fails with a non-`NotFound` error. Chosen over a chmod-000 directory because
// it reproduces identically whether or not the test runs as root, so the
// assertion can never silently degrade into a vacuous pass in a container.
#[test]
fn prune_keeps_entry_when_path_error_is_ambiguous() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("claude-config");
    std::fs::create_dir_all(&cfg).unwrap();
    let workspace = tmp.path().join("live-repo");
    std::fs::create_dir_all(&workspace).unwrap();

    // A regular file standing where a directory would have to be.
    let blocker = tmp.path().join("not-a-directory");
    std::fs::write(&blocker, b"regular file").unwrap();
    let unreachable = blocker.join("project");

    // Assert the precondition this test depends on: the error really is
    // ambiguous, NOT NotFound. Without this the test could pass for the wrong
    // reason on a platform that reports ENOENT here.
    let err =
        std::fs::symlink_metadata(&unreachable).expect_err("stat through a regular file must fail");
    assert_ne!(
        err.kind(),
        std::io::ErrorKind::NotFound,
        "precondition: this path must fail AMBIGUOUSLY, not with NotFound — \
         otherwise this test is not exercising the over-deletion guard (got {err:?})"
    );

    write_config_with_projects(
        &cfg,
        serde_json::json!({
            unreachable.to_string_lossy(): seeder_only_entry(),
            workspace.to_string_lossy(): seeder_only_entry(),
        }),
    );

    preseed_managed_trust(&cfg, &workspace).unwrap();

    let projects = read_projects(&cfg);
    assert!(
        projects.contains_key(unreachable.to_string_lossy().as_ref()),
        "an entry whose path is unreachable for an AMBIGUOUS reason must be KEPT — \
         only a definitive NotFound may prune: {:?}",
        projects.keys().collect::<Vec<_>>()
    );
}

// Issue #4206 TEST 4 — an entry carrying Claude Code's OWN runtime state
// (`lastSessionId`, `lastCost`, `mcpServers`, `history`, …) represents real
// user work, not a seeder dropping, and must survive even when its directory
// is definitively gone. A user whose project moved or whose volume is detached
// must not silently lose their session history. Only 21 of the reporting
// operator's 2,541 entries carried such fields — they are precisely the ones
// worth protecting.
#[test]
fn prune_keeps_entry_with_runtime_fields() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("claude-config");
    std::fs::create_dir_all(&cfg).unwrap();
    let workspace = tmp.path().join("live-repo");
    std::fs::create_dir_all(&workspace).unwrap();

    let gone = tmp.path().join("moved-away-project");
    assert!(!gone.exists(), "precondition: the path must not exist");

    write_config_with_projects(
        &cfg,
        serde_json::json!({
            gone.to_string_lossy(): {
                "hasTrustDialogAccepted": true,
                "hasCompletedProjectOnboarding": true,
                "projectOnboardingSeenCount": 3,
                "enabledMcpjsonServers": ["trusty-mpm"],
                // The distinguishing signal: real Claude Code runtime state.
                "lastSessionId": "abc-123",
                "lastCost": 4.2,
                "mcpServers": { "custom": { "command": "x" } },
            },
            workspace.to_string_lossy(): seeder_only_entry(),
        }),
    );

    preseed_managed_trust(&cfg, &workspace).unwrap();

    let projects = read_projects(&cfg);
    let kept = projects
        .get(gone.to_string_lossy().as_ref())
        .expect("an entry with Claude Code runtime fields must be KEPT even when its path is gone");
    assert_eq!(
        kept.get("lastSessionId").and_then(|v| v.as_str()),
        Some("abc-123"),
        "the preserved entry must keep its runtime state intact, not just its key"
    );
    assert!(
        kept.get("mcpServers").is_some(),
        "mcpServers must survive the prune"
    );
}

// ─── issue #4206: legible quarantine ──────────────────────────────────────

// Issue #4206 TEST 5 — two quarantine events must produce TWO distinct files.
// Both writers used the fixed name `.claude.json.corrupt`, so a second
// corruption silently overwrote the first one's bytes — erasing the only
// record of the first failure in a file that also holds OAuth state. A
// post-mortem could tell that corruption had happened at least once, and
// nothing more.
#[test]
fn two_quarantine_events_produce_two_distinct_files() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("claude-config");
    std::fs::create_dir_all(&cfg).unwrap();
    let workspace = tmp.path().join("repo");
    std::fs::create_dir_all(&workspace).unwrap();

    // First corruption + quarantine.
    std::fs::write(cfg.join(".claude.json"), b"{ first corruption !!!").unwrap();
    preseed_managed_trust(&cfg, &workspace).unwrap();
    assert_eq!(
        quarantine_files(&cfg).len(),
        1,
        "first quarantine must produce exactly one file"
    );

    // Second, independent corruption + quarantine.
    std::fs::write(cfg.join(".claude.json"), b"{ second corruption ???").unwrap();
    preseed_managed_trust(&cfg, &workspace).unwrap();

    let files = quarantine_files(&cfg);
    assert_eq!(
        files.len(),
        2,
        "two quarantine events must leave TWO distinct records, not overwrite one: {files:?}"
    );

    // And both sets of original bytes must still be recoverable — the whole
    // point of quarantining rather than deleting.
    let bodies: Vec<String> = files
        .iter()
        .map(|f| std::fs::read_to_string(cfg.join(f)).unwrap())
        .collect();
    assert!(
        bodies.iter().any(|b| b.contains("first corruption")),
        "the FIRST failure's bytes must survive the second quarantine: {bodies:?}"
    );
    assert!(
        bodies.iter().any(|b| b.contains("second corruption")),
        "the second failure's bytes must be preserved too: {bodies:?}"
    );
}

/// // #4181 (ADR-0042): the seeder writes NO MCP approval.
///
/// Why: an approved name makes a workspace `.mcp.json` entry of that name win
/// over the operator's own user-scope declaration — the displacement the
/// #3918→#3950 chain kept re-defusing. Removing the approval removes it.
/// What: seeds a fresh config dir and asserts the trust keys are present and
/// `enabledMcpjsonServers` is absent.
/// Test: this is the test.
#[test]
fn test_preseed_managed_trust_writes_no_mcp_approval() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("cfg");
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    preseed_managed_trust(&cfg, &workspace).unwrap();

    let projects = read_projects(&cfg);
    let entry = &projects[&workspace.to_string_lossy().to_string()];
    assert_eq!(entry["hasTrustDialogAccepted"], serde_json::json!(true));
    assert!(
        entry.get("enabledMcpjsonServers").is_none(),
        "no MCP name may be pre-approved: {entry}"
    );
}

/// // #4181: a stale approval a prior version wrote is REMOVED.
///
/// Why: ceasing to write leaves the key in place on every machine an older tm
/// launched, so the displacement stays live exactly where a repo could reach
/// it. The strip is what makes the invariant real rather than forward-only.
/// What: seeds over an entry that already carries a full approval list and a
/// higher onboarding count, and asserts the key is gone while Claude Code's own
/// runtime state in the same entry survives.
/// Test: this is the test.
#[test]
fn test_preseed_managed_trust_strips_a_stale_mcp_approval() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("cfg");
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&cfg).unwrap();
    write_config_with_projects(
        &cfg,
        serde_json::json!({
            workspace.to_string_lossy().to_string(): {
                "hasTrustDialogAccepted": true,
                "hasCompletedProjectOnboarding": true,
                "projectOnboardingSeenCount": 4,
                "enabledMcpjsonServers": ["trusty-mpm", "evil-server"],
                "lastSessionId": "abc-123"
            }
        }),
    );

    preseed_managed_trust(&cfg, &workspace).unwrap();

    let projects = read_projects(&cfg);
    let entry = &projects[&workspace.to_string_lossy().to_string()];
    assert!(
        entry.get("enabledMcpjsonServers").is_none(),
        "a stale approval must be stripped, not merely left unwritten: {entry}"
    );
    assert_eq!(entry["lastSessionId"], serde_json::json!("abc-123"));
    assert_eq!(entry["projectOnboardingSeenCount"], serde_json::json!(4));
}
