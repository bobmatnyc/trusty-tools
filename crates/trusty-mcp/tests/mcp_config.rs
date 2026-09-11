//! Integration coverage for the `config` feature (#7452).
//!
//! Why: `config` is the shape trusty-code, trusty-agents and (via the
//! Claude-Code adapter) trusty-mpm are about to depend on. These tests
//! exercise it through the crate's PUBLIC surface, the way those consumers
//! will, so a change that keeps the internals compiling but breaks a consumer
//! still fails here.
//!
//! What: the TOML file's round trip and its three fail-closed rejections, the
//! resolver's precedence and ordering, and the Claude-Code adapter against
//! goldens copied from `trusty_mpm::core::mcp_config`'s own tests.
//!
//! Test: this file is the coverage; a feature-off run compiles it to nothing.

#![cfg(feature = "config")]

use std::collections::BTreeMap;

use serde_json::json;
use tempfile::TempDir;
use trusty_mcp::config::{
    McpConfigError, McpConfigFile, McpServerConfig, McpServerOverride, McpTransport,
    claude_code::{read_mcp_servers, write_mcp_servers},
    file::default_path_at,
    resolve,
};

/// A stdio server with no env.
fn stdio(name: &str, command: &str, args: &[&str]) -> McpServerConfig {
    McpServerConfig::new(
        name,
        McpTransport::Stdio {
            command: command.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
            env: BTreeMap::new(),
        },
    )
}

/// An http server with no headers.
fn http(name: &str, url: &str) -> McpServerConfig {
    McpServerConfig::new(
        name,
        McpTransport::Http {
            url: url.to_string(),
            headers: BTreeMap::new(),
        },
    )
}

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// ---------------------------------------------------------------- shape ----

#[test]
fn new_defaults_to_enabled() {
    let server = stdio("echo", "echo", &["hi"]);
    assert!(
        server.enabled,
        "a new server is usable without being told to be"
    );
    assert!(server.extensions.is_empty());
}

#[test]
fn omitted_enabled_defaults_to_true() {
    // `bool::default()` is false, so an absent key must not fall through to it.
    let text = r#"
        [[servers]]
        name = "echo"
        [servers.transport]
        type = "stdio"
        command = "echo"
    "#;
    let parsed: McpConfigFile = toml::from_str(text).expect("parses");
    assert_eq!(parsed.servers.len(), 1);
    assert!(parsed.servers[0].enabled);
}

#[test]
fn debug_redacts_env_and_header_values() {
    let mut server = stdio("srv", "srv", &[]);
    server.transport = McpTransport::Stdio {
        command: "srv".into(),
        args: Vec::new(),
        env: map(&[("API_KEY", "secret-value")]),
    };
    let rendered = format!("{server:?}");
    assert!(
        !rendered.contains("secret-value"),
        "a derived Debug would put the key straight into every log line: {rendered}"
    );
    assert!(
        rendered.contains("API_KEY") && rendered.contains("<redacted>"),
        "the key name is the diagnostic value and must survive: {rendered}"
    );
    // `command` is not a credential and stays legible.
    assert!(rendered.contains("srv"), "{rendered}");

    let remote = McpServerConfig::new(
        "remote",
        McpTransport::Http {
            url: "https://x/mcp".into(),
            headers: map(&[("Authorization", "Bearer t")]),
        },
    );
    let rendered = format!("{remote:?}");
    assert!(!rendered.contains("Bearer t"), "{rendered}");
    assert!(rendered.contains("Authorization"), "{rendered}");
    assert!(rendered.contains("https://x/mcp"), "{rendered}");

    // Sse shares the redaction, not just http.
    let events = McpServerConfig::new(
        "events",
        McpTransport::Sse {
            url: "https://x/sse".into(),
            headers: map(&[("Authorization", "Bearer t")]),
        },
    );
    let rendered = format!("{events:?}");
    assert!(!rendered.contains("Bearer t"), "{rendered}");
    assert!(rendered.contains("Authorization"), "{rendered}");
}

// ----------------------------------------------------------------- file ----

#[test]
fn default_path_at_layout() {
    let got = default_path_at(std::path::Path::new("/home/ada"));
    assert_eq!(
        got,
        std::path::PathBuf::from("/home/ada/.trusty-tools/mcp/servers.toml")
    );
}

