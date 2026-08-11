//! Unit tests for `mcp_config` — split out to keep the production module
//! under the 500-SLOC cap (mirrors the `native_mcp_tests.rs` /
//! `custom_mcp_tests.rs` split pattern already used in this crate).
//! What: CRUD round-trips for the user-scope `mcpServers` map, the
//! `BUILTIN_MANAGED_MCP_SERVERS`/`UNCONDITIONAL_BUILTIN_MCP_SERVERS`/
//! `CONDITIONAL_BUILTIN_MCP_SERVERS` drift guard, and the trust-derivation
//! coverage for [`super::managed_mcp_server_names`],
//! [`super::launch_trusted_mcp_names_from`], and
//! [`super::resolve_conditional_mcp_toggles`] — including the issue #3934
//! regression (a conditional builtin must drop out of the trust list when its
//! injector toggle is off) and the issue #3950 regression (either builtin —
//! conditional OR unconditional — must drop out when its pin write fails,
//! not merely when a toggle is off).
//! Test: this file IS the test module.

use super::*;
use tempfile::TempDir;

fn stdio(command: &str, args: &[&str]) -> Value {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    build_stdio_entry(command, &args, &Map::new())
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn strip_arg_separator_removes_only_leading() {
    // The npx shape: a leading `--` is dropped exactly once.
    let args = strings(&["--", "-y", "@modelcontextprotocol/server-github"]);
    assert_eq!(
        strip_arg_separator(&args),
        &["-y", "@modelcontextprotocol/server-github"]
    );
    // Only the FIRST token is stripped — a second leading `--` survives.
    let doubled = strings(&["--", "--", "-y"]);
    assert_eq!(strip_arg_separator(&doubled), &["--", "-y"]);
}

#[test]
fn strip_arg_separator_preserves_deeper_and_absent() {
    // A `--` that is not the first token is a real argument — keep it.
    let deeper = strings(&["run", "--", "tool"]);
    assert_eq!(strip_arg_separator(&deeper), &["run", "--", "tool"]);
    // No separator at all → unchanged.
    let clean = strings(&["-y", "pkg"]);
    assert_eq!(strip_arg_separator(&clean), &["-y", "pkg"]);
    // Empty slice → empty (no panic).
    let empty: Vec<String> = Vec::new();
    assert!(strip_arg_separator(&empty).is_empty());
}

#[test]
fn build_stdio_entry_has_expected_shape() {
    let e = stdio("echo", &["hi"]);
    assert_eq!(e["type"], "stdio");
    assert_eq!(e["command"], "echo");
    assert_eq!(e["args"], serde_json::json!(["hi"]));
    assert!(e.get("env").is_none(), "env omitted when empty");
}

#[test]
fn build_stdio_entry_with_env() {
    let mut env = Map::new();
    env.insert("API_KEY".into(), Value::String("xxx".into()));
    let e = build_stdio_entry("srv", &[], &env);
    assert_eq!(e["env"]["API_KEY"], "xxx");
    assert_eq!(e["args"], serde_json::json!([]), "args always present");
}

#[test]
fn build_remote_entry_http() {
    let e = build_remote_entry(McpTransport::Http, "https://x/mcp", &Map::new());
    assert_eq!(e["type"], "http");
    assert_eq!(e["url"], "https://x/mcp");
    assert!(e.get("headers").is_none());
    // sse shares the shape with a different discriminant.
    let s = build_remote_entry(McpTransport::Sse, "https://x/sse", &Map::new());
    assert_eq!(s["type"], "sse");
}

#[test]
fn build_remote_entry_with_headers() {
    let mut h = Map::new();
    h.insert("Authorization".into(), Value::String("Bearer t".into()));
    let e = build_remote_entry(McpTransport::Http, "https://x", &h);
    assert_eq!(e["headers"]["Authorization"], "Bearer t");
}

#[test]
fn add_then_get_roundtrips() {
    let tmp = TempDir::new().unwrap();
    // config dir does not exist yet — add must create it + a fresh {}.
    let cfg = tmp.path().join("claude-config");
    assert!(add_server(&cfg, "echo", stdio("echo", &["hi"])).unwrap());
    let got = get_server(&cfg, "echo").unwrap().expect("server present");
    assert_eq!(got["command"], "echo");
    // The file must be valid JSON with a top-level mcpServers map.
    let text = std::fs::read_to_string(cfg.join(CLAUDE_JSON)).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert!(v["mcpServers"]["echo"].is_object());
}

#[test]
fn add_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("cc");
    assert!(add_server(&cfg, "echo", stdio("echo", &["hi"])).unwrap());
    // Second identical add → no change, no rewrite.
    assert!(!add_server(&cfg, "echo", stdio("echo", &["hi"])).unwrap());
}

