use super::*;

fn write_plugin_manifest(plugins_dir: &Path, plugin_dir_name: &str, manifest_json: &str) {
    let dir = plugins_dir.join(plugin_dir_name).join(".claude-plugin");
    std::fs::create_dir_all(&dir).expect("mkdir manifest dir");
    std::fs::write(dir.join("plugin.json"), manifest_json).expect("write manifest");
}

/// A missing `.claude/plugins/` directory yields an empty list, not an
/// error — most projects use no plugins.
///
/// Why: mirrors `agents::discover_agents_missing_dir_is_empty`'s
/// graceful-absence contract for the plugin scan root.
/// Test: this test.
#[test]
fn discover_plugin_roots_missing_dir_is_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(discover_plugin_roots(tmp.path()).is_empty());
}

/// A plugin subdirectory with no manifest falls back to its directory name
/// and the `agents`/`skills` convention.
///
/// Why: pins the "no manifest" path #3539 requires — plugin discovery must
/// not require a `.claude-plugin/plugin.json`.
/// What: one bare subdirectory under `.claude/plugins/`, no manifest file;
/// asserts the resolved name equals the directory name and the agents/skills
/// dirs follow the convention.
/// Test: this test.
#[test]
fn discover_plugin_roots_falls_back_without_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    std::fs::create_dir_all(plugins_dir.join("my-plugin")).expect("mkdir");

    let roots = discover_plugin_roots(tmp.path());
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].name, "my-plugin");
    assert_eq!(roots[0].agents_dir, plugins_dir.join("my-plugin/agents"));
    assert_eq!(roots[0].skills_dir, plugins_dir.join("my-plugin/skills"));
}

/// A manifest's `name:` overrides the directory name.
///
/// Why: #3539 — "honor its `name`" is the manifest's whole point when the
/// directory name is not the identity a plugin author wants surfaced.
/// Test: this test.
#[test]
fn discover_plugin_roots_honors_manifest_name_override() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    write_plugin_manifest(&plugins_dir, "on-disk-dir", r#"{"name": "pretty-name"}"#);

    let roots = discover_plugin_roots(tmp.path());
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].name, "pretty-name");
}

/// A manifest's `agents`/`skills` keys override the default subdirectory
/// convention.
///
/// Why: #3539 explicitly locks in "the `agents`/`skills` path overrides".
/// What: the override targets must actually exist on disk — `safe_plugin_subdir`
/// (#3547) requires the joined candidate to canonicalize, since a
/// non-existent path can't be verified as contained within `plugin_dir`.
/// Test: this test.
#[test]
fn discover_plugin_roots_honors_path_overrides() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    write_plugin_manifest(
        &plugins_dir,
        "custom-layout",
        r#"{"name": "custom", "agents": "my-agents", "skills": "my-skills"}"#,
    );
    std::fs::create_dir_all(plugins_dir.join("custom-layout").join("my-agents")).expect("mkdir");
    std::fs::create_dir_all(plugins_dir.join("custom-layout").join("my-skills")).expect("mkdir");

    let roots = discover_plugin_roots(tmp.path());
    assert_eq!(roots.len(), 1);
    assert_eq!(
        roots[0].agents_dir,
        plugins_dir.join("custom-layout/my-agents")
    );
    assert_eq!(
        roots[0].skills_dir,
        plugins_dir.join("custom-layout/my-skills")
    );
}