#[test]
fn mirrored_trusty_tools_dir_matches_trusty_common() {
    // `config::file` mirrors the constant instead of importing it, so that the
    // `config` feature does not pull `trusty-common` into the lean rlib
    // ADR-0040 protects. Nothing but this assertion keeps the copy honest: a
    // rename on either side would otherwise leave the MCP config file at a
    // path no other trusty-* crate looks in.
    assert_eq!(
        trusty_mcp::config::file::TRUSTY_TOOLS_DIR,
        trusty_common::crate_config::TRUSTY_TOOLS_DIR,
        "the mirrored copy drifted from the workspace-wide constant"
    );
}

#[test]
fn toml_round_trip_preserves_extensions() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("nested").join("servers.toml");

    let mut agents = stdio("agents", "tagent", &["serve", "--mcp"]);
    agents.transport = McpTransport::Stdio {
        command: "tagent".into(),
        args: vec!["serve".into(), "--mcp".into()],
        env: map(&[("RUST_LOG", "info")]),
    };
    // The consumer-specific keys this crate never interprets.
    agents
        .extensions
        .insert("scopes".into(), json!(["memory:read", "search:read"]));
    agents
        .extensions
        .insert("discovery_ttl_secs".into(), json!(900));

    let mut remote = http("gworkspace", "https://example.test/mcp");
    remote.transport = McpTransport::Http {
        url: "https://example.test/mcp".into(),
        headers: map(&[("Authorization", "Bearer t")]),
    };
    remote.enabled = false;

    // The third transport has to survive the file too, or g2 cannot store what
    // `tm mcp add --transport sse` accepts.
    let mut events = McpServerConfig::new(
        "events",
        McpTransport::Sse {
            url: "https://example.test/sse".into(),
            headers: map(&[("Authorization", "Bearer s")]),
        },
    );
    events
        .extensions
        .insert("scopes".into(), json!(["events:read"]));

    let original = McpConfigFile::new(vec![agents, remote, events]);
    original.save(&path).expect("save");

    let loaded = McpConfigFile::load(&path).expect("load");
    assert_eq!(loaded, original, "save -> load is lossless");

    // And the on-disk form really is TOML naming the servers.
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[[servers]]"), "TOML array of tables: {text}");
    assert!(
        text.contains("discovery_ttl_secs"),
        "extensions survive: {text}"
    );
    assert!(
        text.contains("type = \"sse\""),
        "the sse discriminant is what distinguishes it from http: {text}"
    );
}

#[cfg(unix)]
#[test]
fn saved_config_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("servers.toml");

    // A pre-existing world-readable file at the temp path must be tightened,
    // not inherited — `.mode()` alone applies only when the open creates it.
    let stale = tmp
        .path()
        .join(format!(".servers.toml.tmp.{}", std::process::id()));
    std::fs::write(&stale, "stale").unwrap();
    std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o644)).unwrap();

    let mut secret = stdio("srv", "srv", &[]);
    secret.transport = McpTransport::Stdio {
        command: "srv".into(),
        args: Vec::new(),
        env: map(&[("API_KEY", "secret-value")]),
    };
    McpConfigFile::new(vec![secret]).save(&path).unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "env and headers hold credentials, so the file must not be group- or world-readable"
    );
}

#[test]
fn save_round_trips_config_with_null_extension_value() {
    // trusty-agents' auth block goes in `extensions` under #7454, and a struct
    // with an `Option::None` field serialises to a JSON null. TOML has no
    // null, so `toml::to_string_pretty` used to fail the WHOLE file with the
    // bare string "unsupported unit type", naming neither server nor key.
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("servers.toml");

    let mut server = stdio("agents", "tagent", &["serve"]);
    // A top-level null, the shape a whole absent optional block takes.
    server
        .extensions
        .insert("auth".into(), serde_json::Value::Null);
    // A null nested one level down, beside a sibling that must survive.
    server.extensions.insert(
        "discovery".into(),
        json!({"ttl_secs": 900, "last_seen": null}),
    );
    // And one two levels down, inside an object inside an array.
    server.extensions.insert(
        "scopes".into(),
        json!([{"name": "memory:read", "expires_at": null}]),
    );

    let original = McpConfigFile::new(vec![server]);
    original
        .save(&path)
        .expect("a null extension must not fail the save");

    let loaded = McpConfigFile::load(&path).expect("load");
    let ext = &loaded.servers[0].extensions;
    assert!(
        !ext.contains_key("auth"),
        "a wholly null extension is dropped, not rendered: {ext:?}"
    );
    assert_eq!(
        ext["discovery"],
        json!({"ttl_secs": 900}),
        "the null member goes, its sibling stays"
    );
    assert_eq!(
        ext["scopes"],
        json!([{"name": "memory:read"}]),
        "nulls are stripped at any depth"
    );
}