#[test]
fn add_preserves_other_keys() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("cc");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join(CLAUDE_JSON),
        r#"{"oauthAccount":{"keep":"me"},"mcpServers":{"pre":{"type":"stdio","command":"pre","args":[]}}}"#,
    )
    .unwrap();
    add_server(&cfg, "echo", stdio("echo", &["hi"])).unwrap();
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(cfg.join(CLAUDE_JSON)).unwrap()).unwrap();
    assert_eq!(v["oauthAccount"]["keep"], "me", "unrelated key preserved");
    assert!(v["mcpServers"]["pre"].is_object(), "existing server kept");
    assert!(v["mcpServers"]["echo"].is_object(), "new server added");
}

#[test]
fn remove_existing_returns_true() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("cc");
    add_server(&cfg, "echo", stdio("echo", &["hi"])).unwrap();
    assert!(remove_server(&cfg, "echo").unwrap());
    assert!(get_server(&cfg, "echo").unwrap().is_none());
}

#[test]
fn remove_absent_returns_false() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("cc");
    assert!(!remove_server(&cfg, "nope").unwrap());
}

#[test]
fn get_absent_returns_none() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("cc");
    assert!(get_server(&cfg, "nope").unwrap().is_none());
}

#[test]
fn list_returns_all_added() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("cc");
    add_server(&cfg, "a", stdio("a", &[])).unwrap();
    add_server(
        &cfg,
        "b",
        build_remote_entry(McpTransport::Http, "https://b", &Map::new()),
    )
    .unwrap();
    let all = list_servers(&cfg).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all["a"]["type"], "stdio");
    assert_eq!(all["b"]["type"], "http");
}

#[test]
fn add_quarantines_malformed_json() {
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("cc");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join(CLAUDE_JSON), b"{ not valid json !!!").unwrap();
    // add must succeed despite the corrupt file.
    add_server(&cfg, "echo", stdio("echo", &["hi"])).unwrap();
    // Issue #4206: the quarantine name is TIMESTAMPED
    // (`.claude.json.corrupt-<stamp>`), not the fixed `.claude.json.corrupt`
    // this used to assert — a fixed name let a second quarantine silently
    // overwrite the first one's record. Match the family by prefix.
    let quarantined: Vec<String> = std::fs::read_dir(&cfg)
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|n| n.starts_with(".claude.json.corrupt"))
        .collect();
    assert_eq!(
        quarantined.len(),
        1,
        "corrupt file quarantined under a timestamped name, not deleted: {quarantined:?}"
    );
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(cfg.join(CLAUDE_JSON)).unwrap()).unwrap();
    assert!(v["mcpServers"]["echo"].is_object());
}

#[test]
fn builtin_server_entry_matches_known_servers() {
    // Every name in the builtin list resolves to a stdio launch entry.
    for name in BUILTIN_MANAGED_MCP_SERVERS {
        let e = builtin_server_entry(name).expect("builtin resolves");
        assert_eq!(e["type"], "stdio");
        assert_eq!(e["command"], *name, "command matches the binary name");
        assert!(e["args"].is_array(), "args always present");
    }
    // The exact launch args must stay pinned (parity with ensure_mcp_config).
    assert_eq!(
        builtin_server_entry("trusty-memory").unwrap()["args"],
        serde_json::json!(["serve", "--stdio"])
    );
    assert_eq!(
        builtin_server_entry("trusty-mpm").unwrap()["args"],
        serde_json::json!(["serve", "--stdio"])
    );
    assert_eq!(
        builtin_server_entry("trusty-search").unwrap()["args"],
        serde_json::json!(["serve"])
    );
    // Unknown names yield None.
    assert!(builtin_server_entry("not-a-builtin").is_none());
}

