//! #8533: the launch and `tm sessions instructions` select one output style.
//!
//! Why: the report assembled its own precedence chain without the manifest
//! tier, so a project whose only style source was the harness manifest saw the
//! report name the default style while the launch wrote the manifest's.
//! What: sets a style ONLY in the project manifest, runs the real launch, and
//! compares the `outputStyle` it wrote with what the report names. A second
//! test reads what a bare `claude` launch would load after a tm launch; a
//! third makes the composite unwritable and checks the launch falls back.
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
        settings["outputStyle"], "tm-demo-01.tm-floor",
        "the launch applies the manifest's style, through its composite"
    );

    let report = crate::core::output_style::describe_effective_style(&fw.root, project);
    assert!(
        report.starts_with("output style: tm-demo-01 (project) + floor, file "),
        "the report names the style the launch wrote: {report}"
    );
    assert!(!report.contains("warning:"), "{report}");
}

#[test]
#[serial_test::serial]
fn a_bare_claude_launch_loads_the_project_prose_then_the_floor() {
    // #8533 critic HIGH: a bare `claude` reads the style `outputStyle` names
    // and no appended prompt. Naming the project style file itself lost the
    // floor there; the launch now names a composite of prose and floor.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    std::fs::write(
        project.join(".trusty-mpm.toml"),
        "[style]\nactive = \"tm-demo-01\"\n",
    )
    .unwrap();
    let styles = project.join(crate::core::output_style::PROJECT_STYLES_DIR);
    std::fs::create_dir_all(&styles).unwrap();
    std::fs::write(
        styles.join("tm-demo-01.md"),
        "---\nname: tm-demo-01\ndescription: Demo voice\n---\nSpeak as the fixture voice.\n",
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

    // What a bare `claude` in this project loads: the settings' style name,
    // resolved to the file of that name under `.claude/output-styles/`.
    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    let named = settings["outputStyle"].as_str().expect("an outputStyle");
    let file = std::fs::read_to_string(styles.join(format!("{named}.md")))
        .expect("the named style file exists");
    assert!(file.contains(&format!("\nname: {named}\n")), "{file}");
    assert!(file.contains("\ndescription: Demo voice\n"), "{file}");
    let prose = file
        .find("Speak as the fixture voice.")
        .expect("the project prose");
    let floor = file
        .find(&crate::core::output_style::style_floor())
        .expect("the floor");
    assert!(prose < floor, "the floor follows the prose: {file}");

    // The tm launch reads the same composite, so its appended prompt adds no
    // second floor.
    let stash = std::fs::read_to_string(
        crate::core::harness_root::harness_dir(project).join("last-instructions.md"),
    )
    .unwrap();
    assert!(!stash.contains(crate::core::output_style::STYLE_FLOOR_HEADING));
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn an_unwritable_composite_names_the_default_style_and_keeps_the_floor() {
    // #8533 critic round 3 LOW: a composite that cannot be written must not
    // leave `outputStyle` naming a missing file, must be announced, and must
    // leave the floor in the appended prompt.
    use std::os::unix::fs::PermissionsExt as _;

    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    std::fs::write(
        project.join(".trusty-mpm.toml"),
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
    std::fs::set_permissions(&styles, std::fs::Permissions::from_mode(0o555)).unwrap();
    // Root writes into a read-only directory regardless; skip rather than
    // assert a false property.
    let probe = styles.join("probe");
    if std::fs::write(&probe, "").is_ok() {
        let _ = std::fs::remove_file(&probe);
        std::fs::set_permissions(&styles, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let report = prepare_session_inner(
        &fw,
        project,
        None,
        true,
        None,
        None,
        HostInputs::with_home(Some(tmp_home.path())),
    );
    std::fs::set_permissions(&styles, std::fs::Permissions::from_mode(0o755)).unwrap();
    let report = report.expect("the launch prepares");

    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(settings["outputStyle"], "trusty-mpm");
    assert!(
        report
            .asset_notices
            .iter()
            .any(|n| n.contains("tm-demo-01") && n.contains("cannot write its composite")),
        "{:?}",
        report.asset_notices
    );
    let stash = std::fs::read_to_string(
        crate::core::harness_root::harness_dir(project).join("last-instructions.md"),
    )
    .unwrap();
    assert!(stash.contains(crate::core::output_style::STYLE_FLOOR_HEADING));
}