#[test]
fn save_rejects_null_inside_an_extension_array() {
    // Dropping an array element renumbers the rest, so this one fails closed
    // instead — and says which server and which key, which the `toml` error
    // never did.
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("servers.toml");

    let mut server = stdio("agents", "tagent", &[]);
    server
        .extensions
        .insert("scopes".into(), json!(["memory:read", null]));

    let err = McpConfigFile::new(vec![server])
        .save(&path)
        .expect_err("a null array element is not representable");
    match &err {
        McpConfigError::NullExtensionValue { server, key, .. } => {
            assert_eq!(server, "agents");
            assert_eq!(key, "scopes[1]");
        }
        other => panic!("expected NullExtensionValue, got {other:?}"),
    }
    assert!(
        err.to_string().contains("scopes[1]"),
        "the operator is told which key to fix: {err}"
    );
    assert!(!path.exists(), "a refused save writes nothing");
}

#[test]
fn save_creates_parent_directory() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("a").join("b").join("servers.toml");
    McpConfigFile::new(vec![stdio("echo", "echo", &[])])
        .save(&path)
        .expect("save creates a/b");
    assert!(path.is_file());
}

#[test]
fn save_replaces_existing_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("servers.toml");
    McpConfigFile::new(vec![stdio("one", "one", &[])])
        .save(&path)
        .unwrap();
    McpConfigFile::new(vec![stdio("two", "two", &[])])
        .save(&path)
        .unwrap();

    let loaded = McpConfigFile::load(&path).unwrap();
    assert_eq!(loaded.servers.len(), 1);
    assert_eq!(loaded.servers[0].name, "two");
    // The write-then-rename leaves no temporary file behind.
    let strays: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != "servers.toml")
        .collect();
    assert!(strays.is_empty(), "temp files left behind: {strays:?}");
}

#[test]
fn empty_file_loads_as_no_servers() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("servers.toml");
    std::fs::write(&path, "").unwrap();
    assert!(McpConfigFile::load(&path).unwrap().servers.is_empty());
}

#[test]
fn load_rejects_malformed_toml() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("servers.toml");
    std::fs::write(&path, "[[servers]\nname = \"broken\"\n").unwrap();

    let err = McpConfigFile::load(&path).expect_err("malformed TOML must not load");
    assert!(
        matches!(&err, McpConfigError::Parse { path: p, .. } if p == &path),
        "expected a Parse error naming the path, got: {err}"
    );
    // Fail closed: the operator is told which file, not handed an empty config.
    assert!(
        err.to_string().contains(&path.display().to_string()),
        "the message names the file: {err}"
    );
}

#[test]
fn load_rejects_duplicate_names() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("servers.toml");
    let text = r#"
        [[servers]]
        name = "echo"
        [servers.transport]
        type = "stdio"
        command = "echo"

        [[servers]]
        name = "echo"
        [servers.transport]
        type = "http"
        url = "https://example.test/mcp"
    "#;
    std::fs::write(&path, text).unwrap();

    let err = McpConfigFile::load(&path).expect_err("a duplicate name is ambiguous for resolve");
    assert!(
        matches!(&err, McpConfigError::DuplicateName { name, .. } if name == "echo"),
        "expected DuplicateName(\"echo\"), got: {err}"
    );
}

#[test]
fn save_rejects_duplicate_names() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("servers.toml");
    let dup = McpConfigFile::new(vec![stdio("echo", "a", &[]), stdio("echo", "b", &[])]);

    assert!(matches!(
        dup.save(&path),
        Err(McpConfigError::DuplicateName { .. })
    ));
    assert!(!path.exists(), "a rejected save writes nothing");
}

