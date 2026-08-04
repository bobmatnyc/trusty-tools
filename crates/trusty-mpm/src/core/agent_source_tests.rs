//! Unit tests for [`super`] — bundled agent-source self-heal and auto-deploy
//! (issue #4840).
//!
//! Why: #4840's four load-bearing behaviors are (1) the deploy fires when the
//! compiled-in bundle differs from disk, (2) it is a no-op when identical,
//! (3) it fails open when the target cannot be written, and (4) it WARNS —
//! in a bounded count-plus-preview summary, not one line per file (PR #4848
//! review) — whenever it declines to overwrite something or fails to compose
//! it. Each gets a test here.
//! What: hermetic — every case runs against `tempfile::TempDir`, never the
//! real `~/.trusty-mpm`.
//! Test: this file.

use super::*;

/// The real bundled agent set is materialized into a temp dir per test, so
/// these assertions pin the actual shipped roster rather than a fixture.
fn bundled_agent_basenames() -> Vec<&'static str> {
    bundle::ALL
        .iter()
        .filter(|a| a.rel_path.starts_with("agents/"))
        .map(|a| a.rel_path.strip_prefix("agents/").unwrap())
        .collect()
}

#[test]
fn agent_bundle_stamp_is_stable_across_calls() {
    // Pure function of the compiled-in table: two calls must agree, or the
    // "cheap when nothing changed" gate would re-materialize on every launch.
    assert_eq!(agent_bundle_stamp(), agent_bundle_stamp());
}

#[test]
fn agent_bundle_stamp_differs_from_skill_stamp() {
    // The agent stamp must fingerprint the `agents/*` slice, not the whole
    // table — otherwise a skill-only change would churn the agent source.
    assert_ne!(
        agent_bundle_stamp(),
        crate::core::skill_source::skill_bundle_stamp()
    );
}

#[test]
fn materialize_agent_artifacts_writes_all_agents() {
    let tmp = tempfile::TempDir::new().unwrap();
    let agents = tmp.path().join("agents");

    let written = materialize_agent_artifacts(&agents).unwrap();

    let expected = bundled_agent_basenames();
    assert_eq!(written.len(), expected.len());
    for name in expected {
        let content = std::fs::read_to_string(agents.join(name))
            .unwrap_or_else(|e| panic!("missing {name}: {e}"));
        let artifact = bundle::ALL
            .iter()
            .find(|a| a.rel_path == format!("agents/{name}"))
            .unwrap();
        assert_eq!(
            content, artifact.contents,
            "{name} content must match bundle"
        );
    }
    assert!(
        agents.join("BASE-AGENT.md").exists(),
        "BASE-AGENT.md is the file #4840 measured as stale — it must materialize"
    );
}

#[test]
fn materialize_agent_artifacts_prunes_files_not_in_table() {
    let tmp = tempfile::TempDir::new().unwrap();
    let agents = tmp.path().join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    // A leftover from a renamed/removed agent the current table no longer lists.
    std::fs::write(agents.join("retired-engineer.md"), "stale\n").unwrap();

    materialize_agent_artifacts(&agents).unwrap();

    assert!(!agents.join("retired-engineer.md").exists());
    assert!(agents.join("engineer.md").exists());
}

#[test]
fn ensure_agent_source_fresh_materializes_when_missing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let agents = tmp.path().join("agents");
    assert!(!agents.exists());

    let refreshed = ensure_agent_source_fresh(&agents).unwrap();

    assert!(refreshed, "a missing source dir must trigger a refresh");
    assert!(agents.join("BASE-AGENT.md").exists());
    assert!(agents.join(STAMP_FILE_NAME).exists());
}

#[test]
fn ensure_agent_source_fresh_is_noop_when_current() {
    let tmp = tempfile::TempDir::new().unwrap();
    let agents = tmp.path().join("agents");
    assert!(ensure_agent_source_fresh(&agents).unwrap());

    let marker = agents.join("BASE-AGENT.md");
    let before = std::fs::metadata(&marker).unwrap().modified().unwrap();

    let refreshed = ensure_agent_source_fresh(&agents).unwrap();

    assert!(!refreshed, "an already-current source dir must be a no-op");
    assert_eq!(
        before,
        std::fs::metadata(&marker).unwrap().modified().unwrap(),
        "a no-op refresh must not rewrite files"
    );
}

#[test]
fn ensure_agent_source_fresh_prunes_renamed_files() {
    // The #4840 shape after an agent rename: an operator's existing source dir
    // holds a stale stamp (or none) plus a leftover pre-rename file.
    let tmp = tempfile::TempDir::new().unwrap();
    let agents = tmp.path().join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("mpm-old-agent.md"), "stale\n").unwrap();
    std::fs::write(agents.join(STAMP_FILE_NAME), "stale-stamp").unwrap();

    let refreshed = ensure_agent_source_fresh(&agents).unwrap();

    assert!(refreshed, "a stale stamp must trigger a refresh");
    assert!(!agents.join("mpm-old-agent.md").exists());
    assert!(agents.join("engineer.md").exists());
}