#[test]
fn builtin_mcp_server_split_unions_to_full_set() {
    // Drift guard: UNCONDITIONAL ∪ CONDITIONAL must always equal the full
    // reserved-name set, sorted, with no overlap or omission.
    let mut split: Vec<&str> = UNCONDITIONAL_BUILTIN_MCP_SERVERS
        .iter()
        .chain(CONDITIONAL_BUILTIN_MCP_SERVERS.iter())
        .copied()
        .collect();
    split.sort_unstable();
    assert_eq!(split, BUILTIN_MANAGED_MCP_SERVERS);
}

#[test]
fn managed_mcp_server_names_defaults_to_builtin() {
    let names = managed_mcp_server_names(&serde_json::json!({}), true, true, true, true);
    assert_eq!(
        names,
        vec![
            "trusty-memory",
            "trusty-mpm",
            "trusty-review",
            "trusty-search"
        ]
    );
}

#[test]
fn managed_mcp_server_names_unions_builtin_with_configured() {
    let config = serde_json::json!({
        "mcpServers": {
            "aaa-custom": { "type": "stdio", "command": "x", "args": [] },
            "trusty-memory": { "type": "stdio", "command": "trusty-memory", "args": [] }
        }
    });
    let names = managed_mcp_server_names(&config, true, true, true, true);
    // Built-in four + the custom server, sorted + deduped (trusty-memory
    // appears once despite being in both the builtin list and the config).
    assert_eq!(
        names,
        vec![
            "aaa-custom",
            "trusty-memory",
            "trusty-mpm",
            "trusty-review",
            "trusty-search"
        ]
    );
}

#[test]
fn managed_mcp_server_names_excludes_disabled_conditional_builtins() {
    // Issue #3934: when a manifest disables the trusty-memory/trusty-search
    // injector for this run, the derived trust list must NOT include the
    // name (unrelated to the config's own `tm mcp add` registry, which is
    // a SEPARATE, always-trusted union source — see the next test for
    // that source's own, unrelated collision behaviour).
    let config = serde_json::json!({
        "mcpServers": {
            "aaa-custom": { "type": "stdio", "command": "x", "args": [] }
        }
    });
    let names = managed_mcp_server_names(&config, true, true, false, false);
    assert!(
        !names.contains(&"trusty-memory".to_string()),
        "trusty-memory must be excluded when its injector did not run: {names:?}"
    );
    assert!(!names.contains(&"trusty-search".to_string()));
    // The unconditional two are present when THEIR pin succeeded.
    assert!(names.contains(&"trusty-mpm".to_string()));
    assert!(names.contains(&"trusty-review".to_string()));
}

#[test]
fn managed_mcp_server_names_excludes_unconditional_builtin_when_pin_failed() {
    // Issue #3950 (fifth instance): the two UNCONDITIONAL builtins
    // (`trusty-mpm`/`trusty-review`) have no manifest toggle, but their
    // force-overwrite WRITE can still fail (disk full, permission error,
    // transient I/O fault). Before this fix `managed_mcp_server_names` had
    // no parameter for this at all — it unconditionally trusted both names
    // regardless of whether prepare_session_inner's injectors actually
    // succeeded this run. A failed pin must drop the name from the trust
    // list exactly like a disabled conditional injector does.
    let config = serde_json::json!({
        "mcpServers": {
            "aaa-custom": { "type": "stdio", "command": "x", "args": [] }
        }
    });
    let names = managed_mcp_server_names(&config, false, false, true, true);
    assert!(
        !names.contains(&"trusty-mpm".to_string()),
        "trusty-mpm must be excluded when its pin write failed this run: {names:?}"
    );
    assert!(
        !names.contains(&"trusty-review".to_string()),
        "trusty-review must be excluded when its pin write failed this run: {names:?}"
    );
    // The conditional two are unaffected by the unconditional pair's result.
    assert!(names.contains(&"trusty-memory".to_string()));
    assert!(names.contains(&"trusty-search".to_string()));
}

