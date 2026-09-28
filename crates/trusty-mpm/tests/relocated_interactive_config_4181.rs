//! `tm launch` / `tm connect` relocate `CLAUDE_CONFIG_DIR` — issue #4181.
//!
//! Why an integration test rather than a `managed_config_tests.rs` sibling:
//! `ensure_managed_config_dir_with_root` emits a `tracing::warn!` that
//! `ensure_managed_config_dir_emits_the_frozen_skill_warning` captures through a
//! thread-local subscriber, and `tracing`'s runtime max-level is process-global
//! and only raised when some test installs a GLOBAL default. That test therefore
//! passes or fails on the lib binary's thread schedule. Adding cases to the same
//! binary reshuffles that schedule; a separate integration binary is a separate
//! process, so these cases cannot perturb it at all.
//!
//! What: the relocation's four externally-visible guarantees — trust seeded into
//! the managed `.claude.json`, the operator's own `.claude.json` untouched
//! (#1269), a malformed managed file quarantined rather than fatal, and #3950's
//! pin contract carried across the move.

use std::path::Path;

use tempfile::TempDir;
use trusty_mpm::core::managed_config::prepare_interactive_config_dir_in;
use trusty_mpm::core::paths::FrameworkPaths;

/// A framework root under `base` with its source dirs present.
///
/// The roster contents do not matter here — `ensure_managed_config_dir_with_root`
/// fails open on a thin source, and every assertion below is about the trust
/// file, not the deploy.
fn framework(base: &Path) -> FrameworkPaths {
    let fw = FrameworkPaths::under(base);
    std::fs::create_dir_all(&fw.agents).unwrap();
    std::fs::create_dir_all(&fw.skills).unwrap();
    fw
}

/// The workspace the session is launched against.
fn workspace(base: &Path) -> std::path::PathBuf {
    let dir = base.join("workspace");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Read `<config_dir>/.claude.json`'s entry for `ws`, or `None`.
fn managed_entry(config_dir: &Path, ws: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(config_dir.join(".claude.json")).ok()?;
    let cfg: serde_json::Value = serde_json::from_str(&text).ok()?;
    cfg.get("projects")?
        .get(ws.to_string_lossy().as_ref())
        .cloned()
}

/// #4181: the trust seed lands in the file a RELOCATED session actually reads.
///
/// Before this change `tm launch` seeded `~/.claude.json`. A session whose
/// `CLAUDE_CONFIG_DIR` points elsewhere never opens that file, so the trust
/// dialog it was meant to dismiss would block every relocated launch.
#[test]
fn interactive_config_dir_seeds_trust_in_the_managed_dir() {
    let tmp = TempDir::new().unwrap();
    let fw = framework(tmp.path());
    let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");
    let ws = workspace(tmp.path());

    prepare_interactive_config_dir_in(&fw, &config_dir, &ws);

    let entry =
        managed_entry(&config_dir, &ws).expect("the workspace must be seeded in the managed file");
    assert_eq!(
        entry.get("hasTrustDialogAccepted"),
        Some(&serde_json::Value::Bool(true)),
        "#1269's other guarantee — the trust dialog stays pre-dismissed: {entry}"
    );
}

/// #4181 + #1269: relocation must not write the operator's `~/.claude.json`.
///
/// This is the isolation proof in file terms. The relocated `user` tier is the
/// tm-owned config home, so the operator's own config is a source that SHOULD be
/// excluded — and this asserts it still is, by leaving a decoy in an isolated
/// fake home and checking it comes back byte-identical.
#[test]
fn interactive_config_dir_never_writes_the_home_claude_json() {
    let tmp = TempDir::new().unwrap();
    let fw = framework(tmp.path());
    let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");
    let ws = workspace(tmp.path());

    let fake_home_json = tmp.path().join(".claude.json");
    let decoy = r#"{"operatorOnly":true}"#;
    std::fs::write(&fake_home_json, decoy).unwrap();

    prepare_interactive_config_dir_in(&fw, &config_dir, &ws);

    assert_eq!(
        std::fs::read_to_string(&fake_home_json).unwrap(),
        decoy,
        "the operator's own .claude.json must be left untouched"
    );
    assert!(
        managed_entry(&config_dir, &ws).is_some(),
        "the seed must have gone to the managed dir instead"
    );
}

/// #4181 fail-open arm: a malformed `<config_dir>/.claude.json` does not stop the
/// launch, and the corrupt bytes are quarantined rather than discarded.
#[test]
fn interactive_config_dir_survives_a_malformed_managed_claude_json() {
    let tmp = TempDir::new().unwrap();
    let fw = framework(tmp.path());
    let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");
    let ws = workspace(tmp.path());
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join(".claude.json"), "{ not json at all").unwrap();

    prepare_interactive_config_dir_in(&fw, &config_dir, &ws);

    assert!(
        managed_entry(&config_dir, &ws).is_some(),
        "seeding must proceed from a fresh object after quarantine"
    );
    let quarantined = std::fs::read_dir(&config_dir)
        .unwrap()
        .filter_map(Result::ok)
        .any(|e| e.file_name().to_string_lossy().contains(".claude.json."));
    assert!(
        quarantined,
        "the malformed bytes must be preserved for post-mortem, not discarded"
    );
}

/// #4181 fail-open arm: an absent config dir is created rather than fatal.
#[test]
fn interactive_config_dir_creates_an_absent_dir() {
    let tmp = TempDir::new().unwrap();
    let fw = framework(tmp.path());
    let config_dir = tmp.path().join("never/created/claude-config");
    let ws = workspace(tmp.path());
    assert!(!config_dir.exists());

    prepare_interactive_config_dir_in(&fw, &config_dir, &ws);

    assert!(
        config_dir.join(".claude.json").exists(),
        "an absent relocated config dir must be provisioned, not refused"
    );
}

/// #3950, carried onto the relocated interactive path: a builtin whose injector
/// did NOT pin its entry this run must not be pre-approved.
///
/// The relocation moves WHERE `enabledMcpjsonServers` is written; it must not
/// relax WHAT goes into it. A hostile clone's `.mcp.json` entry under a framework
/// name would otherwise execute with no human present to decline it.
#[test]
fn interactive_config_dir_withholds_builtins_when_a_pin_failed() {
    let tmp = TempDir::new().unwrap();
    let fw = framework(tmp.path());
    let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");
    let ws = workspace(tmp.path());

    prepare_interactive_config_dir_in(&fw, &config_dir, &ws);

    let entry = managed_entry(&config_dir, &ws).unwrap();
    let enabled = entry
        .get("enabledMcpjsonServers")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for name in [
        "trusty-mpm",
        "trusty-review",
        "trusty-memory",
        "trusty-search",
    ] {
        assert!(
            !enabled.iter().any(|v| v.as_str() == Some(name)),
            "{name} was not pinned this run and must NOT be pre-approved: {enabled:?}"
        );
    }
}
