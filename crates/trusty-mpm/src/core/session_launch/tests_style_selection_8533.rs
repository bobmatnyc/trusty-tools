//! #8533: the launch and `tm sessions instructions` select one output style.
//!
//! Why: the report assembled its own precedence chain without the manifest
//! tier, so a project whose only style source was the harness manifest saw the
//! report name the default style while the launch wrote the manifest's.
//! What: sets a style ONLY in the project manifest, runs the real launch, and
//! compares the `outputStyle` it wrote with what the report names.
//! Test: this is the test module.

use super::tests::EnvVarGuard;
use super::*;
use tempfile::tempdir;

#[test]
#[serial_test::serial]
fn a_manifest_only_style_is_named_alike_by_the_report_and_the_launch() {
    // `prepare_session_inner` seeds `$HOME/.claude.json` through the process
    // `$HOME`; see `prepare_session_continues_after_agent_deploy_failure`.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    // The manifest tier is the only tier that names a style: no flag, no
    // `.trusty-mpm.toml`, and the host config under `fw.root` does not exist.
    let manifest_dir = crate::core::harness_root::framework_dir(project);
    std::fs::create_dir_all(&manifest_dir).unwrap();
    std::fs::write(
        manifest_dir.join(crate::core::manifest::MANIFEST_FILE),
        "[style]\nactive = \"tm-demo-01\"\n",
    )
    .unwrap();
    let styles = project.join(crate::core::output_style::PROJECT_STYLES_DIR);
    std::fs::create_dir_all(&styles).unwrap();
    std::fs::write(
        styles.join("tm-demo-01.md"),
        "---\nname: tm-demo-01\n---\nSpeak as the fixture voice.\n",
    )
    .unwrap();

    prepare_session_inner(
        &fw,
        project,
        None,
        true,
        None,
        None,
        HostInputs::with_home(Some(tmp_home.path())),
    )
    .expect("the launch prepares");

    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        settings["outputStyle"], "tm-demo-01",
        "the launch applies the manifest's style"
    );

    let report = crate::core::output_style::describe_effective_style(&fw.root, project);
    assert!(
        report.starts_with("output style: tm-demo-01 (project) + floor, file "),
        "the report names the style the launch wrote: {report}"
    );
    assert!(!report.contains("warning:"), "{report}");
}