#[test]
fn managed_mcp_server_names_registry_collision_does_not_bypass_the_pin_flags() {
    // #4181 REVERSED THIS ASSERTION. It used to read: a registry entry named
    // `trusty-memory` is unioned in regardless of the conditional toggle,
    // because the registry is operator-controlled and therefore trusted.
    //
    // Two things make that wrong now. Narrowly: ADR-0042 seeds all four
    // builtins into the registry, so under the old rule the four `_pinned`
    // flags would become dead parameters and every builtin would be approved
    // on every launch — #3950 with no code change to the derivation.
    // Broadly: it was never sound anyway. This approval is content-blind and
    // resolves against the WORKSPACE `.mcp.json`, and `custom_mcp`'s registry
    // loop skips reserved names, so a registry entry named `trusty-memory` is
    // never what pins the workspace entry. Only `inject_trusty_memory_mcp`
    // does — the very injector the toggle gates.
    let config = serde_json::json!({
        "mcpServers": {
            "trusty-memory": { "type": "stdio", "command": "trusty-memory", "args": [] }
        }
    });
    let names = managed_mcp_server_names(&config, true, true, false, false);
    assert!(
        !names.contains(&"trusty-memory".to_string()),
        "a registry entry under a builtin name must not bypass that builtin's pin flag: {names:?}"
    );
    // A non-builtin registry name is untouched — that union is the point.
    let config = serde_json::json!({
        "mcpServers": {
            "slack-mcp": { "type": "stdio", "command": "slack-mcp", "args": [] }
        }
    });
    assert!(
        managed_mcp_server_names(&config, false, false, false, false)
            .contains(&"slack-mcp".to_string()),
        "an operator's non-builtin registration must still be trusted"
    );
}

#[test]
fn managed_mcp_server_names_partial_conditional_toggle() {
    // Only trusty_search disabled: trusty-memory still trusted, trusty-search not.
    let names = managed_mcp_server_names(&serde_json::json!({}), true, true, true, false);
    assert!(names.contains(&"trusty-memory".to_string()));
    assert!(!names.contains(&"trusty-search".to_string()));
}

#[test]
fn mcp_server_names_reads_workspace_mcp_json() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join(".mcp.json"),
        serde_json::json!({
            "mcpServers": {
                "trusty-mpm": {"type": "stdio", "command": "trusty-mpm", "args": []},
                "tickets-mcp": {"type": "stdio", "command": "tickets-mcp", "args": []},
            }
        })
        .to_string(),
    )
    .unwrap();
    let names = mcp_server_names(tmp.path());
    assert_eq!(names, vec!["tickets-mcp", "trusty-mpm"], "sorted keys");
}

#[test]
fn mcp_server_names_empty_when_absent() {
    let tmp = TempDir::new().unwrap();
    // No .mcp.json written — must not fail, must yield an empty vector.
    assert!(mcp_server_names(tmp.path()).is_empty());
}

#[test]
fn launch_trusted_mcp_names_from_unions_registry() {
    // Why (#3926): the interactive `tm launch` path must trust the SAME
    // provenance-safe set as the daemon-managed path — builtin four union
    // the operator's own `tm mcp add` registry, regardless of what a
    // cloned repo's workspace `.mcp.json` separately declares (this
    // function never reads a workspace's `.mcp.json` keys — the
    // conditional-builtin toggles are supplied explicitly by the caller,
    // issue #3934).
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("claude-config");
    add_server(&cfg, "slack-mcp", stdio("slack-mcp", &["serve"])).unwrap();

    let names = launch_trusted_mcp_names_from(&cfg, true, true, true, true);
    assert_eq!(
        names,
        vec![
            "slack-mcp",
            "trusty-memory",
            "trusty-mpm",
            "trusty-review",
            "trusty-search"
        ]
    );
}

#[test]
fn launch_trusted_mcp_names_from_defaults_to_builtin_when_absent() {
    // Why (#3926): a fresh install with no `tm mcp add` registry yet must
    // still trust exactly the framework builtin four — never error, never
    // an empty list (the four builtins always launch, so they must always
    // be pre-approved).
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("does-not-exist");

    let names = launch_trusted_mcp_names_from(&cfg, true, true, true, true);
    assert_eq!(
        names,
        vec![
            "trusty-memory",
            "trusty-mpm",
            "trusty-review",
            "trusty-search"
        ]
    );
}