#[test]
fn load_missing_file_is_io_error() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("absent.toml");
    assert!(matches!(
        McpConfigFile::load(&path),
        Err(McpConfigError::Io { .. })
    ));
}

#[test]
fn load_or_default_on_missing_is_empty() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("absent.toml");
    assert!(
        McpConfigFile::load_or_default(&path)
            .unwrap()
            .servers
            .is_empty()
    );
}

#[test]
fn load_or_default_still_rejects_malformed() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("servers.toml");
    std::fs::write(&path, "not = = toml").unwrap();
    // Absence and corruption are different answers.
    assert!(matches!(
        McpConfigFile::load_or_default(&path),
        Err(McpConfigError::Parse { .. })
    ));
}

// -------------------------------------------------------------- resolve ----

#[test]
fn resolve_set_replaces_wholesale() {
    let global = vec![{
        let mut s = stdio("memory", "trusty-mcp", &["memory"]);
        s.transport = McpTransport::Stdio {
            command: "trusty-mcp".into(),
            args: vec!["memory".into()],
            env: map(&[("RUST_LOG", "warn"), ("KEEP_ME", "no")]),
        };
        s
    }];
    let replacement = http("memory", "https://memory.test/mcp");
    let got = resolve(&global, &[McpServerOverride::Set(replacement.clone())]);

    assert_eq!(got, vec![replacement]);
    // Wholesale means the global `env` is gone, not merged in.
    assert!(matches!(&got[0].transport, McpTransport::Http { .. }));
}

#[test]
fn resolve_disable_removes_global_entry() {
    let global = vec![stdio("a", "a", &[]), stdio("b", "b", &[])];
    let got = resolve(&global, &[McpServerOverride::Disable { name: "a".into() }]);
    assert_eq!(
        got.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["b"]
    );
}

#[test]
fn resolve_set_reenables_disabled_global() {
    let mut off = stdio("memory", "trusty-mcp", &["memory"]);
    off.enabled = false;
    let global = vec![off];

    let on = stdio("memory", "trusty-mcp", &["memory"]);
    let got = resolve(&global, &[McpServerOverride::Set(on)]);

    assert_eq!(got.len(), 1);
    assert!(got[0].enabled, "the override's enabled flag wins outright");
}

#[test]
fn resolve_set_with_enabled_false_stays_disabled_in_output() {
    // The mirror of `resolve_set_reenables_disabled_global`, and the reason
    // `Disable` is a separate variant rather than a synonym for this: a `Set`
    // carrying `enabled = false` STAYS in the list, disabled, so a consumer
    // can still see the entry it is choosing not to connect to. `Disable`
    // removes it outright. Nothing else pinned that difference.
    let global = vec![
        stdio("memory", "trusty-mcp", &["memory"]),
        stdio("b", "b", &[]),
    ];

    let mut off = stdio("memory", "trusty-mcp", &["memory"]);
    off.enabled = false;
    let got = resolve(&global, &[McpServerOverride::Set(off)]);

    assert_eq!(
        got.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["memory", "b"],
        "a disabled Set keeps the global entry's position, unlike Disable"
    );
    assert!(
        !got[0].enabled,
        "the override's enabled flag wins in this direction too"
    );
}

#[test]
fn resolve_orders_global_first_then_new_additions() {
    let global = vec![stdio("a", "a", &[]), stdio("b", "b", &[])];
    let overrides = vec![
        McpServerOverride::Set(stdio("z", "z", &[])),
        McpServerOverride::Set(stdio("b", "b2", &[])),
        McpServerOverride::Set(stdio("m", "m", &[])),
    ];
    let got = resolve(&global, &overrides);

    // Global order first — overriding `b` does not move it — then the two new
    // names in the order the overrides listed them, NOT alphabetically.
    assert_eq!(
        got.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["a", "b", "z", "m"]
    );
    assert!(
        matches!(&got[1].transport, McpTransport::Stdio { command, .. } if command == "b2"),
        "the overridden entry kept its position and took the new config"
    );
}