#[test]
fn deploy_summary_lines_is_empty_on_a_clean_deploy() {
    let result = DeployResult {
        deployed: vec!["engineer.md".into()],
        ..DeployResult::default()
    };
    assert!(deploy_summary_lines(&result).is_empty());
}

#[test]
fn deploy_summary_lines_summarises_skips_in_one_line() {
    // PR #4848 review (HIGH): the skipped set can be dozens wide and is never
    // reconciled until someone runs `--reset-agents`, so this runs on every
    // spawn and resume. It must be ONE line with a count + preview, matching
    // `agents::deployer`'s #2504 policy — never one line per file.
    let skipped: Vec<String> = (0..12).map(|i| format!("agent-{i}.md")).collect();
    let result = DeployResult {
        skipped: skipped.clone(),
        untracked_modified: vec!["agent-0.md".into()],
        ..DeployResult::default()
    };

    let lines = deploy_summary_lines(&result);

    assert_eq!(lines.len(), 1, "one summary line, not N: {lines:?}");
    assert!(lines[0].contains("12 agent file(s)"), "{:?}", lines[0]);
    assert!(lines[0].contains("agent-0.md"), "{:?}", lines[0]);
    assert!(lines[0].contains('…'), "elision marker: {:?}", lines[0]);
    assert!(
        !lines[0].contains("agent-11.md"),
        "the preview must be truncated: {:?}",
        lines[0]
    );
    // The actionable pointer survives the shrink — the original silence was
    // half the defect.
    assert!(
        lines[0].contains("--reset-agents agent-0"),
        "{:?}",
        lines[0]
    );
}

#[test]
fn deploy_summary_lines_reports_failed_agents() {
    // A compose failure means the agent does not land AT ALL — worse than
    // stale, and previously never surfaced anywhere (PR #4848 review, MEDIUM).
    let result = DeployResult {
        failed: vec!["engineer: invalid frontmatter YAML: bad".into()],
        ..DeployResult::default()
    };

    let lines = deploy_summary_lines(&result);

    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("engineer"), "{:?}", lines[0]);
    assert!(
        lines[0].contains("did NOT deploy at all"),
        "the failure must read as worse than stale: {:?}",
        lines[0]
    );
}

#[test]
fn autodeploy_agents_deploys_when_bundle_differs() {
    // The core #4840 fix: nothing on disk, nobody ran `tm install`, and the
    // agents still land.
    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("framework/agents");
    let target = tmp.path().join("claude-config/agents");

    let out = autodeploy_agents(&source, &target);

    assert!(out.refreshed, "a stale (absent) source must refresh");
    assert!(
        out.warnings.is_empty(),
        "unexpected warnings: {:?}",
        out.warnings
    );
    assert!(
        out.deployed.iter().any(|f| f == "BASE-AGENT.md"),
        "BASE-AGENT.md must deploy: {:?}",
        out.deployed
    );
    assert!(target.join("engineer.md").exists());
}

#[test]
fn autodeploy_agents_is_a_noop_when_already_current() {
    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("framework/agents");
    let target = tmp.path().join("claude-config/agents");
    assert!(autodeploy_agents(&source, &target).refreshed);

    let marker = target.join("engineer.md");
    let before = std::fs::metadata(&marker).unwrap().modified().unwrap();

    let out = autodeploy_agents(&source, &target);

    assert!(!out.refreshed, "matching checksums must skip the refresh");
    assert!(
        out.deployed.is_empty(),
        "nothing should be rewritten: {:?}",
        out.deployed
    );
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    assert_eq!(
        before,
        std::fs::metadata(&marker).unwrap().modified().unwrap(),
        "a no-op run must not rewrite deployed files"
    );
}

#[test]
fn autodeploy_agents_fails_open_when_target_is_unwritable() {
    // A broken deploy must never block a session — it degrades to a warning.
    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("framework/agents");
    // A regular file where the target directory must go: nothing can be
    // written under it, and `create_dir_all` cannot repair it.
    let target = tmp.path().join("not-a-dir");
    std::fs::write(&target, "blocking file\n").unwrap();

    let out = autodeploy_agents(&source, &target);

    assert!(
        out.refreshed,
        "the source refresh is independent of the target and must still run"
    );
    assert!(out.deployed.is_empty());
    assert!(
        out.warnings.iter().any(|w| w.contains("could not deploy")),
        "the failure must surface as a warning, not an error: {:?}",
        out.warnings
    );
    // The blocking file is untouched — auto-deploy destroys nothing on failure.
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "blocking file\n");
}

#[test]
fn autodeploy_agents_fails_open_when_source_is_unwritable() {
    // The other half of fail-open: the source refresh itself cannot run.
    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("not-a-dir");
    std::fs::write(&source, "blocking file\n").unwrap();
    let target = tmp.path().join("claude-config/agents");

    let out = autodeploy_agents(&source, &target);

    assert!(!out.refreshed);
    assert!(
        out.warnings.iter().any(|w| w.contains("could not refresh")),
        "{:?}",
        out.warnings
    );
}

