//! Startup provisioning, happy path and failure modes (#4325).
//!
//! Why: this runs on EVERY app launch, so two properties matter more than the
//! happy path — it must never be able to fail startup, and it must never
//! disturb a home the user has edited. Both are covered here rather than
//! assumed from `ensure`'s own tests, because the boot path is where they
//! actually bite.
//! What: no home, complete home, partial home, user-edited home, and a home the
//! app CANNOT create.
//! Test: this module IS the test surface.

use super::home_tests::temp_home;
use crate::assistants::{
    AssistantHome, AssistantInstanceId, HomeIssueKind, provision, provision_all,
    provision_startup_homes_in,
};

#[test]
fn startup_creates_a_missing_home() {
    let (_tmp, home) = temp_home();
    assert!(!home.exists(), "precondition: nothing on disk yet");

    let report = provision(&home);

    assert!(home.exists());
    assert!(report.is_healthy(), "issues: {:?}", report.health.issues);
    assert!(report.created_anything());
    assert_eq!(report.id.as_str(), "izzie");
    assert_eq!(report.home, home.path());
    assert!(report.health.narration().is_none());
}

#[test]
fn second_startup_creates_nothing() {
    let (_tmp, home) = temp_home();
    let first = provision(&home);
    assert!(first.created_anything());

    let second = provision(&home);

    assert!(!second.created_anything(), "created: {:?}", second.created);
    assert!(second.is_healthy());
}

/// Why: a user who deleted one directory gets it back; everything else is left
/// exactly as they left it.
#[test]
fn startup_fills_only_the_gaps() {
    let (_tmp, home) = temp_home();
    provision(&home);
    std::fs::write(home.okg_dir().join("kept.md"), "mine").unwrap();
    std::fs::remove_dir_all(home.attachments_dir()).unwrap();

    let report = provision(&home);

    assert_eq!(report.created, vec![home.attachments_dir()]);
    assert!(report.is_healthy());
    assert_eq!(
        std::fs::read_to_string(home.okg_dir().join("kept.md")).unwrap(),
        "mine"
    );
}

/// Why: THE regression this module could cause. `ensure`'s never-overwrite
/// semantics used to guard an occasional explicit call; now they stand between
/// a user's edited files and every single boot.
#[test]
fn repeated_startup_never_disturbs_user_edits() {
    let (_tmp, home) = temp_home();
    provision(&home);
    std::fs::write(home.instructions_path(), "MY OWN INSTRUCTIONS\n").unwrap();
    std::fs::write(home.config_path(), "id = \"izzie\"\nmine = true\n").unwrap();

    for _ in 0..3 {
        let report = provision(&home);
        assert!(!report.created_anything());
        assert!(report.is_healthy());
    }

    assert_eq!(
        std::fs::read_to_string(home.instructions_path()).unwrap(),
        "MY OWN INSTRUCTIONS\n"
    );
    assert!(
        std::fs::read_to_string(home.config_path())
            .unwrap()
            .contains("mine = true")
    );
}

/// Why: the failure mode that matters. A file sitting where the home directory
/// belongs makes `create_dir_all` fail — the portable stand-in for a read-only
/// filesystem or a denied permission, neither of which can be simulated
/// reliably in CI. Startup must survive it and must say WHY.
#[test]
fn startup_survives_a_home_it_cannot_create() {
    let (_tmp, home) = temp_home();
    std::fs::create_dir_all(home.path().parent().unwrap()).unwrap();
    std::fs::write(home.path(), "a file, not a directory").unwrap();

    // The call itself returns — no panic, no Result to unwrap, no early exit.
    let report = provision(&home);

    assert!(!report.is_healthy());
    let issue = &report.health.issues[0];
    assert_eq!(
        issue.kind,
        HomeIssueKind::NotCreatable,
        "the creation failure must lead the report: {:?}",
        report.health.issues
    );
    assert!(!issue.remedy.is_empty());
    // The reason survives to the narration seam the concierge reads.
    let narration = report.health.narration().expect("degraded home narrates");
    assert!(narration.contains(&issue.remedy), "was: {narration}");
    assert!(!report.created_anything());
}