#[test]
fn launch_trusted_mcp_names_from_excludes_disabled_conditional_builtins() {
    // Issue #3934 (the actual attack, unit-level): a manifest disabling
    // the trusty-memory injector for this run must drop the name from
    // the trust list even though the operator's own registry (or, in the
    // real attack, a spoofed workspace `.mcp.json` entry the caller does
    // not even consult here) might otherwise suggest it belongs.
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("claude-config");

    let names = launch_trusted_mcp_names_from(&cfg, true, true, false, false);
    assert!(!names.contains(&"trusty-memory".to_string()));
    assert!(!names.contains(&"trusty-search".to_string()));
    assert!(names.contains(&"trusty-mpm".to_string()));
    assert!(names.contains(&"trusty-review".to_string()));
}

#[test]
fn launch_trusted_mcp_names_from_excludes_unconditional_builtin_when_pin_failed() {
    // Issue #3950 (fifth instance): mirrors
    // `managed_mcp_server_names_excludes_unconditional_builtin_when_pin_failed`
    // one layer up — a failed `trusty-mpm`/`trusty-review` pin write must
    // drop the name from the interactive-path trust derivation too.
    let tmp = TempDir::new().unwrap();
    let cfg = tmp.path().join("claude-config");

    let names = launch_trusted_mcp_names_from(&cfg, false, false, true, true);
    assert!(!names.contains(&"trusty-mpm".to_string()));
    assert!(!names.contains(&"trusty-review".to_string()));
    assert!(names.contains(&"trusty-memory".to_string()));
    assert!(names.contains(&"trusty-search".to_string()));
}

#[test]
fn resolve_conditional_mcp_toggles_defaults_to_both_on() {
    // No manifest at all (compiled-in default): both conditional builtins
    // are enabled, matching today's zero-regression behaviour.
    let tmp_home = TempDir::new().unwrap();
    let tmp_project = TempDir::new().unwrap();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    let (memory, search) = resolve_conditional_mcp_toggles(&fw, tmp_project.path());
    assert!(memory);
    assert!(search);
}

#[test]
fn resolve_conditional_mcp_toggles_honors_project_manifest_toggle() {
    // Issue #3934: a project-scope manifest.toml (the attack's vector —
    // git-tracked, arrives WITH a cloned repo) disabling trusty_memory
    // must resolve to `inject_trusty_memory == false`, and must NOT affect
    // the untouched trusty_search toggle.
    let tmp_home = TempDir::new().unwrap();
    let tmp_project = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp_project.path().join(".trusty-mpm").join("framework")).unwrap();
    std::fs::write(
        tmp_project
            .path()
            .join(".trusty-mpm")
            .join("framework")
            .join("manifest.toml"),
        "[mcp]\ntrusty_memory = false\n",
    )
    .unwrap();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    let (memory, search) = resolve_conditional_mcp_toggles(&fw, tmp_project.path());
    assert!(!memory, "project manifest toggle must be honored");
    assert!(search, "untouched toggle must keep its default");
}

// ─── #4181 / ADR-0042: seed_builtin_servers ───────────────────────────────
//
// Seeding declares the framework builtins ONCE in the user-scope `mcpServers`
// map, insert-if-absent, on a path that runs before every launch. The contract
// these tests pin: it inserts what is missing, it never touches what is already
// there, and it never destroys the file it reads — which also holds OAuth state.

/// The four builtin names, sorted, as `seed_builtin_servers` reports them.
fn all_builtins() -> Vec<String> {
    let mut names: Vec<String> = BUILTIN_MANAGED_MCP_SERVERS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    names.sort();
    names
}

fn read_servers(config_dir: &std::path::Path) -> Map<String, Value> {
    let text = std::fs::read_to_string(config_dir.join(".claude.json")).unwrap();
    let val: Value = serde_json::from_str(&text).unwrap();
    val["mcpServers"].as_object().cloned().unwrap_or_default()
}

#[test]
fn seed_builtin_servers_inserts_every_builtin_when_absent() {
    let tmp = TempDir::new().unwrap();
    let seeded = seed_builtin_servers(tmp.path()).unwrap();

    assert_eq!(seeded, all_builtins(), "every builtin must be reported");
    let servers = read_servers(tmp.path());
    for name in BUILTIN_MANAGED_MCP_SERVERS {
        assert_eq!(
            servers.get(*name),
            builtin_server_entry(name).as_ref(),
            "{name} must be written as its canonical builtin entry"
        );
    }
}

