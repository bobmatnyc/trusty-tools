//! Unit tests for the default-deny session MCP scope (#7422).
//!
//! Why: the whole point of the module is what it LEAVES OUT, and an omission is
//! invisible unless a test names the server that must not appear. Each fixture
//! therefore plants a shared-only server and asserts on both halves — what the
//! composed map holds and what `excluded` reports.
//! What: scope composition, the two fail-arms, the flag renderers, and the
//! project-config opt-in reader.
//! Test: this file.

use std::path::Path;

use serde_json::json;
use tempfile::TempDir;

use super::*;

/// Write a tm-managed `.claude.json` holding `names` as stdio servers.
fn shared_config(dir: &Path, names: &[&str]) {
    let mut servers = serde_json::Map::new();
    for name in names {
        servers.insert(
            (*name).to_string(),
            json!({"type": "stdio", "command": name, "args": []}),
        );
    }
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(".claude.json"),
        serde_json::to_string_pretty(&json!({"mcpServers": servers})).unwrap(),
    )
    .unwrap();
}

/// Write a project `.mcp.json` holding `names` as stdio servers.
fn project_mcp_json(dir: &Path, names: &[&str]) {
    let mut servers = serde_json::Map::new();
    for name in names {
        servers.insert(
            (*name).to_string(),
            json!({"type": "stdio", "command": name, "args": []}),
        );
    }
    std::fs::write(
        dir.join(".mcp.json"),
        serde_json::to_string_pretty(&json!({"mcpServers": servers})).unwrap(),
    )
    .unwrap();
}

#[test]
fn session_mcp_path_is_per_workspace() {
    let a = session_mcp_path(Path::new("/repo/.claude/worktrees/wt-1"));
    let b = session_mcp_path(Path::new("/repo"));
    assert_ne!(a, b, "two worktrees must not share one composed file");
    assert!(a.ends_with(".trusty-mpm/session-mcp.json"), "{a:?}");
}

#[test]
fn scoped_for_declines_a_non_relocated_spawn() {
    assert_eq!(scoped_for(Path::new("/repo"), None), None);
    assert!(scoped_for(Path::new("/repo"), Some(Path::new("/cfg"))).is_some());
}

#[test]
fn strict_mcp_flag_string_quotes_the_path() {
    let rendered = strict_mcp_flag_string(Some(Path::new("/a dir/session-mcp.json")));
    assert_eq!(
        rendered, " --strict-mcp-config --mcp-config '/a dir/session-mcp.json'",
        "the pane shell re-splits this line, so the path must be quoted"
    );
}

#[test]
fn strict_mcp_flag_string_is_empty_without_a_file() {
    assert_eq!(strict_mcp_flag_string(None), "");
}

#[test]
fn strict_mcp_argv_is_three_unquoted_tokens() {
    let argv = strict_mcp_argv(Some(Path::new("/a dir/session-mcp.json")));
    assert_eq!(
        argv,
        vec![
            "--strict-mcp-config".to_owned(),
            "--mcp-config".to_owned(),
            "/a dir/session-mcp.json".to_owned(),
        ],
        "exec takes the path verbatim — quoting would name a file claude cannot open"
    );
    assert!(strict_mcp_argv(None).is_empty());
}

#[test]
fn resolve_scope_includes_builtins_and_project_servers() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("repo");
    let cfg = tmp.path().join("cfg");
    std::fs::create_dir_all(&cwd).unwrap();
    shared_config(&cfg, &[]);
    project_mcp_json(&cwd, &["project-only"]);

    let scope = resolve_scope(&cwd, &cfg);

    for builtin in crate::core::mcp_config::BUILTIN_MANAGED_MCP_SERVERS {
        assert!(
            scope.servers.contains_key(*builtin),
            "builtin {builtin} must always load: {:?}",
            scope.included
        );
    }
    assert!(scope.servers.contains_key("project-only"));
    assert!(scope.excluded.is_empty(), "{:?}", scope.excluded);
    assert_eq!(scope.degraded, None);
}

#[test]
fn resolve_scope_excludes_a_shared_only_server() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("repo");
    let cfg = tmp.path().join("cfg");
    std::fs::create_dir_all(&cwd).unwrap();
    shared_config(&cfg, &["slack-mcp", "gworkspace-mcp", "trusty-memory"]);

    let scope = resolve_scope(&cwd, &cfg);

    assert!(
        !scope.servers.contains_key("slack-mcp"),
        "a shared-only server must not load without an opt-in"
    );
    assert_eq!(
        scope.excluded,
        vec!["gworkspace-mcp".to_owned(), "slack-mcp".to_owned()],
        "both shared-only servers are reported; the builtin is not"
    );
    assert!(scope.servers.contains_key("trusty-memory"));
}

#[test]
fn resolve_scope_includes_an_opted_in_shared_server() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("repo");
    let cfg = tmp.path().join("cfg");
    std::fs::create_dir_all(&cwd).unwrap();
    shared_config(&cfg, &["slack-mcp", "heygen"]);
    std::fs::write(
        cwd.join(crate::core::project_config::PROJECT_CONFIG_FILE),
        "[session]\nmcp_servers = [\"slack-mcp\"]\n",
    )
    .unwrap();

    let scope = resolve_scope(&cwd, &cfg);

    assert!(scope.servers.contains_key("slack-mcp"));
    assert_eq!(scope.excluded, vec!["heygen".to_owned()]);
}

