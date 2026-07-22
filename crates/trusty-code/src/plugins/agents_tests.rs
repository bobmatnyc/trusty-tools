use super::*;

fn write_plugin_agent(plugins_dir: &Path, plugin: &str, agent_name: &str, content: &str) {
    let dir = plugins_dir.join(plugin).join("agents");
    std::fs::create_dir_all(&dir).expect("mkdir agents dir");
    std::fs::write(dir.join(format!("{agent_name}.md")), content).expect("write agent");
}

/// Discovery namespaces every plugin agent `<plugin>:<name>` and projects
/// its fields.
///
/// Why: this is the acceptance criterion for #3539's namespacing decision.
/// Test: this test.
#[test]
fn discover_plugin_agents_namespaces_entries() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    write_plugin_agent(
        &plugins_dir,
        "my-plugin",
        "reviewer",
        "---\nname: reviewer\ndescription: Reviews code\nmodel: sonnet\n---\n\nYou review code.\n",
    );

    let agents = discover_plugin_agents(tmp.path());
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].agent.name, "my-plugin:reviewer");
    assert_eq!(agents[0].agent.description.as_deref(), Some("Reviews code"));
    assert_eq!(agents[0].agent.model.as_deref(), Some("sonnet"));
    assert_eq!(agents[0].system_prompt.content, "You review code.");
}

/// A plugin agent literally named a `BASE-*` template name is excluded from
/// discovery, exactly like the embedded/disk tiers (#3539's base-filter
/// interaction clause).
///
/// Test: this test.
#[test]
fn discover_plugin_agents_excludes_base_named_agent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    write_plugin_agent(
        &plugins_dir,
        "my-plugin",
        "base-engineer",
        "---\nname: base-engineer\n---\n\nTemplate only.\n",
    );
    write_plugin_agent(
        &plugins_dir,
        "my-plugin",
        "real-agent",
        "---\nname: real-agent\n---\n\nBody.\n",
    );

    let agents = discover_plugin_agents(tmp.path());
    let names: Vec<&str> = agents.iter().map(|a| a.agent.name.as_str()).collect();
    assert_eq!(names, vec!["my-plugin:real-agent"]);
}

/// A plugin agent that fails to load (unreadable/malformed) is skipped with
/// a warning, never aborting the whole scan.
///
/// Test: this test.
#[test]
fn discover_plugin_agents_skips_unparseable_agent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    // A directory named `broken.md` (unreadable as a file) exercises the
    // read failure path without depending on filesystem permissions.
    std::fs::create_dir_all(
        plugins_dir
            .join("my-plugin")
            .join("agents")
            .join("broken.md"),
    )
    .expect("mkdir");
    write_plugin_agent(
        &plugins_dir,
        "my-plugin",
        "good-agent",
        "---\nname: good-agent\n---\n\nBody.\n",
    );

    let agents = discover_plugin_agents(tmp.path());
    let names: Vec<&str> = agents.iter().map(|a| a.agent.name.as_str()).collect();
    assert_eq!(names, vec!["my-plugin:good-agent"]);
}

/// [`load_plugin_agent`] projects every supported field.
///
/// Test: this test.
#[test]
fn load_plugin_agent_projects_fields() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("solo.md");
    std::fs::write(
        &path,
        "---\nname: solo\nrole: engineer\ndescription: A lone agent\nmodel: sonnet\nmax_tokens: 4096\ntools: [read_file]\n---\n\nBody text.\n",
    )
    .expect("write");

    let cfg = load_plugin_agent("plug", "solo", &path, tmp.path()).expect("load");
    assert_eq!(cfg.agent.name, "plug:solo");
    assert_eq!(cfg.agent.role.as_deref(), Some("engineer"));
    assert_eq!(cfg.agent.description.as_deref(), Some("A lone agent"));
    assert_eq!(cfg.agent.model.as_deref(), Some("sonnet"));
    assert_eq!(cfg.llm.max_tokens, Some(4096));
    assert_eq!(cfg.system_prompt.content, "Body text.");
    assert_eq!(
        cfg.tools.and_then(|t| t.allowed),
        Some(vec!["read_file".to_string()])
    );
}

