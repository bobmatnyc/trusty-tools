//! Tests for the `asset_tier` doctor probe (#4442).
//!
//! Why: the probe's whole value is that it FIRES on a shadowing project-tier
//! copy that every presence-only check reports green on, and stays silent on a
//! project's own agents — including one whose NAME collides with a bundled
//! agent but whose ledger entry proves the operator owns it. Both halves are
//! pinned here; delete the check and `project_tier_stub_on_a_bundled_name_fails`
//! fails.
//! What: end-to-end `check_asset_tier` cases against temp project/home trees,
//! plus the pure `verdict` branches and the roster construction.
//! Test: this file.

use super::*;
use trusty_agents_common::agents::manifest::{AgentManifest, ManifestEntry, Origin, checksum};
use trusty_agents_common::agents::tier_audit::TierResidentClass;

/// A hermetic `FrameworkPaths` whose agent SOURCE dir is inside `base`.
///
/// `trusty_mpm_root` is cleared so `agent_source_dir()` cannot resolve to the
/// real repository's `agents/agents` submodule and leak host state into the
/// assertions (same guard `doctor_deploy_validate`'s tests use).
fn hermetic_paths(base: &Path) -> FrameworkPaths {
    let mut paths = FrameworkPaths::under(base);
    paths.trusty_mpm_root = None;
    paths
}

/// A minimal agent document declaring `name`.
fn doc(name: &str) -> String {
    format!("---\nname: {name}\nrole: engineer\n---\n\nBody.\n")
}

/// Write `<base>/.claude/agents/<file_name>` with the given content.
fn place_agent(base: &Path, file_name: &str, body: &str) {
    let dir = base.join(".claude").join("agents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(file_name), body).unwrap();
}

/// Claim `<base>/.claude/agents/<file_name>` in that tier's ownership ledger.
fn track(base: &Path, file_name: &str, origin: Origin) {
    let dir = base.join(".claude").join("agents");
    let mut manifest = AgentManifest::default();
    manifest.managed.insert(
        file_name.to_owned(),
        ManifestEntry {
            source_chain: vec![],
            checksum: checksum("whatever"),
            deployed_at: "2026-07-31T00:00:00Z".to_owned(),
            origin,
        },
    );
    manifest.save(&dir).unwrap();
}

/// Build a scan result for the pure `verdict` tests.
fn scan(label: &'static str, dir: &Path, names: &[&str]) -> TierScan {
    TierScan {
        label,
        dir: dir.to_path_buf(),
        found: names
            .iter()
            .map(|s| MisplacedAgent {
                path: dir.join(format!("{s}.md")),
                name: (*s).to_owned(),
                class: TierResidentClass::ShadowsBundled,
            })
            .collect(),
    }
}

#[test]
fn project_tier_stub_on_a_bundled_name_fails() {
    // THE case: #4408's 32-byte project-tier stub resolving to a bundled name.
    // Presence-only checks see a perfect canonical deploy; this must Fail.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    place_agent(
        &project,
        "rust-engineer.md",
        "---\nname: rust-engineer\n---\n",
    );

    let paths = hermetic_paths(&home);
    let check = check_asset_tier(&paths, Some(&project), &home);

    assert_eq!(
        check.status,
        CheckStatus::Fail,
        "message: {}",
        check.message
    );
    assert!(check.message.contains("rust-engineer"), "{}", check.message);
    assert!(check.message.contains("project tier"), "{}", check.message);
}

#[test]
fn clean_project_tier_is_ok() {
    // The negative control: nothing outside the canonical tier, no noise.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();

    let paths = hermetic_paths(&home);
    let check = check_asset_tier(&paths, Some(&project), &home);

    assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
}

#[test]
fn custom_project_agent_does_not_fire() {
    // The anti-noise guarantee that lets #4443 ship project-custom agents: a
    // name tm does not bundle is the project's own and must never be reported.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    place_agent(
        &project,
        "acme-internal-reviewer.md",
        &doc("acme-internal-reviewer"),
    );

    let paths = hermetic_paths(&home);
    let check = check_asset_tier(&paths, Some(&project), &home);

    assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
}

#[test]
fn user_owned_project_agent_on_a_bundled_name_does_not_fire() {
    // Ownership beats a name collision end to end: the operator's own `qa.md`,
    // tracked `Origin::User`, on a name tm also ships. Flagging it would point
    // #4448 at a file it must never move.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    place_agent(&project, "qa.md", &doc("qa"));
    track(&project, "qa.md", Origin::User);

    let paths = hermetic_paths(&home);
    let check = check_asset_tier(&paths, Some(&project), &home);

    assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
}

#[test]
fn a_renamed_project_file_declaring_a_bundled_name_fails() {
    // Identity is the frontmatter name: `helper.md` declaring `name: qa`
    // shadows `qa`, and a stem-keyed predicate would miss it entirely.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    place_agent(&project, "helper.md", &doc("qa"));

    let paths = hermetic_paths(&home);
    let check = check_asset_tier(&paths, Some(&project), &home);

    assert_eq!(
        check.status,
        CheckStatus::Fail,
        "message: {}",
        check.message
    );
    assert!(check.message.contains("qa"), "{}", check.message);
}

