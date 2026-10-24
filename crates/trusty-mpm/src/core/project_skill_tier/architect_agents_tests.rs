//! #10161 D3c: the Architect agents' seed, deploy rule and shipped history.
//!
//! Why: the agents must reach the Architect project only, follow the skills'
//! shipped-history and owner-edit rule, and trip a test when their text moves
//! without its old hash joining the history.
//! What: deploys the checkout's agents into temp projects, plants an owner edit
//! and an older shipped text, and resolves a tier onto a PM tier by symlink.
//! Test: this is the test module.

use sha2::{Digest, Sha256};

use super::*;
use crate::core::content_source::test_support::rc;
use crate::core::project_skill_tier::architect_seed::load_agents_from;

/// The checkout's shipped agents, in [`SHIPPED_AGENTS`] order.
fn shipped() -> Vec<ArchitectSkill> {
    load_agents_from(rc(), SHIPPED_AGENTS).expect("the checkout carries every agent")
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// One agent named `probe` with `text` and an optional shipped history.
fn probe(text: &str, previous: &'static [&'static str]) -> Vec<ArchitectSkill> {
    vec![ArchitectSkill {
        stem: "probe",
        contents: text.to_owned(),
        previous,
    }]
}

/// Drift guard: editing an agent without moving its old hash into `previous`
/// would make a `tm fleet init` seed of the old text read as an owner edit.
#[test]
fn the_shipped_agent_history_is_pinned_so_an_asset_edit_trips() {
    const PINNED: [(&str, &str); 5] = [
        (
            "runner",
            "9486452f2c982e4479a94d19167e3cc41d6c2d3823e9a6e560484af0c7554d55",
        ),
        (
            "event-extractor",
            "474a1373fa089e151d239f4f43c6aeb32cbdab362b985ca4db16ef59998ced2a",
        ),
        (
            "improver",
            "3c1cde4b226291e68591defd73fbf9121ff5a7ab188ec14aa81b846f6d8fa124",
        ),
        (
            "improvement-curator",
            "53d1774e096f61874bbe43feed0da552d09178a28d85c3d3886e03ade5ff5325",
        ),
        (
            "drift-guard",
            "1e94d54d878bfe88c37a1a9319b94bf576f1a0aae9234aa9cd4ab1612b222b66",
        ),
    ];
    // An agent on its first release has no earlier text; its first edit
    // moves it off this list and its old hash into `previous`.
    const FIRST_RELEASE: [&str; 5] = [
        "runner",
        "event-extractor",
        "improver",
        "improvement-curator",
        "drift-guard",
    ];
    assert_eq!(PINNED.len(), SHIPPED_AGENTS.len());
    for (agent, (name, pinned)) in shipped().iter().zip(PINNED) {
        assert_eq!(agent.stem, name);
        assert_eq!(
            sha256_hex(agent.contents.as_bytes()),
            pinned,
            "{name} changed: add the OLD hash (the PINNED value) to its `previous` \
             list, then update PINNED"
        );
        assert_eq!(
            agent.previous.is_empty(),
            FIRST_RELEASE.contains(&name),
            "{name}: only a first-release agent has no shipped history"
        );
        assert!(
            !agent.previous.contains(&pinned),
            "{name}: the current text is listed as a previous one"
        );
    }
}

/// Each shipped agent declares its own name, so Claude Code dispatches it by
/// the file tm writes; the usage-set agent is not in this release (Q1 a).
#[test]
fn every_shipped_agent_names_itself_and_none_is_from_a_later_slice() {
    let agents = shipped();
    assert_eq!(agents.len(), 5);
    for agent in &agents {
        let head = agent.contents.split("\n---\n").next().unwrap_or_default();
        assert!(
            head.lines().any(|l| l == format!("name: {}", agent.stem)),
            "{}: frontmatter does not name it",
            agent.stem
        );
        assert!(
            head.contains("metadata:\n  version: "),
            "{}: no metadata block",
            agent.stem
        );
    }
    assert!(!agents.iter().any(|a| a.stem == "token-optimizer"));
}

/// The five agents land byte-equal in `<project>/.claude/agents`, recorded,
/// and nowhere else under `.claude`.
#[test]
fn the_five_agents_are_written_byte_equal_and_recorded() {
    let project = tempfile::tempdir().unwrap();
    let agents = shipped();
    let report = deploy_architect_agents(project.path(), &[], &agents);
    assert!(report.errors.is_empty(), "{report:?}");
    let names: Vec<&str> = SHIPPED_AGENTS.iter().map(|a| a.name).collect();
    assert_eq!(report.written, names);
    let tier = agents_tier(project.path());
    for agent in &agents {
        let live = std::fs::read_to_string(agent_path(&tier, agent.stem)).unwrap();
        assert_eq!(live, agent.contents, "{}", agent.stem);
    }
    assert!(tier.join(AGENT_RECORD_FILE).is_file());
    let mut under_claude: Vec<String> = std::fs::read_dir(project.path().join(".claude"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    under_claude.sort();
    assert_eq!(under_claude, ["agents"]);
    let again = deploy_architect_agents(project.path(), &[], &agents);
    assert_eq!(again, DeployReport::default(), "a second run wrote");
}

/// A `.claude` that resolves onto a PM agent tier is refused: nothing is
/// written there, and the one error line names the tier.
#[test]
fn an_agent_tier_resolving_onto_a_pm_tier_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let pm = root.path().join("pm-config");
    std::fs::create_dir_all(pm.join("agents")).unwrap();
    let project = root.path().join("architect");
    std::fs::create_dir_all(&project).unwrap();
    std::os::unix::fs::symlink(&pm, project.join(".claude")).unwrap();
    let report = deploy_architect_agents(&project, &[pm.join("agents")], &shipped());
    assert!(report.written.is_empty(), "{report:?}");
    assert_eq!(report.errors.len(), 1, "{report:?}");
    assert!(report.errors[0].contains("agents are never written there"), "{report:?}");
    assert_eq!(std::fs::read_dir(pm.join("agents")).unwrap().count(), 0);
}

/// The skills' rule, for agents: an older shipped text with no record is
/// refreshed, an owner's edit is kept and reported, and a refreshed copy
/// follows the next shipped text.
#[test]
fn an_older_shipped_agent_is_refreshed_and_an_edited_one_is_kept() {
    let old = "---\nname: probe\n---\nv1\n";
    // sha256 of `old`, as a shipped history entry.
    let history: &'static [&'static str] =
        &["be6e021341490aae26cb26c453ddf08bf9d1e8eb1a01db98d58dc7435cd9d07b"];
    assert_eq!(sha256_hex(old.as_bytes()), history[0]);
    let untouched = tempfile::tempdir().unwrap();
    let path = agent_path(&agents_tier(untouched.path()), "probe");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, old).unwrap();
    let report = deploy_architect_agents(untouched.path(), &[], &probe("v2\n", history));
    assert_eq!(report.written, ["probe"], "{report:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "v2\n");

    let edited = tempfile::tempdir().unwrap();
    deploy_architect_agents(edited.path(), &[], &probe("v2\n", history));
    let path = agent_path(&agents_tier(edited.path()), "probe");
    std::fs::write(&path, "the owner's version\n").unwrap();
    let report = deploy_architect_agents(edited.path(), &[], &probe("v3\n", history));
    assert_eq!(report.kept_custom, ["probe"], "{report:?}");
    assert!(report.written.is_empty());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "the owner's version\n"
    );
}