#[test]
fn seed_builtin_servers_never_overwrites_an_existing_entry() {
    let tmp = TempDir::new().unwrap();
    // The operator's own declaration, under a builtin name, with a different
    // binary and a pinned index — exactly what `tm mcp add` writes.
    let operator = stdio("/opt/operator/trusty-search", &["serve", "--index", "cto"]);
    add_server(tmp.path(), "trusty-search", operator.clone()).unwrap();

    let seeded = seed_builtin_servers(tmp.path()).unwrap();

    assert_eq!(
        seeded,
        strings(&["trusty-memory", "trusty-mpm", "trusty-review"]),
        "the occupied name must not be reported as seeded"
    );
    assert_eq!(
        read_servers(tmp.path()).get("trusty-search"),
        Some(&operator),
        "the operator's entry must survive verbatim — tm mcp add wins"
    );
}

#[test]
fn seed_builtin_servers_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let claude_json = tmp.path().join(".claude.json");

    assert_eq!(seed_builtin_servers(tmp.path()).unwrap(), all_builtins());
    let first = std::fs::read(&claude_json).unwrap();

    let second_run = seed_builtin_servers(tmp.path()).unwrap();
    assert!(
        second_run.is_empty(),
        "a second run must insert nothing, got {second_run:?}"
    );
    assert_eq!(
        first,
        std::fs::read(&claude_json).unwrap(),
        "a no-op run must not rewrite the file"
    );
}

#[test]
fn seed_builtin_servers_preserves_unrelated_keys() {
    let tmp = TempDir::new().unwrap();
    // `.claude.json` also holds OAuth state and every project's trust — seeding
    // shares the file and must leave the rest of it alone.
    std::fs::write(
        tmp.path().join(".claude.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "oauthAccount": { "emailAddress": "op@example.com" },
            "projects": { "/w": { "hasTrustDialogAccepted": true } }
        }))
        .unwrap(),
    )
    .unwrap();

    seed_builtin_servers(tmp.path()).unwrap();

    let text = std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap();
    let val: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        val["oauthAccount"]["emailAddress"].as_str(),
        Some("op@example.com"),
        "OAuth state must survive seeding"
    );
    assert_eq!(val["projects"]["/w"]["hasTrustDialogAccepted"], true);
    assert!(val["mcpServers"]["trusty-mpm"].is_object());
}

#[test]
fn seed_builtin_servers_declines_a_malformed_config_without_quarantining() {
    let tmp = TempDir::new().unwrap();
    let claude_json = tmp.path().join(".claude.json");
    let malformed = br#"{"oauthAccount": {"emailAddress": "op@example.com"},"#;
    std::fs::write(&claude_json, malformed).unwrap();

    let seeded = seed_builtin_servers(tmp.path()).unwrap();

    assert!(seeded.is_empty(), "a malformed config must seed nothing");
    assert_eq!(
        std::fs::read(&claude_json).unwrap(),
        malformed,
        "the file must be byte-identical — no rename, no write"
    );
    // `read_config` (the `tm mcp` path) renames to a timestamped `.corrupt`
    // sibling. Seeding runs unattended on every launch, so it must not.
    let strays: Vec<String> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != ".claude.json")
        .collect();
    assert!(
        strays.is_empty(),
        "seeding must leave no siblings behind: {strays:?}"
    );
}

#[test]
fn seed_builtin_servers_declines_a_non_object_config() {
    let tmp = TempDir::new().unwrap();
    let claude_json = tmp.path().join(".claude.json");
    std::fs::write(&claude_json, b"[1, 2, 3]").unwrap();

    assert!(seed_builtin_servers(tmp.path()).unwrap().is_empty());
    assert_eq!(std::fs::read(&claude_json).unwrap(), b"[1, 2, 3]");
}

