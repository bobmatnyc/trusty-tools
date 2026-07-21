use super::*;

fn write_plugin_skill(plugins_dir: &Path, plugin: &str, skill_dir_name: &str, content: &str) {
    let dir = plugins_dir.join(plugin).join("skills").join(skill_dir_name);
    std::fs::create_dir_all(&dir).expect("mkdir skill dir");
    std::fs::write(dir.join("SKILL.md"), content).expect("write skill");
}

/// Discovery namespaces every plugin skill `<plugin>:<name>` and reads its
/// cheap frontmatter.
///
/// Why: the acceptance criterion for #3539's skill-namespacing decision.
/// Test: this test.
#[test]
fn discover_plugin_skills_namespaces_entries() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    write_plugin_skill(
        &plugins_dir,
        "my-plugin",
        "review-checklist",
        "---\nname: review-checklist\ndescription: A checklist\n---\n\nFull body.\n",
    );

    let skills = discover_plugin_skills(tmp.path());
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "my-plugin:review-checklist");
    assert_eq!(skills[0].description, "A checklist");
}

/// A skill directory name is used as a fallback when frontmatter declares
/// no `name:`.
///
/// Test: this test.
#[test]
fn discover_plugin_skills_falls_back_to_dirname() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    write_plugin_skill(
        &plugins_dir,
        "my-plugin",
        "no-frontmatter",
        "# Just a body\n",
    );

    let skills = discover_plugin_skills(tmp.path());
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "my-plugin:no-frontmatter");
}

/// A skill subdirectory without a readable `SKILL.md` is skipped, not an
/// error.
///
/// Test: this test.
#[test]
fn discover_plugin_skills_skips_dir_without_skill_md() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    std::fs::create_dir_all(
        plugins_dir
            .join("my-plugin")
            .join("skills")
            .join("empty-dir"),
    )
    .expect("mkdir");
    write_plugin_skill(
        &plugins_dir,
        "my-plugin",
        "real-skill",
        "---\nname: real-skill\n---\n\nBody.\n",
    );

    let skills = discover_plugin_skills(tmp.path());
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "my-plugin:real-skill");
}

/// A project with no `.claude/plugins/` directory yields no plugin skills.
///
/// Test: this test.
#[test]
fn discover_plugin_skills_missing_plugins_dir_is_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(discover_plugin_skills(tmp.path()).is_empty());
}

/// [`resolve_plugin_skill_body`] returns the body with frontmatter stripped
/// for a known plugin/skill pair.
///
/// Test: this test.
#[test]
fn resolve_plugin_skill_body_returns_body() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    write_plugin_skill(
        &plugins_dir,
        "my-plugin",
        "demo-skill",
        "---\nname: demo-skill\ndescription: Demo\n---\n\n# Demo Skill\nFull instructions.\n",
    );

    let body = resolve_plugin_skill_body(tmp.path(), "my-plugin", "demo-skill")
        .expect("must resolve body");
    assert!(body.contains("# Demo Skill"));
    assert!(body.contains("Full instructions."));
    assert!(!body.contains("description: Demo"));
}

/// An unknown plugin name resolves to `None`.
///
/// Test: this test.
#[test]
fn resolve_plugin_skill_body_unknown_plugin_is_none() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(resolve_plugin_skill_body(tmp.path(), "no-such-plugin", "demo-skill").is_none());
}

/// A known plugin but an unknown skill name resolves to `None`.
///
/// Test: this test.
#[test]
fn resolve_plugin_skill_body_unknown_skill_is_none() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    write_plugin_skill(
        &plugins_dir,
        "my-plugin",
        "demo-skill",
        "---\nname: demo-skill\n---\n\nBody.\n",
    );

    assert!(resolve_plugin_skill_body(tmp.path(), "my-plugin", "ghost-skill").is_none());
}

/// A traversal payload in `skill_name` — exactly the `use_skill` LLM-input
/// shape (`tools::skill::UseSkillTool::execute` -> `FsSkillResolver::resolve`
/// -> here) — is rejected BEFORE any path is built, even when a real file
/// sits at the escape target outside the plugin's `skills_dir`
/// (code-critic PR #3547 review, CRITICAL 2).
///
/// Why: planting a REAL `SKILL.md` at the traversal target and asserting it
/// is never read proves the guard fires before the filesystem is touched
/// with the unsafe path — not merely that the target happens to miss.
/// Test: this test.
#[test]
fn resolve_plugin_skill_body_rejects_traversal_name() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    write_plugin_skill(
        &plugins_dir,
        "my-plugin",
        "demo-skill",
        "---\nname: demo-skill\n---\n\nBody.\n",
    );
    // A real "secret" SKILL.md at the traversal target — if the guard did
    // not fire, `skills_dir.join("../../../secret").join("SKILL.md")`
    // resolves exactly here.
    let secret_dir = tmp.path().join("secret");
    std::fs::create_dir_all(&secret_dir).expect("mkdir secret");
    std::fs::write(
        secret_dir.join("SKILL.md"),
        "---\nname: secret\n---\n\nSHOULD NEVER BE READ.\n",
    )
    .expect("write secret");

    assert!(
        resolve_plugin_skill_body(tmp.path(), "my-plugin", "../../../secret").is_none(),
        "a traversal payload in skill_name must be rejected, not resolved"
    );
    assert!(
        resolve_plugin_skill_body(tmp.path(), "my-plugin", "/etc/passwd").is_none(),
        "an absolute-path-shaped skill_name must be rejected"
    );
}