/// An absolute-path `agents` override in `plugin.json` does NOT escape the
/// plugin root — it is rejected and falls back to the default `agents`
/// convention (code-critic PR #3547 review, CRITICAL 1).
///
/// Why: `PathBuf::join` replaces the base entirely when the joined
/// component is absolute — `plugin_dir.join("/etc")` == `/etc` — so an
/// unvalidated override would let a malicious `plugin.json` point
/// discovery at an arbitrary host directory (here, a decoy "victim" dir
/// standing in for something like `~/.ssh`).
/// What: asserts the resolved `agents_dir` is NOT the absolute victim path,
/// and IS the plugin-root-relative default instead.
/// Test: this test.
#[test]
fn load_plugin_root_rejects_absolute_agents_override() {
    crate::test_support::begin_capture();

    let tmp = tempfile::tempdir().expect("tempdir");
    let victim_dir = tmp.path().join("victim");
    std::fs::create_dir_all(&victim_dir).expect("mkdir victim");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    let manifest_json = format!(r#"{{"name": "evil", "agents": {victim_dir:?}}}"#);
    write_plugin_manifest(&plugins_dir, "evil-plugin", &manifest_json);

    let roots = discover_plugin_roots(tmp.path());
    assert_eq!(roots.len(), 1);
    assert_ne!(
        roots[0].agents_dir, victim_dir,
        "an absolute override must never be honored"
    );
    assert_eq!(
        roots[0].agents_dir,
        plugins_dir.join("evil-plugin").join("agents"),
        "must fall back to the default 'agents' convention"
    );

    let captured = crate::test_support::captured_at_least(tracing::Level::WARN);
    assert!(
        captured.iter().any(|m| m.contains("absolute")),
        "expected a rejection warning, got: {captured:?}"
    );
}

/// A `..`-bearing `skills` override in `plugin.json` does NOT escape the
/// plugin root (code-critic PR #3547 review, CRITICAL 1).
///
/// Test: this test.
#[test]
fn load_plugin_root_rejects_dotdot_skills_override() {
    crate::test_support::begin_capture();

    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    write_plugin_manifest(
        &plugins_dir,
        "evil-plugin",
        r#"{"name": "evil", "skills": "../../../../etc"}"#,
    );

    let roots = discover_plugin_roots(tmp.path());
    assert_eq!(roots.len(), 1);
    assert_eq!(
        roots[0].skills_dir,
        plugins_dir.join("evil-plugin").join("skills"),
        "must fall back to the default 'skills' convention"
    );
    assert!(
        !roots[0].skills_dir.to_string_lossy().contains(".."),
        "the resolved path must never contain a literal '..' escape"
    );

    let captured = crate::test_support::captured_at_least(tracing::Level::WARN);
    assert!(
        captured.iter().any(|m| m.contains("..")),
        "expected a rejection warning, got: {captured:?}"
    );
}

/// [`is_valid_namespaced_name`] accepts exactly two safe segments.
///
/// Test: this test.
#[test]
fn is_valid_namespaced_name_accepts_two_safe_segments() {
    assert!(is_valid_namespaced_name("my-plugin:reviewer"));
    assert!(is_valid_namespaced_name("a:b"));
}

/// [`is_valid_namespaced_name`] rejects a local segment containing `..` or
/// `/` — the exact traversal payload shape (code-critic PR #3547 review,
/// CRITICAL 2).
///
/// Test: this test.
#[test]
fn is_valid_namespaced_name_rejects_traversal_segment() {
    assert!(!is_valid_namespaced_name("foo:../../../../etc/passwd"));
    assert!(!is_valid_namespaced_name("foo:..veryless"));
    assert!(!is_valid_namespaced_name("foo:a/b"));
    assert!(!is_valid_namespaced_name("../../etc:foo"));
}

/// [`is_valid_namespaced_name`] rejects an absolute-path-shaped segment.
///
/// Test: this test.
#[test]
fn is_valid_namespaced_name_rejects_absolute_segment() {
    assert!(!is_valid_namespaced_name("foo:/etc/passwd"));
}

/// [`is_valid_namespaced_name`] rejects a plain (unnamespaced) name — it is
/// not this validator's job to accept those, callers fall through to their
/// existing plain-name path instead.
///
/// Test: this test.
#[test]
fn is_valid_namespaced_name_rejects_plain_name() {
    assert!(!is_valid_namespaced_name("engineer"));
    assert!(!is_valid_namespaced_name(""));
}

/// [`is_valid_namespaced_name`] rejects more than one colon.
///
/// Test: this test.
#[test]
fn is_valid_namespaced_name_rejects_extra_colon() {
    assert!(!is_valid_namespaced_name("foo:bar:baz"));
    assert!(!is_valid_namespaced_name("foo:bar:baz:qux"));
}

/// A malformed `plugin.json` degrades to the no-manifest fallback rather
/// than erroring or panicking.
///
/// Why: discovery must never crash the harness over one plugin's bad JSON.
/// Test: this test.
#[test]
fn discover_plugin_roots_malformed_manifest_falls_back() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    write_plugin_manifest(&plugins_dir, "broken", "{not valid json");

    let roots = discover_plugin_roots(tmp.path());
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].name, "broken");
}

/// Multiple plugins are all discovered, sorted by resolved name.
///
/// Test: this test.
#[test]
fn discover_plugin_roots_finds_subdirs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    std::fs::create_dir_all(plugins_dir.join("zeta")).expect("mkdir");
    std::fs::create_dir_all(plugins_dir.join("alpha")).expect("mkdir");

    let roots = discover_plugin_roots(tmp.path());
    let names: Vec<&str> = roots.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "zeta"], "sorted by resolved name");
}

/// A manifest declaring a later-phase key (`hooks`, `commands`,
/// `mcpServers`) is still ingested for agents/skills — the unsupported key
/// is logged, never a hard failure.
///
/// Why: #3539 — "ignore commands/hooks with a debug log if present".
/// Test: this test.
#[test]
fn discover_plugin_roots_warns_on_later_phase_keys() {
    crate::test_support::begin_capture();

    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    write_plugin_manifest(
        &plugins_dir,
        "full-featured",
        r#"{"name": "full", "hooks": {"pre": "x"}, "commands": ["y"]}"#,
    );

    let roots = discover_plugin_roots(tmp.path());
    assert_eq!(
        roots.len(),
        1,
        "plugin is still discovered despite later-phase keys"
    );
    assert_eq!(roots[0].name, "full");

    let captured = crate::test_support::captured_at_least(tracing::Level::DEBUG);
    let line = captured
        .iter()
        .find(|m| m.contains("later-phase") || m.contains("Phase 1 ingests agents+skills only"))
        .unwrap_or_else(|| panic!("expected a later-phase-keys debug log, got: {captured:?}"));
    assert!(line.contains("hooks"), "got: {line}");
    assert!(line.contains("commands"), "got: {line}");
}

/// [`project_root_two_levels_up`] recovers `<root>` from `<root>/.claude/agents`.
///
/// Test: this test.
#[test]
fn project_root_two_levels_up_recovers_root() {
    let root = Path::new("/fake/project");
    let dir = root.join(".claude").join("agents");
    assert_eq!(project_root_two_levels_up(&dir), Some(root.to_path_buf()));
}

/// A path with fewer than two ancestors yields `None` rather than panicking.
///
/// Test: this test.
#[test]
fn project_root_two_levels_up_none_when_too_shallow() {
    assert_eq!(project_root_two_levels_up(Path::new("agents")), None);
    assert_eq!(project_root_two_levels_up(Path::new("/")), None);
}