#[test]
fn resolve_later_override_wins_over_earlier() {
    let global = vec![stdio("a", "a", &[])];
    let overrides = vec![
        McpServerOverride::Disable { name: "a".into() },
        McpServerOverride::Set(stdio("a", "revived", &[])),
    ];
    let got = resolve(&global, &overrides);
    assert_eq!(got.len(), 1);
    assert!(
        matches!(&got[0].transport, McpTransport::Stdio { command, .. } if command == "revived")
    );

    // And the mirror: a Disable after a Set wins too, including for an
    // addition that was never in the global list.
    let reversed = vec![
        McpServerOverride::Set(stdio("new", "new", &[])),
        McpServerOverride::Disable { name: "new".into() },
    ];
    assert!(resolve(&global, &reversed).iter().all(|s| s.name != "new"));
}

#[test]
fn resolve_drops_duplicate_global_names() {
    // McpConfigFile rejects this on disk; in memory the resolver still has to
    // produce one entry per name so overrides stay unambiguous.
    let global = vec![stdio("a", "first", &[]), stdio("a", "second", &[])];
    let got = resolve(&global, &[]);
    assert_eq!(got.len(), 1);
    assert!(matches!(&got[0].transport, McpTransport::Stdio { command, .. } if command == "first"));
}

#[test]
fn resolve_without_overrides_is_the_global_list() {
    let global = vec![stdio("a", "a", &[]), http("b", "https://b.test/mcp")];
    assert_eq!(resolve(&global, &[]), global);
}

// ---------------------------------------------------------- claude code ----

#[test]
fn claude_code_stdio_entry_matches_trusty_mpm_golden() {
    // Golden copied from `crates/trusty-mpm/src/core/mcp_config_tests.rs`
    // (`build_stdio_entry_has_expected_shape`, `build_stdio_entry_with_env`):
    // `type`/`command`/`args` always present, `env` only when non-empty.
    let plain = write_mcp_servers(&[stdio("echo", "echo", &["hi"])]);
    assert_eq!(
        plain,
        json!({ "echo": { "type": "stdio", "command": "echo", "args": ["hi"] } })
    );

    let mut with_env = stdio("srv", "srv", &[]);
    with_env.transport = McpTransport::Stdio {
        command: "srv".into(),
        args: Vec::new(),
        env: map(&[("API_KEY", "xxx")]),
    };
    assert_eq!(
        write_mcp_servers(&[with_env]),
        json!({
            "srv": { "type": "stdio", "command": "srv", "args": [], "env": { "API_KEY": "xxx" } }
        }),
        "args is present even when empty, matching build_stdio_entry"
    );
}

#[test]
fn claude_code_remote_entry_matches_trusty_mpm_golden() {
    // Golden from `build_remote_entry_http` / `build_remote_entry_with_headers`.
    let plain = write_mcp_servers(&[http("remote", "https://x/mcp")]);
    assert_eq!(
        plain,
        json!({ "remote": { "type": "http", "url": "https://x/mcp" } })
    );

    let mut with_headers = http("remote", "https://x");
    with_headers.transport = McpTransport::Http {
        url: "https://x".into(),
        headers: map(&[("Authorization", "Bearer t")]),
    };
    assert_eq!(
        write_mcp_servers(&[with_headers]),
        json!({
            "remote": {
                "type": "http",
                "url": "https://x",
                "headers": { "Authorization": "Bearer t" }
            }
        })
    );
}

#[test]
fn claude_code_sse_entry_matches_trusty_mpm_golden() {
    // Golden from `build_remote_entry_http`'s tail, which asserts sse shares
    // the remote shape with a different discriminant:
    //   let s = build_remote_entry(McpTransport::Sse, "https://x/sse", &Map::new());
    //   assert_eq!(s["type"], "sse");
    let plain = write_mcp_servers(&[McpServerConfig::new(
        "events",
        McpTransport::Sse {
            url: "https://x/sse".into(),
            headers: BTreeMap::new(),
        },
    )]);
    assert_eq!(
        plain,
        json!({ "events": { "type": "sse", "url": "https://x/sse" } })
    );

    let with_headers = write_mcp_servers(&[McpServerConfig::new(
        "events",
        McpTransport::Sse {
            url: "https://x/sse".into(),
            headers: map(&[("Authorization", "Bearer t")]),
        },
    )]);
    assert_eq!(
        with_headers,
        json!({
            "events": {
                "type": "sse",
                "url": "https://x/sse",
                "headers": { "Authorization": "Bearer t" }
            }
        }),
        "headers are added only when non-empty, exactly as for http"
    );
}