/// Why: a home that cannot be created for one instance must not stop the
/// others — startup provisions each independently.
#[test]
fn provisions_every_instance_independently() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("trusty-agents");
    std::fs::create_dir_all(&root).unwrap();
    // `broken` cannot be created; the other two can.
    std::fs::write(root.join("broken"), "in the way").unwrap();

    let roster: Vec<AssistantHome> = ["izzie", "broken", "cto-assistant"]
        .into_iter()
        .map(|n| AssistantHome::under(&root, AssistantInstanceId::new(n).unwrap()))
        .collect();

    let report = provision_all(roster);

    assert_eq!(report.homes.len(), 3);
    assert!(root.join("izzie").join("okg").is_dir());
    assert!(root.join("cto-assistant").join("okg").is_dir());

    let degraded: Vec<&str> = report.degraded().map(|h| h.id.as_str()).collect();
    assert_eq!(degraded, vec!["broken"]);
    let created: Vec<&str> = report.newly_created().map(|h| h.id.as_str()).collect();
    assert_eq!(created, vec!["izzie", "cto-assistant"]);
}

#[test]
fn an_empty_roster_is_not_an_error() {
    let report = provision_all(Vec::new());
    assert!(report.homes.is_empty());
    assert_eq!(report.degraded().count(), 0);
    assert_eq!(report.newly_created().count(), 0);
}

/// Why: provisioning creates the layout; it never repairs a file the user
/// broke. A malformed `config.toml` is reported, not rewritten — rewriting it
/// would destroy the user's work on the next boot.
#[test]
fn startup_reports_but_never_repairs_a_broken_file() {
    let (_tmp, home) = temp_home();
    provision(&home);
    std::fs::write(home.config_path(), "not [[[ toml").unwrap();

    let report = provision(&home);

    assert!(!report.is_healthy());
    assert!(
        report
            .health
            .issues
            .iter()
            .any(|i| i.kind == HomeIssueKind::Malformed && i.entry == "config.toml")
    );
    assert_eq!(
        std::fs::read_to_string(home.config_path()).unwrap(),
        "not [[[ toml",
        "provisioning must not rewrite what the user broke"
    );
}

/// Why: the hermetic core of the boot-path call — discovery and provisioning
/// wired together, pointed at a tempdir instead of a real `$HOME`. The `$HOME`
/// resolution and logging in `provision_startup_homes()` are the only things
/// this does not cover, deliberately: exercising them would write into the
/// developer's actual home directory on every `cargo test`.
#[test]
fn startup_provisions_the_discovered_roster() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("agents");
    let root = tmp.path().join("trusty-agents");
    std::fs::create_dir_all(agents.join("izzie")).unwrap();
    std::fs::write(
        agents.join("izzie").join("agent.toml"),
        "[agent]\nname = \"izzie\"\nrole = \"assistant\"\nextends = \"assistant\"\n",
    )
    .unwrap();
    // Declares the role but is not an instance — must get no home.
    std::fs::create_dir_all(agents.join("ctrl")).unwrap();
    std::fs::write(
        agents.join("ctrl").join("agent.toml"),
        "[agent]\nname = \"ctrl\"\nrole = \"assistant\"\n",
    )
    .unwrap();

    let report = provision_startup_homes_in(&root, &[agents]);

    assert_eq!(report.homes.len(), 1);
    assert_eq!(report.homes[0].id.as_str(), "izzie");
    assert!(root.join("izzie").join("okg").is_dir());
    assert!(!root.join("ctrl").exists(), "ctrl is not an instance");
    assert_eq!(report.degraded().count(), 0);

    // Second launch: idempotent, nothing created, still healthy.
    let second = provision_startup_homes_in(&root, &[tmp.path().join("agents")]);
    assert_eq!(second.newly_created().count(), 0);
    assert_eq!(second.degraded().count(), 0);
}
