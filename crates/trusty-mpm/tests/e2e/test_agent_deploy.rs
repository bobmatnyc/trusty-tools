//! E2E: agent composition and deployment.
//!
//! These scenarios drive the `trusty_mpm_core` agent pipeline against
//! temp-directory source / target trees. They use the harness's
//! `write_agent_sources` helper, which writes a real three-level `extends:`
//! chain (`engineer -> base-engineer -> base-agent`) with lowercase filenames
//! so composition resolves identically on case-sensitive CI hosts.

use crate::harness::write_agent_sources;
use tempfile::TempDir;
use trusty_mpm::core::agent_builder::compose_agent;
use trusty_mpm::core::agent_deployer::deploy_agents;
use trusty_mpm::core::agent_manifest::AgentManifest;

/// Deploying composes the inheritance chain into a self-contained file that
/// carries content from every level.
#[test]
fn deploy_writes_composed_file() {
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_agent_sources(src.path());

    let result = deploy_agents(src.path(), tgt.path()).expect("deploy succeeds");
    assert!(result.deployed.contains(&"engineer.md".to_string()));

    let engineer = std::fs::read_to_string(tgt.path().join("engineer.md")).unwrap();
    assert!(engineer.contains("BASE-AGENT CONTENT"), "base level merged");
    assert!(
        engineer.contains("BASE-ENGINEER CONTENT"),
        "mid level merged"
    );
    assert!(engineer.contains("ENGINEER CONTENT"), "leaf level merged");
    // `extends:` must never leak into the composed frontmatter.
    assert!(!engineer.contains("extends:"));
}

/// After a deploy the manifest exists and records the deployed agent.
#[test]
fn deploy_creates_manifest() {
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_agent_sources(src.path());

    deploy_agents(src.path(), tgt.path()).expect("deploy succeeds");

    let manifest_path = tgt.path().join(".trusty-mpm-manifest.json");
    assert!(manifest_path.exists(), "manifest file written");

    let manifest = AgentManifest::load(tgt.path());
    assert!(manifest.is_managed("engineer.md"), "engineer.md tracked");
}

/// A target file trusty-mpm never deployed (absent from the manifest) is left
/// untouched by a subsequent deploy.
#[test]
fn deploy_skips_user_file() {
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_agent_sources(src.path());

    // User drops their own file, sharing a name with a source agent.
    let user_content = "USER OWNED FILE — not managed by trusty-mpm\n";
    std::fs::write(tgt.path().join("engineer.md"), user_content).unwrap();

    let result = deploy_agents(src.path(), tgt.path()).expect("deploy succeeds");
    assert!(result.skipped.contains(&"engineer.md".to_string()));

    let after = std::fs::read_to_string(tgt.path().join("engineer.md")).unwrap();
    assert_eq!(after, user_content, "user file untouched");
}

/// A managed BUNDLED file whose deployed copy drifted from the manifest
/// checksum is re-deployed from the bundle on the next deploy (issue #4408).
///
/// Bundled files are framework-owned (the `Overwrite` install tier), so a
/// checksum mismatch there means drift or corruption, never user ownership.
/// The user-owned tier is covered by `deploy_skips_user_file` above, which
/// still passes untouched.
#[test]
fn deploy_repairs_drifted_bundled_file() {
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_agent_sources(src.path());

    deploy_agents(src.path(), tgt.path()).expect("first deploy");
    let bundled = std::fs::read_to_string(tgt.path().join("engineer.md")).unwrap();

    // The deployed copy corrupts into the degenerate `v1` stub from the
    // incident, while the manifest still records the real content's checksum.
    std::fs::write(
        tgt.path().join("engineer.md"),
        "---\nname: engineer\n---\n\nv1\n",
    )
    .unwrap();

    let result = deploy_agents(src.path(), tgt.path()).expect("second deploy");
    assert!(result.deployed.contains(&"engineer.md".to_string()));
    assert!(!result.skipped.contains(&"engineer.md".to_string()));

    let after = std::fs::read_to_string(tgt.path().join("engineer.md")).unwrap();
    assert_eq!(after, bundled, "bundled composition restored");
}

/// Composing an agent resolves the full inheritance chain in base-first order.
#[test]
fn inheritance_chain_resolved() {
    let src = TempDir::new().unwrap();
    write_agent_sources(src.path());

    let composed = compose_agent("engineer", src.path()).expect("compose succeeds");

    let base = composed.find("BASE-AGENT CONTENT").expect("base present");
    let mid = composed.find("BASE-ENGINEER CONTENT").expect("mid present");
    let leaf = composed.find("ENGINEER CONTENT").expect("leaf present");
    assert!(base < mid, "base content precedes mid content");
    assert!(mid < leaf, "mid content precedes leaf content");
}