#[test]
fn claude_code_write_omits_disabled_servers() {
    let mut off = stdio("off", "off", &[]);
    off.enabled = false;
    let written = write_mcp_servers(&[off, stdio("on", "on", &[])]);
    let obj = written.as_object().unwrap();
    assert!(obj.contains_key("on"));
    assert!(
        !obj.contains_key("off"),
        "Claude Code has no enabled key, so a disabled server must not be registered"
    );
}

#[test]
fn claude_code_write_then_read_round_trips() {
    let mut stdio_server = stdio("alpha", "tagent", &["serve"]);
    stdio_server.transport = McpTransport::Stdio {
        command: "tagent".into(),
        args: vec!["serve".into()],
        env: map(&[("RUST_LOG", "info")]),
    };
    let mut remote = http("beta", "https://beta.test/mcp");
    remote.transport = McpTransport::Http {
        url: "https://beta.test/mcp".into(),
        headers: map(&[("X-Key", "v")]),
    };

    let events = McpServerConfig::new(
        "gamma",
        McpTransport::Sse {
            url: "https://gamma.test/sse".into(),
            headers: map(&[("X-Key", "v")]),
        },
    );

    let servers = vec![stdio_server, remote, events];
    let back = read_mcp_servers(&write_mcp_servers(&servers)).expect("round trip");
    // Read output is name-ordered; the input already is.
    assert_eq!(back, servers);
}

#[test]
fn read_infers_transport_without_type() {
    let value = json!({
        "a": { "command": "a", "args": ["x"] },
        "b": { "url": "https://b.test/mcp" },
    });
    let got = read_mcp_servers(&value).expect("type is inferable");
    assert_eq!(got.len(), 2);
    assert!(matches!(&got[0].transport, McpTransport::Stdio { command, .. } if command == "a"));
    assert!(
        matches!(&got[1].transport, McpTransport::Http { url, .. } if url == "https://b.test/mcp")
    );
}

#[test]
fn read_rejects_unknown_transport() {
    let err = read_mcp_servers(&json!({ "x": { "type": "carrier-pigeon", "url": "u" } }))
        .expect_err("an unknown transport must not be guessed at");
    assert!(
        matches!(&err, McpConfigError::ClaudeCodeEntry { name, .. } if name == "x"),
        "got: {err}"
    );
}

#[test]
fn read_accepts_sse_transport() {
    let got = read_mcp_servers(&json!({
        "s": { "type": "sse", "url": "https://x/sse", "headers": { "X-Key": "v" } }
    }))
    .expect("sse is one of the three transports");
    assert_eq!(got.len(), 1);
    // Sse, not Http — the discriminant decides which protocol a client speaks.
    assert!(
        matches!(
            &got[0].transport,
            McpTransport::Sse { url, headers }
                if url == "https://x/sse" && headers["X-Key"] == "v"
        ),
        "got: {:?}",
        got[0].transport
    );
}

#[test]
fn read_rejects_non_object_entry() {
    assert!(matches!(
        read_mcp_servers(&json!({ "x": "echo" })),
        Err(McpConfigError::ClaudeCodeEntry { .. })
    ));
    assert!(matches!(
        read_mcp_servers(&json!([])),
        Err(McpConfigError::ClaudeCodeShape { .. })
    ));
    // A stdio entry with no command has nothing to spawn.
    assert!(matches!(
        read_mcp_servers(&json!({ "x": { "type": "stdio" } })),
        Err(McpConfigError::ClaudeCodeEntry { .. })
    ));
}

#[test]
fn read_rejects_non_string_env_value() {
    // Stringifying 8080 here would differ from what the operator wrote.
    let err = read_mcp_servers(&json!({
        "x": { "type": "stdio", "command": "c", "env": { "PORT": 8080 } }
    }))
    .expect_err("env values are strings");
    assert!(err.to_string().contains("PORT"), "got: {err}");
}