/// Unsupported trusty-mpm-style frontmatter fields
/// (`effort`/`maxTurns`/`memory`/`isolation`/`disallowedTools`) are dropped
/// with one aggregated warning, never a load failure.
///
/// Why: this is the acceptance criterion for #3539's "DROP unsupported
/// plugin fields ... with a one-line warn per agent" requirement.
/// Test: this test.
#[test]
fn load_plugin_agent_warns_and_drops_unsupported_fields() {
    crate::test_support::begin_capture();

    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("fancy.md");
    std::fs::write(
        &path,
        "---\nname: fancy\neffort: high\nmaxTurns: 10\nmemory: enabled\nisolation: worktree\ndisallowedTools: [Bash]\n---\n\nBody.\n",
    )
    .expect("write");

    let cfg =
        load_plugin_agent("plug", "fancy", &path, tmp.path()).expect("load must still succeed");
    assert_eq!(cfg.agent.name, "plug:fancy");

    let captured = crate::test_support::captured_at_least(tracing::Level::WARN);
    let warning = captured
        .iter()
        .find(|m| m.contains("unsupported field"))
        .unwrap_or_else(|| panic!("expected an unsupported-field warning, got: {captured:?}"));
    assert!(warning.contains("plug:fancy"), "got: {warning}");
    assert!(warning.contains("effort"), "got: {warning}");
    assert!(warning.contains("maxTurns"), "got: {warning}");
    assert!(warning.contains("memory"), "got: {warning}");
    assert!(warning.contains("isolation"), "got: {warning}");
    assert!(warning.contains("disallowedTools"), "got: {warning}");
}

/// An `extends:` chain on a plugin agent is warned-and-ignored — the agent
/// still loads as a leaf, its own body only, with no parent content pulled
/// in.
///
/// Why: #3539 — "a plugin agent with extends: -> warn + treat as direct".
/// Test: this test.
#[test]
fn load_plugin_agent_warns_on_extends_and_treats_as_leaf() {
    crate::test_support::begin_capture();

    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("child.md");
    std::fs::write(
        &path,
        "---\nname: child\nextends: some-base\n---\n\nOnly my own body.\n",
    )
    .expect("write");

    let cfg =
        load_plugin_agent("plug", "child", &path, tmp.path()).expect("load must still succeed");
    assert_eq!(cfg.system_prompt.content, "Only my own body.");

    let captured = crate::test_support::captured_at_least(tracing::Level::WARN);
    let warning = captured
        .iter()
        .find(|m| m.contains("extends"))
        .unwrap_or_else(|| panic!("expected an extends warning, got: {captured:?}"));
    assert!(warning.contains("plug:child"), "got: {warning}");
    assert!(warning.contains("some-base"), "got: {warning}");
}

/// A missing plugin agent file surfaces a descriptive error, never a panic.
///
/// Test: this test.
#[test]
fn load_plugin_agent_missing_file_errors() {
    let result = load_plugin_agent(
        "plug",
        "ghost",
        Path::new("/nonexistent/ghost.md"),
        Path::new("/nonexistent"),
    );
    assert!(result.is_err());
}

/// A plugin agent file that is (or resolves through) a symlink escaping
/// `agents_dir` is rejected — the secret content it would otherwise expose
/// is planted at the escape target and asserted never read (code-critic PR
/// #3547 re-review, CRITICAL 5, CWE-59).
///
/// Why: this is the exact repro shape the re-review flagged — a real
/// `agents/leak.md` symlinked to an arbitrary host file (`~/.ssh/id_rsa` in
/// the wild; a planted "secret" file here).
/// Test: this test.
#[test]
#[cfg(unix)]
fn load_plugin_agent_rejects_symlinked_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let agents_dir = tmp.path().join("agents");
    std::fs::create_dir_all(&agents_dir).expect("mkdir");

    let secret_dir = tmp.path().join("outside");
    std::fs::create_dir_all(&secret_dir).expect("mkdir");
    let secret_path = secret_dir.join("id_rsa");
    std::fs::write(
        &secret_path,
        "-----BEGIN OPENSSH PRIVATE KEY-----\nSECRET\n",
    )
    .expect("write secret");

    let leak_path = agents_dir.join("leak.md");
    std::os::unix::fs::symlink(&secret_path, &leak_path).expect("create symlink");

    let result = load_plugin_agent("plug", "leak", &leak_path, &agents_dir);
    let err = result.expect_err("a symlinked leaf file must be rejected");
    let msg = format!("{err:#}");
    assert!(
        !msg.contains("SECRET"),
        "the secret content must never appear anywhere, even in an error message, got: {msg}"
    );
}

/// [`find_plugin_agent_config`] resolves a known plugin/agent pair.
///
/// Test: this test.
#[test]
fn find_plugin_agent_config_resolves_known_agent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    write_plugin_agent(
        &plugins_dir,
        "my-plugin",
        "reviewer",
        "---\nname: reviewer\n---\n\nBody.\n",
    );

    let result = find_plugin_agent_config(tmp.path(), "my-plugin", "reviewer");
    let cfg = result.expect("must find").expect("must load");
    assert_eq!(cfg.agent.name, "my-plugin:reviewer");
}

/// An unknown plugin name resolves to `None` (not found), never an error.
///
/// Test: this test.
#[test]
fn find_plugin_agent_config_unknown_plugin_is_none() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(find_plugin_agent_config(tmp.path(), "no-such-plugin", "reviewer").is_none());
}