#[test]
fn resolve_scope_degrades_on_a_malformed_project_mcp_json() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("repo");
    let cfg = tmp.path().join("cfg");
    std::fs::create_dir_all(&cwd).unwrap();
    shared_config(&cfg, &["slack-mcp"]);
    std::fs::write(cwd.join(".mcp.json"), "{ not json").unwrap();

    let scope = resolve_scope(&cwd, &cfg);

    assert!(
        scope.degraded.is_some(),
        "a project .mcp.json that will not parse must be reported"
    );
    for builtin in crate::core::mcp_config::BUILTIN_MANAGED_MCP_SERVERS {
        assert!(scope.servers.contains_key(*builtin));
    }
    assert!(
        !scope.servers.contains_key("slack-mcp"),
        "degrading must never widen the set"
    );
}

#[test]
fn shared_servers_tolerates_a_malformed_config() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join(".claude.json"), "{ not json").unwrap();

    assert!(shared_servers(&cfg).is_empty());
    assert!(
        cfg.join(".claude.json").exists(),
        "the launch path must never quarantine the file that holds OAuth state"
    );
}

#[test]
fn opt_in_servers_is_empty_without_a_config() {
    let tmp = TempDir::new().unwrap();
    assert!(opt_in_servers(tmp.path()).is_empty());
    assert!(opt_in_plugins(tmp.path()).is_empty());
}

#[test]
fn opt_in_servers_reads_the_project_config() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path()
            .join(crate::core::project_config::PROJECT_CONFIG_FILE),
        "[session]\nmcp_servers = [\"apex\", \"slack-mcp\"]\n",
    )
    .unwrap();
    assert_eq!(
        opt_in_servers(tmp.path()),
        vec!["apex".to_owned(), "slack-mcp".to_owned()]
    );
}

#[test]
fn opt_in_plugins_reads_the_project_config() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path()
            .join(crate::core::project_config::PROJECT_CONFIG_FILE),
        "[session]\nplugins = [\"aws-core\"]\n",
    )
    .unwrap();
    assert_eq!(opt_in_plugins(tmp.path()), vec!["aws-core".to_owned()]);
}

#[test]
fn provision_writes_the_composed_map() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("repo");
    let cfg = tmp.path().join("cfg");
    std::fs::create_dir_all(&cwd).unwrap();
    shared_config(&cfg, &["slack-mcp"]);

    let path = provision(&cwd, &cfg).expect("provision succeeds");

    assert_eq!(path, session_mcp_path(&cwd));
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let servers = written["mcpServers"].as_object().unwrap();
    assert!(servers.contains_key("trusty-mpm"));
    assert!(
        !servers.contains_key("slack-mcp"),
        "the written file is the default-deny set, not the shared map"
    );
}

#[test]
fn provision_rewrites_on_every_call() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("repo");
    let cfg = tmp.path().join("cfg");
    std::fs::create_dir_all(&cwd).unwrap();
    shared_config(&cfg, &["slack-mcp"]);

    let path = provision(&cwd, &cfg).unwrap();
    std::fs::write(&path, "{\"mcpServers\":{\"stale\":{}}}").unwrap();
    provision(&cwd, &cfg).unwrap();

    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        written["mcpServers"].get("stale").is_none(),
        "a stale set from a previous launch must not survive"
    );
}

#[test]
fn provision_for_spawn_declines_a_non_relocated_spawn() {
    let tmp = TempDir::new().unwrap();
    assert_eq!(provision_for_spawn(tmp.path(), None).unwrap(), None);
    assert!(
        !session_mcp_path(tmp.path()).exists(),
        "a spawn that reads the operator's own config writes nothing"
    );
}

/// The fail-CLOSED arm: an unwritable state dir aborts rather than degrading.
///
/// Why: the only alternative to a composed file is a spawn with no
/// `--mcp-config`, which is the unscoped shared map this module exists to stop.
/// What: plants a regular FILE where `.trusty-mpm/` must be a directory, so
/// `create_dir_all` fails, and asserts the error rather than a silent `Ok`.
/// Test: this test.
#[test]
fn provision_errors_when_the_state_dir_is_unwritable() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("repo");
    let cfg = tmp.path().join("cfg");
    std::fs::create_dir_all(&cwd).unwrap();
    shared_config(&cfg, &[]);
    // A file where the state directory has to be: `create_dir_all` cannot win.
    std::fs::write(cwd.join(SESSION_STATE_DIR), "not a directory").unwrap();

    let err = provision(&cwd, &cfg).expect_err("an unwritable state dir must fail the launch");

    assert!(
        matches!(err, ScopeError::Write { .. }),
        "expected a Write error, got {err:?}"
    );
    assert!(
        err.to_string().contains("refusing to launch"),
        "the message must say why it did not fall back: {err}"
    );
}