#[test]
fn seed_builtin_servers_declines_a_non_object_mcp_servers_map() {
    let tmp = TempDir::new().unwrap();
    let claude_json = tmp.path().join(".claude.json");
    // `mcp_servers_mut` (the `tm mcp` path) replaces this with `{}`, discarding
    // it. Seeding must decline instead — it is not entitled to destroy operator
    // state it merely failed to understand.
    let hand_edited = br#"{"mcpServers": ["trusty-mpm"]}"#;
    std::fs::write(&claude_json, hand_edited).unwrap();

    assert!(seed_builtin_servers(tmp.path()).unwrap().is_empty());
    assert_eq!(
        std::fs::read(&claude_json).unwrap(),
        hand_edited,
        "a non-object mcpServers must be left exactly as found"
    );
}

#[test]
fn seed_builtin_servers_creates_the_config_when_the_dir_is_absent() {
    let tmp = TempDir::new().unwrap();
    let config_dir = tmp.path().join("never").join("created");

    assert_eq!(seed_builtin_servers(&config_dir).unwrap(), all_builtins());
    assert!(config_dir.join(".claude.json").is_file());
}

#[test]
fn seed_builtin_servers_errors_when_claude_json_is_unreadable() {
    let tmp = TempDir::new().unwrap();
    // A directory where the file belongs: a read error that is neither
    // NotFound nor uid-dependent, so it reproduces for root too.
    std::fs::create_dir_all(tmp.path().join(".claude.json")).unwrap();

    let err = seed_builtin_servers(tmp.path()).unwrap_err().to_string();
    assert!(
        err.contains("failed to read"),
        "the read failure must be reported, got: {err}"
    );
    assert!(
        tmp.path().join(".claude.json").is_dir(),
        "an unreadable path must be left alone"
    );
}

#[test]
fn a_seeded_builtin_is_not_approved_when_its_pin_failed() {
    // #4181 × #3950. Seeding writes the builtins into the SAME `mcpServers`
    // map `managed_mcp_server_names` unions for `enabledMcpjsonServers`. Union
    // it wholesale and every builtin is approved regardless of whether its
    // workspace `.mcp.json` force-overwrite succeeded — approving a name whose
    // project-scope content was never verified, which is the whole exploit.
    let tmp = TempDir::new().unwrap();
    seed_builtin_servers(tmp.path()).unwrap();

    let approved = launch_trusted_mcp_names_from(tmp.path(), false, false, false, false);
    assert!(
        approved.is_empty(),
        "no builtin may be approved when every pin failed, got {approved:?}"
    );

    // An operator's own non-builtin registration is unaffected — it still
    // enters via the registry, which is the union's actual purpose.
    add_server(tmp.path(), "slack-mcp", stdio("slack-mcp", &["serve"])).unwrap();
    assert_eq!(
        launch_trusted_mcp_names_from(tmp.path(), false, false, false, false),
        strings(&["slack-mcp"])
    );
    // And a successful pin still approves its builtin.
    assert_eq!(
        launch_trusted_mcp_names_from(tmp.path(), true, false, false, false),
        strings(&["slack-mcp", "trusty-mpm"])
    );
}

#[test]
fn seeding_and_workspace_injection_coexist_without_clobbering() {
    // Until PR C2 deletes the injectors, both provision the builtins: seeding
    // into user scope, `inject_trusty_mpm_mcp` into the workspace `.mcp.json`.
    // They target different files, so double-provisioning can produce neither a
    // duplicate nor a clobber — this pins that.
    let tmp = TempDir::new().unwrap();
    let config_dir = tmp.path().join("claude-config");
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();

    seed_builtin_servers(&config_dir).unwrap();
    crate::core::session_launch::inject_trusty_mpm_mcp(&workspace).unwrap();
    let after_inject = seed_builtin_servers(&config_dir).unwrap();
    assert!(
        after_inject.is_empty(),
        "the injector must not have disturbed user scope"
    );

    let user_scope = read_servers(&config_dir);
    assert_eq!(user_scope.len(), BUILTIN_MANAGED_MCP_SERVERS.len());
    assert_eq!(
        user_scope.get("trusty-mpm"),
        builtin_server_entry("trusty-mpm").as_ref(),
        "the user-scope entry must be the canonical builtin, unduplicated"
    );

    // The workspace copy is the injector's and stays intact.
    assert_eq!(
        mcp_server_names(&workspace),
        strings(&["trusty-mpm"]),
        "the workspace .mcp.json must still declare exactly what was injected"
    );
}