/// A known plugin but an unknown agent name resolves to `None`.
///
/// Test: this test.
#[test]
fn find_plugin_agent_config_unknown_agent_is_none() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    write_plugin_agent(
        &plugins_dir,
        "my-plugin",
        "reviewer",
        "---\nname: reviewer\n---\n\nBody.\n",
    );

    assert!(find_plugin_agent_config(tmp.path(), "my-plugin", "ghost").is_none());
}

/// A traversal payload in the local `agent_name` segment is rejected
/// BEFORE any path is built — even when a file that would satisfy the
/// naive `<dir>/<name>.md` join actually exists on disk outside the
/// plugin's `agents_dir` (code-critic PR #3547 review, HIGH 3).
///
/// Why: this is the exact repro shape the review flagged —
/// `agents_dir.join(format!("{agent_name}.md"))` built directly from a
/// caller-supplied segment. Planting a REAL file at the escape target and
/// asserting it is never read proves the guard fires before the
/// filesystem is ever touched with the unsafe path, not merely that the
/// (possibly nonexistent) target happens to miss.
/// Test: this test.
#[test]
fn find_plugin_agent_config_rejects_traversal_name() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    write_plugin_agent(
        &plugins_dir,
        "my-plugin",
        "reviewer",
        "---\nname: reviewer\n---\n\nBody.\n",
    );
    // A real file at the traversal target — if the guard did not fire, this
    // is exactly what `agents_dir.join("../../secret.md")` would resolve to
    // and successfully load.
    std::fs::write(
        tmp.path().join("secret.md"),
        "---\nname: secret\n---\n\nSHOULD NEVER BE READ.\n",
    )
    .expect("write secret");

    assert!(
        find_plugin_agent_config(tmp.path(), "my-plugin", "../../secret").is_none(),
        "a traversal payload in agent_name must be rejected, not resolved"
    );
    assert!(
        find_plugin_agent_config(tmp.path(), "my-plugin", "/etc/passwd").is_none(),
        "an absolute-path-shaped agent_name must be rejected"
    );
}

/// `discover_plugin_agents` (the `agents.list` listing path) excludes a
/// symlinked plugin agent file — a real, legitimate agent alongside it is
/// still listed, but the symlinked one, and the secret content it points
/// at, never appear (code-critic PR #3547 re-review, CRITICAL 5, CWE-59).
///
/// Test: this test.
#[test]
#[cfg(unix)]
fn discover_plugin_agents_excludes_symlinked_leak_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    write_plugin_agent(
        &plugins_dir,
        "my-plugin",
        "reviewer",
        "---\nname: reviewer\n---\n\nBody.\n",
    );

    let secret_dir = tmp.path().join("outside");
    std::fs::create_dir_all(&secret_dir).expect("mkdir");
    let secret_path = secret_dir.join("id_rsa");
    std::fs::write(&secret_path, "SECRET_KEY_MATERIAL").expect("write secret");
    let leak_path = plugins_dir.join("my-plugin").join("agents").join("leak.md");
    std::os::unix::fs::symlink(&secret_path, &leak_path).expect("create symlink");

    let agents = discover_plugin_agents(tmp.path());
    let names: Vec<&str> = agents.iter().map(|a| a.agent.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["my-plugin:reviewer"],
        "the symlinked entry must be excluded; the real agent must still be listed"
    );
    assert!(
        agents
            .iter()
            .all(|a| !a.system_prompt.content.contains("SECRET_KEY_MATERIAL")),
        "the secret content must never appear in any listed agent's body"
    );
}

/// `find_plugin_agent_config` (the dispatch/`delegate_to_agent` resolution
/// path) rejects a symlinked plugin agent file, never surfacing the target
/// file's content (code-critic PR #3547 re-review, CRITICAL 5, CWE-59).
///
/// Test: this test.
#[test]
#[cfg(unix)]
fn find_plugin_agent_config_rejects_symlinked_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join(".claude").join("plugins");
    std::fs::create_dir_all(plugins_dir.join("my-plugin").join("agents")).expect("mkdir");

    let secret_dir = tmp.path().join("outside");
    std::fs::create_dir_all(&secret_dir).expect("mkdir");
    let secret_path = secret_dir.join("id_rsa");
    std::fs::write(&secret_path, "SECRET_KEY_MATERIAL").expect("write secret");
    let leak_path = plugins_dir.join("my-plugin").join("agents").join("leak.md");
    std::os::unix::fs::symlink(&secret_path, &leak_path).expect("create symlink");

    match find_plugin_agent_config(tmp.path(), "my-plugin", "leak") {
        None => {}
        Some(Ok(cfg)) => panic!(
            "a symlinked agent must never resolve successfully, got: {:?}",
            cfg.system_prompt.content
        ),
        Some(Err(e)) => {
            let msg = format!("{e:#}");
            assert!(
                !msg.contains("SECRET_KEY_MATERIAL"),
                "the secret content must never leak into the error message, got: {msg}"
            );
        }
    }
}
