//! #9396: a launch reads its PM content and its agent roster from one source.

use super::*;
use crate::core::framework_content::tests::fake_checkout_with_package;

/// #9396: a project inside a checkout gets that checkout's roster even when
/// the process cwd sits in another checkout with a different roster — the
/// repository's own, here. One launch never mixes two sources.
#[test]
fn the_launch_roster_comes_from_the_launch_content() {
    let checkout = tempfile::tempdir().expect("tempdir");
    let package = test_support::rc().required("pm-instruction-package.json");
    fake_checkout_with_package(checkout.path(), package);
    let agents = checkout.path().join("content/agents");
    std::fs::write(agents.join("BASE-AGENT.md"), "base\n").expect("base agent");
    std::fs::write(agents.join("only-here.md"), "here\n").expect("agent");
    let project = checkout.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let cwd = test_support::repo_root().join("crates/trusty-mpm");

    let resolved = resolve_for_in(Some(&project), Some(&cwd), None, Fetch::Never)
        .expect("the project's checkout");
    let launch = LaunchContent::load(&resolved).expect("launch content");
    let roster = launch.roster.expect("the checkout's roster");
    assert_eq!(roster.len(), 2, "the project's roster, not the cwd's");
    assert!(roster.get("only-here.md").is_some());
    assert_eq!(launch.framework.source(), roster.source(), "one source");
}