#[test]
fn autodeploy_agents_warns_when_it_skips_a_user_modified_file() {
    // #4840's second half: the deployer preserves an untracked, differing file
    // — correct, but it used to do so SILENTLY, which is how a three-day-stale
    // BASE-AGENT.md went unnoticed.
    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("framework/agents");
    let target = tmp.path().join("claude-config/agents");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        target.join("engineer.md"),
        "---\nname: engineer\ndescription: hand-edited\n---\n\nMine.\n",
    )
    .unwrap();

    let out = autodeploy_agents(&source, &target);

    assert!(
        out.warnings.iter().any(|w| w.contains("engineer.md")),
        "the skipped file must be named in a warning: {:?}",
        out.warnings
    );
    assert!(
        !out.deployed.iter().any(|f| f == "engineer.md"),
        "the user's file must be preserved, not overwritten"
    );
    assert!(
        std::fs::read_to_string(target.join("engineer.md"))
            .unwrap()
            .contains("Mine."),
        "the user's content must survive byte-for-byte"
    );
    // Everything else still deployed — one preserved file does not stall the roster.
    assert!(target.join("BASE-AGENT.md").exists());
}

#[test]
fn autodeploy_agents_for_skips_source_refresh_on_submodule() {
    // When the `agents/agents` git submodule is checked out it is the
    // authoritative, git-tracked source — materializing the compiled-in bundle
    // over it would be destructive. Deploy from it, never rewrite it.
    let tmp = tempfile::TempDir::new().unwrap();
    let checkout = tempfile::TempDir::new().unwrap();
    let submodule = checkout.path().join("agents").join("agents");
    std::fs::create_dir_all(&submodule).unwrap();
    std::fs::write(
        submodule.join("submodule-agent.md"),
        "---\nname: submodule-agent\ndescription: from the submodule\n---\n\nSUBMODULE.\n",
    )
    .unwrap();

    let mut paths = FrameworkPaths::under(tmp.path());
    paths.trusty_mpm_root = Some(checkout.path().to_path_buf());
    assert_eq!(paths.agent_source_dir(), submodule);
    let target = tmp.path().join("claude-config/agents");

    let out = autodeploy_agents_for(&paths, &target);

    assert!(
        !out.refreshed,
        "a submodule source is never re-materialized"
    );
    assert!(
        !paths.agents.exists(),
        "must never create the framework source dir when a submodule is in play"
    );
    assert_eq!(
        std::fs::read_dir(&submodule).unwrap().count(),
        1,
        "the git-tracked submodule must be left exactly as found"
    );
    assert!(target.join("submodule-agent.md").exists());
}

#[test]
fn autodeploy_agents_for_falls_back_when_the_submodule_is_empty() {
    // PR #4848 review: an EMPTY `agents/agents` (an uninitialized submodule on
    // a source checkout — and #4840 was originally measured on one) satisfies
    // `agent_source_dir()`'s `is_dir()` test, so the old code deployed from it
    // and landed NOTHING, silently. Fall back to the compiled-in bundle.
    let tmp = tempfile::TempDir::new().unwrap();
    let checkout = tempfile::TempDir::new().unwrap();
    let submodule = checkout.path().join("agents").join("agents");
    std::fs::create_dir_all(&submodule).unwrap();

    let mut paths = FrameworkPaths::under(tmp.path());
    paths.trusty_mpm_root = Some(checkout.path().to_path_buf());
    assert_eq!(paths.agent_source_dir(), submodule);
    let target = tmp.path().join("claude-config/agents");

    let out = autodeploy_agents_for(&paths, &target);

    assert!(
        out.refreshed,
        "the empty submodule must not suppress the bundle refresh"
    );
    assert!(
        out.deployed.iter().any(|f| f == "BASE-AGENT.md"),
        "the bundled roster must land: {:?}",
        out.deployed
    );
    assert!(target.join("engineer.md").exists());
    assert_eq!(
        std::fs::read_dir(&submodule).unwrap().count(),
        0,
        "the submodule directory is still never written"
    );
}

#[test]
fn autodeploy_agents_overwrites_a_drifted_bundled_file() {
    // Documented overwrite decision (#4840): a BUNDLED-origin file whose
    // checksum drifted IS re-deployed — the manifest records `origin`, and
    // framework-owned drift is corruption, not user ownership (#4408).
    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("framework/agents");
    let target = tmp.path().join("claude-config/agents");
    assert!(!autodeploy_agents(&source, &target).deployed.is_empty());

    // Corrupt a tracked, bundled-origin deployed file.
    std::fs::write(target.join("engineer.md"), "CORRUPTED\n").unwrap();

    let out = autodeploy_agents(&source, &target);

    assert!(
        out.deployed.iter().any(|f| f == "engineer.md"),
        "a drifted bundled-origin file must be re-deployed: {:?}",
        out.deployed
    );
    assert!(
        !std::fs::read_to_string(target.join("engineer.md"))
            .unwrap()
            .contains("CORRUPTED")
    );
    assert!(
        out.warnings.is_empty(),
        "repairing framework-owned drift is not a declined overwrite: {:?}",
        out.warnings
    );
}