#[test]
fn home_tier_copy_is_only_a_warn() {
    // A managed session relocates CLAUDE_CONFIG_DIR, so a `~/.claude/agents`
    // copy is stale rather than shadowing — real, but not the hard failure.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    place_agent(&home, "qa.md", &doc("qa"));

    let paths = hermetic_paths(&home);
    let check = check_asset_tier(&paths, Some(&project), &home);

    assert_eq!(
        check.status,
        CheckStatus::Warn,
        "message: {}",
        check.message
    );
    assert!(check.message.contains("qa"), "{}", check.message);
}

#[test]
fn canonical_tier_is_never_scanned() {
    // Every file in the canonical directory is tm-owned by definition; scanning
    // it would make the probe fire permanently on a healthy install.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let paths = hermetic_paths(&home);

    let canonical = paths.agent_deploy_dir();
    std::fs::create_dir_all(&canonical).unwrap();
    std::fs::write(canonical.join("rust-engineer.md"), doc("rust-engineer")).unwrap();

    let check = check_asset_tier(&paths, None, &home);
    assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
}

#[test]
fn roster_falls_back_to_the_embedded_bundle() {
    // With no agent source on disk (a binary-only install) the roster must
    // still be populated — an empty roster would classify every shadowing copy
    // as a legitimate custom agent and report a false green.
    let tmp = tempfile::tempdir().unwrap();
    let paths = hermetic_paths(tmp.path());
    assert!(!paths.agent_source_dir().is_dir());

    let roster = bundled_roster(&paths);
    assert!(
        roster.contains("rust-engineer"),
        "embedded roster missing rust-engineer: {roster:?}"
    );
}

#[test]
fn roster_keys_the_embedded_half_by_declared_name() {
    // `BASE-AGENT.md` declares `name: base-agent`. Keying the embedded half by
    // rel_path stem would put `BASE-AGENT` in the roster and silently exempt
    // the name the harness actually resolves.
    let tmp = tempfile::tempdir().unwrap();
    let paths = hermetic_paths(tmp.path());

    let roster = bundled_roster(&paths);
    assert!(roster.contains("base-agent"), "roster: {roster:?}");
    assert!(!roster.contains("BASE-AGENT"), "roster: {roster:?}");
}

#[test]
fn roster_includes_on_disk_source_names() {
    // A locally installed roster is the accurate authority — an agent added
    // there must classify even before the binary that bundles it ships.
    let tmp = tempfile::tempdir().unwrap();
    let paths = hermetic_paths(tmp.path());
    let source = paths.agent_source_dir();
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("whatever.md"), doc("locally-installed")).unwrap();

    assert!(bundled_roster(&paths).contains("locally-installed"));
}

#[test]
fn verdict_project_hit_is_fail() {
    let dir = PathBuf::from("/w/.claude/agents");
    let canonical = PathBuf::from("/c/agents");
    let check = verdict(&[scan("project", &dir, &["qa"])], &canonical);
    assert_eq!(check.status, CheckStatus::Fail);
}

#[test]
fn verdict_home_only_is_warn() {
    let dir = PathBuf::from("/h/.claude/agents");
    let canonical = PathBuf::from("/c/agents");
    let check = verdict(&[scan("operator home", &dir, &["qa"])], &canonical);
    assert_eq!(check.status, CheckStatus::Warn);
}

#[test]
fn verdict_clean_is_ok() {
    let dir = PathBuf::from("/w/.claude/agents");
    let canonical = PathBuf::from("/c/agents");
    let check = verdict(&[scan("project", &dir, &[])], &canonical);
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(check.message.contains("/c/agents"), "{}", check.message);
}

#[test]
fn verdict_names_the_files_and_both_tiers() {
    // The message must be actionable without reading the source: which agents,
    // which directories, and what to run.
    let project = PathBuf::from("/w/.claude/agents");
    let home = PathBuf::from("/h/.claude/agents");
    let canonical = PathBuf::from("/c/agents");
    let check = verdict(
        &[
            scan("project", &project, &["qa"]),
            scan("operator home", &home, &["engineer"]),
        ],
        &canonical,
    );
    assert_eq!(check.status, CheckStatus::Fail);
    for needle in [
        "/w/.claude/agents",
        "/h/.claude/agents",
        "/c/agents",
        "qa",
        "engineer",
        "--reset-agents-workspaces",
    ] {
        assert!(
            check.message.contains(needle),
            "missing {needle}: {}",
            check.message
        );
    }
}

#[test]
fn verdict_summarises_a_long_list() {
    // Six offenders must not produce a six-name wall; the count survives.
    let dir = PathBuf::from("/w/.claude/agents");
    let canonical = PathBuf::from("/c/agents");
    let names = ["a", "b", "c", "d", "e", "f"];
    let check = verdict(&[scan("project", &dir, &names)], &canonical);
    assert!(check.message.contains("(+1 more)"), "{}", check.message);
}
